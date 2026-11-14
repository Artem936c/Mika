//! Metering of paid calls (decision 0028): what an answer says about its
//! tokens and its cost, the length of a sound sent for recognition, and the
//! provider's price list for the tracker in `mika_core::costs`.
//!
//! Nothing here keeps or logs the text of a request or an answer.

use std::time::Duration;

use mika_core::config::{
    ProviderKind, TtsEngine, EMBEDDING_SECRET_KEY, IMAGE_SECRET_KEY, SPEECH_SECRET_KEY,
};
use mika_core::costs::{provider_host, CostKind, Costs, PaidCall, Tokens, UnitPrices};
use mika_core::llm::LlmRequest;
use mika_core::secrets::Secret;
use serde_json::Value;

use crate::live::Live;

/// How long after a speech answer its cost is asked for: the provider needs a
/// moment to count it.
const GENERATION_DELAY: Duration = Duration::from_secs(3);
/// Longest wait for the answer of `/generation`.
const GENERATION_TIMEOUT: Duration = Duration::from_secs(15);

/// A request with pictures is counted as vision, any other as chat.
pub fn request_kind(req: &LlmRequest) -> CostKind {
    if req.messages.iter().any(|m| !m.images.is_empty()) {
        CostKind::Vision
    } else {
        CostKind::Chat
    }
}

/// A count that came as a JSON number.
fn count(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        v.as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .map(|f| f as u64)
    })
}

/// Money that came as a number or a numeric string; only finite and not negative.
fn money(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|c| c.is_finite() && *c >= 0.0)
}

/// What the provider says the request cost: `usage.cost` of RouterAI and OpenRouter.
pub fn usage_cost(usage: &Value) -> Option<f64> {
    money(&usage["cost"])
}

/// Tokens and cost of an OpenAI-style `usage` object. Chat names the tokens
/// `prompt_tokens` and `completion_tokens`; `/audio/transcriptions` and
/// `/images/generations` name them `input_tokens` and `output_tokens`.
/// `None` when the input tokens are not there.
pub fn parse_openai_usage(usage: &Value) -> Option<(Tokens, Option<f64>)> {
    let (prompt, details, output) = match count(&usage["prompt_tokens"]) {
        Some(prompt) => (
            prompt,
            &usage["prompt_tokens_details"],
            &usage["completion_tokens"],
        ),
        None => (
            count(&usage["input_tokens"])?,
            &usage["input_tokens_details"],
            &usage["output_tokens"],
        ),
    };
    let cached = count(&details["cached_tokens"]).unwrap_or(0).min(prompt);
    Some((
        Tokens {
            input: prompt - cached,
            cached,
            output: count(output).unwrap_or(0),
        },
        usage_cost(usage),
    ))
}

/// Seconds of a sound that was sent for recognition, read from the bytes: a
/// WAV by its header, a voice message (Ogg/Opus) by the positions of its
/// pages. 0.0 when the length cannot be read; the call is then counted as
/// one of unknown size.
pub fn sound_seconds(sound: &[u8]) -> f64 {
    match mika_core::sound::ogg_opus_length(sound) {
        Some(length) => length.as_secs_f64(),
        None => wav_seconds(sound),
    }
}

/// Seconds of sound in a WAV file, from its `fmt ` and `data` chunks. 0.0 for
/// anything that is not a readable WAV.
pub fn wav_seconds(wav: &[u8]) -> f64 {
    if wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return 0.0;
    }
    let le32 = |at: usize| -> Option<u32> {
        wav.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let mut byte_rate = 0u32;
    let mut at = 12;
    while let (Some(id), Some(size)) = (wav.get(at..at + 4), le32(at + 4)) {
        let body = at + 8;
        match id {
            b"fmt " => byte_rate = le32(body + 8).unwrap_or(0),
            b"data" => {
                // A recorder that could not go back writes no length: count what is there.
                let data = (size as usize).min(wav.len().saturating_sub(body));
                return if byte_rate == 0 {
                    0.0
                } else {
                    data as f64 / byte_rate as f64
                };
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        at = body + size as usize + (size as usize & 1);
    }
    0.0
}

/// One streamed LLM request. It is recorded exactly once, when the meter is
/// dropped: the stream was read to the end, cut by an error, or stopped.
/// Without any usage seen the request is recorded as one without token data.
pub(crate) struct StreamMeter {
    kind: CostKind,
    base_url: String,
    model: String,
    free: bool,
    tokens: Option<Tokens>,
    cost: Option<f64>,
}

impl StreamMeter {
    pub fn new(kind: CostKind, base_url: &str, model: &str, free: bool) -> Self {
        Self {
            kind,
            base_url: base_url.to_string(),
            model: model.to_string(),
            free,
            tokens: None,
            cost: None,
        }
    }

    /// The `usage` of an OpenAI-style chunk; the last one seen stays.
    pub fn openai(&mut self, usage: &Value) {
        if let Some((tokens, _)) = parse_openai_usage(usage) {
            self.tokens = Some(tokens);
        }
        if let Some(cost) = usage_cost(usage) {
            self.cost = Some(cost);
        }
    }

    /// `message.usage` of Anthropic's `message_start`: what went in. Writing
    /// to the cache is counted as input.
    pub fn anthropic_start(&mut self, usage: &Value) {
        let Some(input) = count(&usage["input_tokens"]) else {
            return;
        };
        let t = self.tokens.get_or_insert_with(Tokens::default);
        t.input = input + count(&usage["cache_creation_input_tokens"]).unwrap_or(0);
        t.cached = count(&usage["cache_read_input_tokens"]).unwrap_or(0);
        if let Some(output) = count(&usage["output_tokens"]) {
            t.output = output;
        }
    }

    /// `usage` of Anthropic's `message_delta`: what came out so far.
    pub fn anthropic_delta(&mut self, usage: &Value) {
        if let Some(output) = count(&usage["output_tokens"]) {
            self.tokens.get_or_insert_with(Tokens::default).output = output;
        }
    }

    /// The tokens seen so far, for the record of the request.
    pub fn tokens(&self) -> Option<Tokens> {
        self.tokens
    }
}

impl Drop for StreamMeter {
    fn drop(&mut self) {
        let mut call = PaidCall::new(self.kind, &self.base_url, &self.model)
            .free(self.free)
            .cost(self.cost);
        if let Some(tokens) = self.tokens {
            call = call.tokens(tokens);
        }
        mika_core::costs::record(call);
    }
}

/// Counts an answer of `/embeddings`. Some providers give only `total_tokens`:
/// an embedding has no output, so that is the input.
pub(crate) fn record_embedding(base: &str, model: &str, usage: &Value) {
    let mut call = PaidCall::new(CostKind::Embedding, base, model).cost(usage_cost(usage));
    let tokens = parse_openai_usage(usage).map(|(t, _)| t).or_else(|| {
        count(&usage["total_tokens"]).map(|total| Tokens {
            input: total,
            ..Default::default()
        })
    });
    if let Some(tokens) = tokens {
        call = call.tokens(tokens);
    }
    mika_core::costs::record(call);
}

/// Counts an answer of `/images/generations`: one picture, plus the tokens
/// and the cost when the provider reports them.
pub(crate) fn record_image(base: &str, model: &str, usage: &Value) {
    let mut call = PaidCall::new(CostKind::Image, base, model)
        .images(1)
        .cost(usage_cost(usage));
    if let Some((tokens, _)) = parse_openai_usage(usage) {
        call = call.tokens(tokens);
    }
    mika_core::costs::record(call);
}

/// Counts a good answer of `/audio/transcriptions`. The length is what the
/// provider reports (`usage.seconds`, or `duration` of a `verbose_json`
/// answer), else the length of the sound that was sent: a WAV, or a voice
/// message from Telegram as it came. A sound of unknown length is counted
/// with zero seconds and gets no price by the second.
pub(crate) fn record_transcription(base: &str, model: &str, wav: &[u8], body: &str) {
    let answer: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let usage = &answer["usage"];
    let positive = |v: &Value| v.as_f64().filter(|s| s.is_finite() && *s > 0.0);
    let seconds = positive(&usage["seconds"])
        .or_else(|| positive(&answer["duration"]))
        .unwrap_or_else(|| sound_seconds(wav));
    let mut call = PaidCall::new(CostKind::Transcription, base, model)
        .seconds(seconds)
        .cost(usage_cost(usage));
    if let Some((tokens, _)) = parse_openai_usage(usage) {
        call = call.tokens(tokens);
    }
    mika_core::costs::record(call);
}

/// Whether an id from the header `X-Generation-Id` may go into an address.
fn generation_id_ok(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// `total_cost` of an answer of `/generation`.
fn generation_total(v: &Value) -> Option<f64> {
    money(&v["data"]["total_cost"]).or_else(|| money(&v["total_cost"]))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Reads the price list of every provider the tracker asks for. One free
/// request to the model list per provider and day, and only after a paid call.
pub async fn run_price_refresher(live: Live, costs: Costs) {
    loop {
        costs.wanted().await;
        for address in costs.take_wanted() {
            let now = unix_now();
            match live.unit_prices(&address).await {
                Ok(prices) => {
                    let models = prices.len();
                    match costs.set_list_prices(&address, prices, now) {
                        Ok(priced) => log::info!(
                            "price list read: host={} models={models} calls_priced={priced}",
                            provider_host(&address)
                        ),
                        Err(e) => log::warn!(
                            "price list not kept: host={} error={e}",
                            provider_host(&address)
                        ),
                    }
                }
                Err(e) => {
                    log::warn!(
                        "price list not read: host={} error={}",
                        provider_host(&address),
                        mika_core::journal::error_text(&e)
                    );
                    costs.note_price_failure(&address, now);
                }
            }
        }
    }
}

impl Live {
    /// The base addresses of the paid providers in use, without local ones.
    pub fn price_addresses(&self) -> Vec<String> {
        let c = self.config().lock().clone();
        let mut out: Vec<String> = Vec::new();
        let mut add = |address: String| {
            let host = provider_host(&address);
            if !host.is_empty() && !mika_core::costs::is_local(&host) && !out.contains(&address) {
                out.push(address);
            }
        };
        // Ollama at home has no prices; at a public address it is a paid
        // provider like any other.
        let llm = c.llm.effective_base_url();
        if !(c.llm.kind == ProviderKind::Ollama
            && mika_core::costs::is_private(&provider_host(&llm)))
        {
            add(llm);
        }
        if !c.image.model.trim().is_empty() {
            add(self.base(&c.image.base_url));
        }
        if !c.embedding.model.trim().is_empty() {
            add(self.base(&c.embedding.base_url));
        }
        let speaks = c.voice.tts == TtsEngine::Provider && !c.voice.tts_model.trim().is_empty();
        if speaks || !c.voice.stt_model.trim().is_empty() {
            add(self.base(&c.voice.speech_base_url));
        }
        out
    }

    /// The key that paid calls to `base_url` go with, and whether the address
    /// is Anthropic's. The address of the main model gets its key and no
    /// other: a section with an empty address of its own points there too,
    /// and its key may be left from another provider. Any other address gets
    /// the key of the section that uses it now. An address no section of the
    /// settings uses gets no key: a key is sent only where its own requests go.
    fn key_for(&self, base_url: &str) -> (Option<Secret>, bool) {
        let c = self.config().lock().clone();
        let same = |own: &str| self.base(own) == base_url;
        let llm = c.llm.effective_base_url() == base_url;
        let anthropic = llm && c.llm.kind == ProviderKind::Anthropic;
        let speaks = c.voice.tts == TtsEngine::Provider && !c.voice.tts_model.trim().is_empty();
        let speech_in_use = speaks || !c.voice.stt_model.trim().is_empty();
        let key = if llm {
            self.key(&c.llm.kind.secret_key())
        } else if same(&c.image.base_url) && !c.image.model.trim().is_empty() {
            self.key(IMAGE_SECRET_KEY)
        } else if same(&c.embedding.base_url) && !c.embedding.model.trim().is_empty() {
            self.key(EMBEDDING_SECRET_KEY)
        } else if same(&c.voice.speech_base_url) && speech_in_use {
            self.key(SPEECH_SECRET_KEY)
        } else {
            None
        };
        (key, anthropic)
    }

    /// The prices of a provider's models, from its model list: one free
    /// request. A list without prices gives an empty answer.
    pub async fn unit_prices(&self, base_url: &str) -> Result<Vec<(String, UnitPrices)>, String> {
        let base_url = base_url.trim().trim_end_matches('/');
        let (key, anthropic) = self.key_for(base_url);
        let models = crate::catalog::fetch_models(base_url, key.as_ref(), anthropic).await?;
        Ok(models
            .into_iter()
            .filter(|m| !m.unit.is_empty())
            .map(|m| (m.id, m.unit))
            .collect())
    }

    /// What a generation cost, by its id from the header `X-Generation-Id`:
    /// `GET /generation?id=`, as OpenRouter serves it. `None` on any failure.
    pub(crate) async fn generation_cost(
        &self,
        base_url: &str,
        key: Option<&Secret>,
        id: &str,
    ) -> Option<f64> {
        if !generation_id_ok(id) {
            return None;
        }
        tokio::time::sleep(GENERATION_DELAY).await;
        let mut req = self
            .http()
            .get(format!("{}/generation", base_url.trim_end_matches('/')))
            .query(&[("id", id)])
            .timeout(GENERATION_TIMEOUT);
        if let Some(k) = key.filter(|k| !k.is_empty()) {
            req = req.bearer_auth(k.expose());
        }
        // A cost that cannot be asked for leaves the phrase counted without
        // money: worth a line only when prices are being looked into.
        let resp = req
            .send()
            .await
            .map_err(|e| log::debug!("cost of a generation not asked: {}", e.without_url()))
            .ok()?;
        if !resp.status().is_success() {
            log::debug!(
                "cost of a generation not given: http={}",
                resp.status().as_u16()
            );
            return None;
        }
        generation_total(&resp.json::<Value>().await.ok()?)
    }

    /// Counts a phrase the provider has just spoken, by its characters. When
    /// no price covers it and the answer named its generation, the cost is
    /// asked for in the background and the phrase is counted when it comes.
    pub(crate) fn meter_speech(
        &self,
        base: &str,
        model: &str,
        text: &str,
        generation: Option<String>,
        key: Option<Secret>,
    ) {
        let Some(costs) = mika_core::costs::installed() else {
            return;
        };
        if !costs.enabled() {
            return;
        }
        let call = PaidCall::new(CostKind::Speech, base, model).chars(text.chars().count() as u64);
        let asked = generation
            .filter(|id| generation_id_ok(id) && !costs.can_price(&call))
            .zip(tokio::runtime::Handle::try_current().ok());
        let Some((id, runtime)) = asked else {
            costs.record(call);
            return;
        };
        let (live, base) = (self.clone(), base.to_string());
        runtime.spawn(async move {
            let cost = live.generation_cost(&base, key.as_ref(), &id).await;
            costs.record(call.cost(cost));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::llm::ChatMessage;
    use mika_core::storage::Db;
    use serde_json::json;
    use std::sync::{Arc, OnceLock};

    /// The tracker of this test process, over a base in memory. The sink is
    /// one per process and tests run side by side: every test uses a model
    /// name of its own.
    fn tracker() -> Costs {
        static COSTS: OnceLock<Costs> = OnceLock::new();
        COSTS
            .get_or_init(|| {
                let costs = Costs::new(Arc::new(Db::open_in_memory().unwrap()));
                mika_core::costs::install(costs.clone());
                costs
            })
            .clone()
    }

    /// The report line of one model today.
    fn line(model: &str) -> Option<mika_core::costs::UsageLine> {
        let today = mika_core::costs::today();
        tracker()
            .report(today, today)
            .unwrap()
            .models
            .into_iter()
            .find(|l| l.model == model)
    }

    #[test]
    fn pictures_make_a_request_vision() {
        let chat = LlmRequest {
            messages: vec![ChatMessage::system("s"), ChatMessage::user("u")],
            tools: Vec::new(),
        };
        assert_eq!(request_kind(&chat), CostKind::Chat);
        let vision = LlmRequest {
            messages: vec![ChatMessage::user_with_image("что тут?", "QUJD")],
            tools: Vec::new(),
        };
        assert_eq!(request_kind(&vision), CostKind::Vision);
    }

    #[test]
    fn openai_usage_gives_tokens_and_cost() {
        // Plain OpenAI.
        assert_eq!(
            parse_openai_usage(
                &json!({"prompt_tokens": 120, "completion_tokens": 30, "total_tokens": 150})
            ),
            Some((
                Tokens {
                    input: 120,
                    cached: 0,
                    output: 30
                },
                None
            ))
        );
        // Cached tokens are taken out of the input.
        assert_eq!(
            parse_openai_usage(&json!({
                "prompt_tokens": 1000,
                "completion_tokens": 50,
                "prompt_tokens_details": {"cached_tokens": 800}
            })),
            Some((
                Tokens {
                    input: 200,
                    cached: 800,
                    output: 50
                },
                None
            ))
        );
        // More cached than sent cannot be.
        let odd = parse_openai_usage(
            &json!({"prompt_tokens": 10, "prompt_tokens_details": {"cached_tokens": 99}}),
        )
        .unwrap()
        .0;
        assert_eq!((odd.input, odd.cached, odd.output), (0, 10, 0));
        // RouterAI and OpenRouter add the cost, as a number or as a string.
        let cost = |v: Value| parse_openai_usage(&v).unwrap().1;
        assert_eq!(
            cost(json!({"prompt_tokens": 1, "completion_tokens": 1, "cost": 0.0123})),
            Some(0.0123)
        );
        assert_eq!(cost(json!({"prompt_tokens": 1, "cost": "0.5"})), Some(0.5));
        assert_eq!(cost(json!({"prompt_tokens": 1, "cost": -1})), None);
        assert_eq!(cost(json!({"prompt_tokens": 1, "cost": "много"})), None);
        // The names of `/audio/transcriptions` and `/images/generations`.
        assert_eq!(
            parse_openai_usage(&json!({
                "type": "tokens", "input_tokens": 14, "output_tokens": 45,
                "input_tokens_details": {"cached_tokens": 4}
            })),
            Some((
                Tokens {
                    input: 10,
                    cached: 4,
                    output: 45
                },
                None
            ))
        );
        // Garbage.
        for v in [
            json!(null),
            json!("usage"),
            json!({}),
            json!({"prompt_tokens": "много"}),
            json!({"completion_tokens": 5}),
            json!({"seconds": 4.3}),
        ] {
            assert_eq!(parse_openai_usage(&v), None, "{v}");
        }
    }

    /// One Ogg page that ends at `position` and carries `data`.
    fn ogg_page(position: u64, data: &[u8]) -> Vec<u8> {
        let mut page = b"OggS\0\0".to_vec();
        page.extend_from_slice(&position.to_le_bytes());
        page.extend_from_slice(&[0; 12]);
        let sizes: Vec<u8> = data.chunks(255).map(|c| c.len() as u8).collect();
        page.push(sizes.len() as u8);
        page.extend_from_slice(&sizes);
        page.extend_from_slice(data);
        page
    }

    #[test]
    fn the_length_of_a_voice_message_is_read_from_its_pages() {
        // An Ogg/Opus stream of 9 seconds, as Telegram sends a voice message.
        let mut head = b"OpusHead\x01\x01".to_vec();
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&[0x80, 0xBB, 0, 0, 0, 0, 0]);
        let mut voice = ogg_page(0, &head);
        voice.extend(ogg_page(0, b"OpusTags\0\0\0\0\0\0\0\0"));
        voice.extend(ogg_page(48_000 * 9, &[7; 300]));
        assert!((sound_seconds(&voice) - 9.0).abs() < 1e-9);
        // A WAV is still read by its header.
        let wav = crate::speech::wav_from_pcm(&[0u8; 80_000], 16_000);
        assert!((sound_seconds(&wav) - 2.5).abs() < 1e-9);
        // Neither: the length is unknown, and no second is made up.
        assert_eq!(sound_seconds(b"OggS\0\x02not really Opus"), 0.0);
        assert_eq!(sound_seconds(b"ID3 an mp3"), 0.0);
        assert_eq!(sound_seconds(b""), 0.0);
    }

    #[test]
    fn the_length_of_a_wav_is_read_from_its_header() {
        // 16 kHz, mono, 16 bit: 32000 bytes a second.
        let wav = crate::speech::wav_from_pcm(&[0u8; 80_000], 16_000);
        assert!((wav_seconds(&wav) - 2.5).abs() < 1e-9);
        // A chunk before the data is stepped over, odd length and all.
        let mut listed = wav[..36].to_vec();
        listed.extend_from_slice(b"LIST");
        listed.extend_from_slice(&3u32.to_le_bytes());
        listed.extend_from_slice(b"abc\0");
        listed.extend_from_slice(&wav[36..]);
        assert!((wav_seconds(&listed) - 2.5).abs() < 1e-9);
        // A length that was never written: what is there is counted.
        let mut open = wav.clone();
        open[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!((wav_seconds(&open) - 2.5).abs() < 1e-9);
        // Not a WAV, a cut header, a zero byte rate.
        assert_eq!(wav_seconds(b"OggS\0\0\0\0\0\0\0\0\0\0\0\0"), 0.0);
        assert_eq!(wav_seconds(b"RIFFfakewav"), 0.0);
        assert_eq!(wav_seconds(&wav[..30]), 0.0);
        assert_eq!(wav_seconds(b""), 0.0);
        let mut broken = wav.clone();
        broken[28..32].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(wav_seconds(&broken), 0.0);
    }

    #[test]
    fn a_stream_is_recorded_once_when_its_meter_is_dropped() {
        tracker();
        let model = "meter-test/openai";
        let mut m = StreamMeter::new(CostKind::Chat, "https://routerai.ru/api/v1", model, false);
        assert!(line(model).is_none(), "nothing before the drop");
        m.openai(&json!({"prompt_tokens": 5, "completion_tokens": 1}));
        // The last usage seen is the one that counts.
        m.openai(&json!({
            "prompt_tokens": 1000, "completion_tokens": 200,
            "prompt_tokens_details": {"cached_tokens": 300}, "cost": 0.42
        }));
        drop(m);
        let l = line(model).unwrap();
        assert_eq!((l.requests, l.blind, l.unpriced), (1, 0, 0));
        assert_eq!(
            (l.tokens_in, l.tokens_cached, l.tokens_out),
            (700, 300, 200)
        );
        assert_eq!(l.cost.len(), 1);
        assert_eq!((l.cost[0].cost, l.cost[0].currency.as_str()), (0.42, "RUB"));
        assert_eq!(l.kind, CostKind::Chat);
    }

    #[test]
    fn a_stream_without_usage_is_recorded_blind() {
        tracker();
        let model = "meter-test/blind";
        drop(StreamMeter::new(
            CostKind::Vision,
            "https://routerai.ru/api/v1",
            model,
            false,
        ));
        let l = line(model).unwrap();
        assert_eq!((l.requests, l.blind, l.unpriced), (1, 1, 0));
        assert!(l.cost.is_empty() && !l.free);
        assert_eq!(l.kind, CostKind::Vision);
    }

    #[test]
    fn anthropic_usage_comes_in_two_parts() {
        tracker();
        let model = "meter-test/anthropic";
        let mut m = StreamMeter::new(CostKind::Chat, "https://api.anthropic.com", model, false);
        m.anthropic_start(&json!({
            "input_tokens": 25, "cache_creation_input_tokens": 100,
            "cache_read_input_tokens": 400, "output_tokens": 1
        }));
        m.anthropic_delta(&json!({"output_tokens": 15}));
        m.anthropic_delta(&json!({"output_tokens": 90}));
        drop(m);
        let l = line(model).unwrap();
        assert_eq!((l.tokens_in, l.tokens_cached, l.tokens_out), (125, 400, 90));
        // Anthropic lists no prices: units only, until the user types a price.
        assert_eq!((l.requests, l.blind, l.unpriced), (1, 0, 1));
    }

    #[test]
    fn a_free_provider_is_recorded_as_free() {
        tracker();
        let model = "meter-test/ollama";
        let mut m = StreamMeter::new(CostKind::Chat, "http://localhost:11434/v1", model, true);
        m.openai(&json!({"prompt_tokens": 40, "completion_tokens": 8}));
        drop(m);
        let l = line(model).unwrap();
        assert!(l.free && l.cost.is_empty());
        assert_eq!(l.units, "48 ток.");
    }

    #[test]
    fn embeddings_pictures_and_recognition_are_recorded_with_their_units() {
        tracker();
        record_embedding(
            "https://routerai.ru/api/v1",
            "meter-test/embed",
            &json!({"prompt_tokens": 64, "total_tokens": 64, "cost": "0.002"}),
        );
        let l = line("meter-test/embed").unwrap();
        assert_eq!(
            (l.kind, l.tokens_in, l.tokens_out),
            (CostKind::Embedding, 64, 0)
        );
        assert_eq!(l.cost[0].cost, 0.002);
        // Only the total is known: it is the input.
        record_embedding(
            "https://routerai.ru/api/v1",
            "meter-test/embed-total",
            &json!({"total_tokens": 12}),
        );
        assert_eq!(line("meter-test/embed-total").unwrap().tokens_in, 12);
        record_embedding(
            "https://routerai.ru/api/v1",
            "meter-test/embed-blind",
            &Value::Null,
        );
        assert_eq!(line("meter-test/embed-blind").unwrap().blind, 1);

        record_image(
            "https://routerai.ru/api/v1",
            "meter-test/image",
            &Value::Null,
        );
        let l = line("meter-test/image").unwrap();
        assert_eq!(
            (l.kind, l.images, l.blind, l.unpriced),
            (CostKind::Image, 1, 0, 1)
        );

        let wav = crate::speech::wav_from_pcm(&[0u8; 64_000], 16_000);
        let base = "https://routerai.ru/api/v1";
        // The provider's own count wins over the length of the file.
        record_transcription(
            base,
            "meter-test/asr",
            &wav,
            r#"{"text":"привет","usage":{"seconds":4.3,"cost":0.0015}}"#,
        );
        let l = line("meter-test/asr").unwrap();
        assert_eq!((l.kind, l.seconds), (CostKind::Transcription, 4.3));
        assert_eq!(l.cost[0].cost, 0.0015);
        // No usage: the WAV is two seconds long.
        record_transcription(base, "meter-test/asr-wav", &wav, r#"{"text":"привет"}"#);
        assert_eq!(line("meter-test/asr-wav").unwrap().seconds, 2.0);
        // A `verbose_json` answer names the length itself.
        record_transcription(
            base,
            "meter-test/asr-verbose",
            b"OggS",
            r#"{"text":"привет","duration":7.5,"segments":[]}"#,
        );
        assert_eq!(line("meter-test/asr-verbose").unwrap().seconds, 7.5);
        // Not a WAV and nothing reported: a request of unknown length, without
        // money, not a free one.
        record_transcription(base, "meter-test/asr-ogg", b"OggS", r#"{"text":"привет"}"#);
        let l = line("meter-test/asr-ogg").unwrap();
        assert_eq!((l.seconds, l.blind, l.unpriced), (0.0, 1, 0));
        assert!(!l.free && l.cost.is_empty());
        // Tokens of a model that is billed by them.
        record_transcription(
            base,
            "meter-test/asr-tokens",
            &wav,
            r#"{"text":"привет","usage":{"type":"tokens","input_tokens":14,"output_tokens":45}}"#,
        );
        let l = line("meter-test/asr-tokens").unwrap();
        assert_eq!((l.tokens_in, l.tokens_out, l.seconds), (14, 45, 2.0));
    }

    #[test]
    fn a_generation_id_is_checked_and_its_cost_is_read() {
        assert!(generation_id_ok("gen-1731571200-AbC_d.e:f"));
        for bad in ["", "a b", "a/b", "a?b=c", "a&b", "ид", &"x".repeat(129)] {
            assert!(!generation_id_ok(bad), "{bad}");
        }
        assert_eq!(
            generation_total(&json!({"data": {"total_cost": 0.0042}})),
            Some(0.0042)
        );
        assert_eq!(generation_total(&json!({"total_cost": "0.5"})), Some(0.5));
        assert_eq!(generation_total(&json!({"data": {"total_cost": -1}})), None);
        assert_eq!(generation_total(&json!({"error": "not found"})), None);
    }

    #[test]
    fn only_paid_addresses_in_use_are_asked_for_prices() {
        use mika_core::config::Config;
        use mika_core::secrets::{MemorySecretStore, SecretStore};
        use parking_lot::Mutex;

        let live = |c: Config, secrets: MemorySecretStore| {
            Live::new(Arc::new(Mutex::new(c)), Arc::new(secrets))
        };
        let mut c = Config::default();
        assert_eq!(
            live(c.clone(), MemorySecretStore::default()).price_addresses(),
            ["https://api.openai.com/v1"]
        );
        c.llm.base_url = "https://routerai.ru/api/v1/".into();
        c.image.model = "flux".into();
        c.embedding.model = "embed".into();
        c.embedding.base_url = "http://localhost:11434/v1".into();
        c.voice.stt_model = "whisper-1".into();
        c.voice.speech_base_url = "https://api.openai.com/v1".into();
        // A section with an address and no model is not in use.
        let mut unused = c.clone();
        unused.voice.stt_model.clear();
        assert_eq!(
            live(unused, MemorySecretStore::default()).price_addresses(),
            ["https://routerai.ru/api/v1"]
        );
        let secrets = MemorySecretStore::default();
        secrets
            .set("llm.openai.api_key", &Secret::new("llm-key"))
            .unwrap();
        secrets
            .set(SPEECH_SECRET_KEY, &Secret::new("speech-key"))
            .unwrap();
        let l = live(c.clone(), secrets);
        assert_eq!(
            l.price_addresses(),
            ["https://routerai.ru/api/v1", "https://api.openai.com/v1"]
        );
        // Each address gets the key its paid calls go with.
        let key = |base: &str| l.key_for(base).0.map(|k| k.expose().to_string());
        assert_eq!(
            key("https://routerai.ru/api/v1").as_deref(),
            Some("llm-key")
        );
        assert_eq!(
            key("https://api.openai.com/v1").as_deref(),
            Some("speech-key")
        );
        // An address the settings no longer point at gets no key at all.
        assert_eq!(key("https://elsewhere.test/v1"), None);
        assert!(!l.key_for("https://routerai.ru/api/v1").1);

        // A speech key left from another provider, the speech address
        // emptied, provider speech not in use: the address of the main model
        // gets the key of the main model, never the old speech key.
        let mut left = c.clone();
        left.voice.speech_base_url.clear();
        left.voice.stt_model.clear();
        left.image.model.clear();
        left.embedding.model.clear();
        let secrets = MemorySecretStore::default();
        secrets
            .set("llm.openai.api_key", &Secret::new("llm-key"))
            .unwrap();
        secrets
            .set(SPEECH_SECRET_KEY, &Secret::new("old-speech-key"))
            .unwrap();
        let l = live(left.clone(), secrets);
        assert_eq!(l.price_addresses(), ["https://routerai.ru/api/v1"]);
        let key = |base: &str| l.key_for(base).0.map(|k| k.expose().to_string());
        assert_eq!(
            key("https://routerai.ru/api/v1").as_deref(),
            Some("llm-key")
        );
        // The same with speech in use at the address of the main model.
        left.voice.stt_model = "whisper-1".into();
        let secrets = MemorySecretStore::default();
        secrets
            .set("llm.openai.api_key", &Secret::new("llm-key"))
            .unwrap();
        secrets
            .set(SPEECH_SECRET_KEY, &Secret::new("old-speech-key"))
            .unwrap();
        let l = live(left.clone(), secrets);
        assert_eq!(
            l.key_for("https://routerai.ru/api/v1")
                .0
                .map(|k| k.expose().to_string())
                .as_deref(),
            Some("llm-key")
        );
        // A speech address of its own that is not in use gets no key.
        left.voice.stt_model.clear();
        left.voice.speech_base_url = "https://speech.example.test/v1".into();
        let secrets = MemorySecretStore::default();
        secrets
            .set(SPEECH_SECRET_KEY, &Secret::new("old-speech-key"))
            .unwrap();
        let l = live(left, secrets);
        assert!(l.key_for("https://speech.example.test/v1").0.is_none());

        c.llm.kind = ProviderKind::Ollama;
        c.llm.base_url.clear();
        c.image.model.clear();
        let local = live(c.clone(), MemorySecretStore::default());
        assert_eq!(local.price_addresses(), ["https://api.openai.com/v1"]);
        // Ollama at a public address is a provider with prices to ask for.
        let mut hosted = c.clone();
        hosted.llm.base_url = "https://ollama.example.com/v1".into();
        assert_eq!(
            live(hosted, MemorySecretStore::default()).price_addresses(),
            ["https://ollama.example.com/v1", "https://api.openai.com/v1"]
        );
        let mut lan = c.clone();
        lan.llm.base_url = "http://192.168.1.20:11434/v1".into();
        assert_eq!(
            live(lan, MemorySecretStore::default()).price_addresses(),
            ["https://api.openai.com/v1"]
        );
        c.llm.kind = ProviderKind::Anthropic;
        let anthropic = live(c, MemorySecretStore::default());
        assert!(anthropic.key_for("https://api.anthropic.com").1);
        assert!(!anthropic.key_for("https://api.openai.com/v1").1);
    }
}
