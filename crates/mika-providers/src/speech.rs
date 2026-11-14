//! Speech through a provider: recognition by `/audio/transcriptions`,
//! synthesis by `/audio/speech` (OpenAI-compatible, as RouterAI serves them).

use async_trait::async_trait;
use mika_core::config::SPEECH_SECRET_KEY;
use mika_core::llm::{SpeechSegment, SpeechToText, TextToSpeech};
use mika_core::sound::SoundKind;
use mika_core::speech_cache::SoundKey;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::factory::redact;
use crate::live::Live;

/// Longest wait for one phrase.
const SPEECH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Address and model of the speech providers that refused `instructions`:
/// until the app restarts they get every phrase without them at once, not
/// after a refused request.
static NO_INSTRUCTIONS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// Address, model and format of the speech providers that refused
/// `response_format` with that format: until the app restarts they are asked
/// without the field at once.
static NO_FORMAT: Mutex<Vec<(String, String, String)>> = Mutex::new(Vec::new());
/// Sample rate assumed for sound that came without a header.
const RAW_PCM_RATE: u32 = 24_000;
/// Name and content type of the sound the microphone sends.
const WAV_FILE: (&str, &str) = ("speech.wav", "audio/wav");

/// True when the bytes start like a sound file a player understands.
pub fn has_audio_header(bytes: &[u8]) -> bool {
    bytes.starts_with(b"RIFF")
        || bytes.starts_with(b"ID3")
        || bytes.starts_with(b"OggS")
        || bytes.starts_with(b"fLaC")
        // MPEG audio frame sync.
        || (bytes.len() > 1 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0)
}

/// A WAV file around 16-bit mono samples.
pub fn wav_from_pcm(pcm: &[u8], rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Sample rate of raw sound from its content type: `audio/pcm; rate=44100`.
pub fn pcm_rate(content_type: &str) -> Option<u32> {
    content_type
        .split(';')
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("rate"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .filter(|r| (8_000..=96_000).contains(r))
}

/// Sound a player understands. The content type alone cannot be trusted:
/// RouterAI labels a WAV file as `audio/pcm`. A file with a header stays as it
/// is; raw samples get a WAV header at the rate the answer names (Fish Audio
/// sends 44.1 kHz), or 24 kHz when it names none.
pub fn playable(bytes: &[u8], content_type: &str) -> Vec<u8> {
    let named_file = [b"RIFF".as_slice(), b"ID3", b"OggS", b"fLaC"]
        .iter()
        .any(|h| bytes.starts_with(h));
    if named_file {
        return bytes.to_vec();
    }
    // Raw samples may start with 0xFFFF, which looks like an MPEG frame.
    if let Some(rate) = pcm_rate(content_type) {
        return wav_from_pcm(bytes, rate);
    }
    // Only a real MPEG frame counts here: the sample -1 at a quiet start of
    // 24 kHz samples labelled plain `audio/pcm` is not one (decision 0029).
    if SoundKind::of(bytes) != SoundKind::Unknown {
        bytes.to_vec()
    } else {
        wav_from_pcm(bytes, RAW_PCM_RATE)
    }
}

/// Text of an OpenAI-style transcription answer.
pub fn parse_transcription(body: &str) -> Result<String, String> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| format!("непонятный ответ распознавания: {e}"))?;
    v["text"]
        .as_str()
        .map(|t| t.trim().to_string())
        .ok_or_else(|| "в ответе распознавания нет текста".to_string())
}

/// Message of a provider error body: `{"error":{"message":"voice is missing"}}`.
fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

impl Live {
    fn speech_settings(&self) -> mika_core::config::VoiceConfig {
        self.config().lock().voice.clone()
    }
}

/// Phrases with times of a `verbose_json` transcription answer. `None` when
/// the answer has no `segments`: the provider cannot tell times.
pub fn parse_segments(body: &str) -> Result<Option<Vec<SpeechSegment>>, String> {
    let v: Value =
        serde_json::from_str(body).map_err(|e| format!("непонятный ответ распознавания: {e}"))?;
    let Some(items) = v["segments"].as_array() else {
        return Ok(None);
    };
    let number = |v: &Value| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    };
    let segments = items
        .iter()
        .filter_map(|s| {
            let text = s["text"].as_str()?.trim().to_string();
            let (start, end) = (number(&s["start"])?, number(&s["end"])?);
            (!text.is_empty()).then_some(SpeechSegment { start, end, text })
        })
        .collect();
    Ok(Some(segments))
}

/// A refusal of the request for times rather than of the sound.
fn refuses_times(status: u16, body: &str) -> bool {
    let body = body.to_lowercase();
    status == 400
        && ["response_format", "verbose_json", "timestamp"]
            .iter()
            .any(|w| body.contains(w))
}

impl Live {
    /// One request to `/audio/transcriptions` with the extra form fields.
    /// `wav` is the sound file, WAV or another kind; `file` is the name and
    /// the content type it goes under: the provider tells the format by the
    /// extension of the name. Returns the status and the body; the key never
    /// reaches the error text.
    async fn transcription(
        &self,
        wav: &[u8],
        file: (&str, &str),
        extra: &[(&str, &str)],
    ) -> Result<(u16, String), String> {
        let voice = self.speech_settings();
        let model = voice.stt_model.trim().to_string();
        if model.is_empty() {
            return Err(
                "модель распознавания речи не задана: укажи её в настройках, раздел «Голос»".into(),
            );
        }
        let key = self.key(SPEECH_SECRET_KEY);
        let hide = |text: String| redact(&text, key.as_ref());
        let (file_name, mime) = file;
        let base = self.base(&voice.speech_base_url);
        let asked = std::time::Instant::now();
        // What was said is the user's: the record has the size of the sound
        // and of the answer, never the words.
        let note = |outcome: Result<(u16, usize), &str>| {
            let sizes = format!(
                "sound_s={:.1} bytes={}{}",
                crate::metering::sound_seconds(wav),
                wav.len(),
                match outcome {
                    Ok((status, chars)) => format!(" http={status} answer_chars={chars}"),
                    Err(_) => String::new(),
                }
            );
            let (level, line) = crate::trace::call_line(
                "recognition",
                &base,
                &model,
                asked.elapsed().as_millis(),
                &sizes,
                outcome.map(|_| ()),
            );
            log::log!(target: "voice", level, "{line}");
        };
        let file = reqwest::multipart::Part::bytes(wav.to_vec())
            .file_name(file_name.to_string())
            .mime_str(mime)
            .map_err(|e| e.to_string())?;
        let mut form = reqwest::multipart::Form::new()
            .text("model", model.clone())
            .part("file", file);
        let language = voice.stt_language.trim().to_string();
        if !language.is_empty() {
            form = form.text("language", language);
        }
        for (name, value) in extra {
            form = form.text(name.to_string(), value.to_string());
        }
        let mut req = self
            .http()
            .post(format!("{base}/audio/transcriptions"))
            .timeout(SPEECH_TIMEOUT)
            .multipart(form);
        if let Some(k) = &key {
            req = req.bearer_auth(k.expose());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| hide(format!("нет связи с распознаванием: {}", e.without_url())))
            .inspect_err(|why| note(Err(why)))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| hide(e.without_url().to_string()))
            .inspect_err(|why| note(Err(why)))?;
        if (200..300).contains(&status) {
            note(Ok((status, body.chars().count())));
            // Every recognition request passes here: it is counted once (decision 0028).
            crate::metering::record_transcription(
                &self.base(&voice.speech_base_url),
                voice.stt_model.trim(),
                wav,
                &body,
            );
            // A good answer comes back whole: `hide` would cut a long text at
            // 300 characters and the JSON would not parse. Only the key is hidden.
            return Ok((
                status,
                match key.as_ref().filter(|k| !k.is_empty()) {
                    Some(k) => body.replace(k.expose(), "***"),
                    None => body,
                },
            ));
        }
        let body = hide(body);
        // A refusal: the body is the provider's error, not what was said.
        note(Err(&format!("http {status}: {}", error_message(&body))));
        Ok((status, body))
    }
}

#[async_trait]
impl SpeechToText for Live {
    async fn transcribe(&self, wav: &[u8]) -> Result<String, String> {
        let (status, body) = self.transcription(wav, WAV_FILE, &[]).await?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "распознавание ответило {status}: {}",
                error_message(&body)
            ));
        }
        parse_transcription(&body).inspect_err(|why| not_understood(status, why))
    }

    async fn transcribe_segments(&self, wav: &[u8]) -> Result<Option<Vec<SpeechSegment>>, String> {
        let (status, body) = self
            .transcription(
                wav,
                WAV_FILE,
                &[
                    ("response_format", "verbose_json"),
                    ("timestamp_granularities[]", "segment"),
                ],
            )
            .await?;
        if refuses_times(status, &body) {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "распознавание ответило {status}: {}",
                error_message(&body)
            ));
        }
        parse_segments(&body).inspect_err(|why| not_understood(status, why))
    }
}

/// The provider answered well, and the answer cannot be read: no text in
/// it, or not JSON. The record of the request itself says `outcome=ok`, so
/// without this one a failed recognition would have no reason. The reason
/// is the parser's or the app's own words, never what was said.
fn not_understood(status: u16, why: &str) {
    log::warn!(
        target: "voice",
        "recognition answer not understood: http={status} error={}",
        mika_core::journal::error_text(&why)
    );
}

/// Why recognition failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttFailure {
    /// The provider refused the request itself (HTTP 400, 415 or 422): for
    /// a sound that is not WAV, most likely its format.
    Refused(String),
    Other(String),
}

impl std::fmt::Display for SttFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SttFailure::Refused(why) | SttFailure::Other(why) => f.write_str(why),
        }
    }
}

impl Live {
    /// Recognises a sound file as it is, such as the OGG/Opus of a Telegram
    /// voice message. The file goes as `voice.<extension>`: RouterAI tells
    /// the format by the extension.
    pub async fn transcribe_sound(
        &self,
        sound: &[u8],
        kind: SoundKind,
    ) -> Result<String, SttFailure> {
        if kind == SoundKind::Unknown {
            return Err(SttFailure::Other("неизвестный формат звука".into()));
        }
        let name = format!("voice.{}", kind.extension());
        let (status, body) = self
            .transcription(sound, (&name, kind.mime()), &[])
            .await
            .map_err(SttFailure::Other)?;
        if (200..300).contains(&status) {
            return parse_transcription(&body).map_err(SttFailure::Other);
        }
        let why = format!("распознавание ответило {status}: {}", error_message(&body));
        Err(match status {
            400 | 415 | 422 => SttFailure::Refused(why),
            _ => SttFailure::Other(why),
        })
    }
}

#[async_trait]
impl TextToSpeech for Live {
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>, String> {
        let style = self.speech_settings().tts_style;
        self.synthesize_with(text, style.trim()).await
    }
}

impl Live {
    /// Speech with `instructions`: the voice and tone in words, as
    /// `gpt-4o-mini-tts` takes them. A provider that refuses the field gets the
    /// request again without it, and later phrases go to it without it at once.
    pub async fn synthesize_with(&self, text: &str, instructions: &str) -> Result<Vec<u8>, String> {
        self.synthesize_opts(text, instructions, None).await
    }

    /// Speech in `format` (`mp3`), asked for as `response_format`. A provider
    /// that refuses the request with the field gets it again without it and
    /// is remembered until the app restarts; the sound then comes in the
    /// provider's own format. Look at the bytes, not at what was asked.
    pub async fn synthesize_format(
        &self,
        text: &str,
        instructions: &str,
        format: &str,
    ) -> Result<Vec<u8>, String> {
        let format = format.trim();
        let voice = self.speech_settings();
        let provider = (
            self.base(&voice.speech_base_url),
            voice.tts_model.trim().to_string(),
            format.to_string(),
        );
        if format.is_empty() || NO_FORMAT.lock().contains(&provider) {
            return self.synthesize_with(text, instructions).await;
        }
        match self.synthesize_opts(text, instructions, Some(format)).await {
            // Too many requests is not a refusal of the field.
            Err(e)
                if e.starts_with("синтез речи ответил 4")
                    && !e.starts_with("синтез речи ответил 429") =>
            {
                log::warn!(
                    "speech provider refused the format, asking without it: format={format} error={}",
                    mika_core::journal::error_text(&e)
                );
                let sound = self.synthesize_with(text, instructions).await;
                if sound.is_ok() {
                    let mut refused = NO_FORMAT.lock();
                    if !refused.contains(&provider) {
                        refused.push(provider);
                    }
                }
                sound
            }
            other => other,
        }
    }

    /// `synthesize_with`, with the sound asked for in `format` when one is named.
    async fn synthesize_opts(
        &self,
        text: &str,
        instructions: &str,
        format: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let instructions = instructions.trim();
        let voice = self.speech_settings();
        let provider = (
            self.base(&voice.speech_base_url),
            voice.tts_model.trim().to_string(),
        );
        if instructions.is_empty() || NO_INSTRUCTIONS.lock().contains(&provider) {
            return self.speak_once(text, "", format).await;
        }
        match self.speak_once(text, instructions, format).await {
            Err(e) if e.starts_with("синтез речи ответил 4") => {
                log::warn!(
                    "speech provider refused instructions, asking without them: error={}",
                    mika_core::journal::error_text(&e)
                );
                let sound = self.speak_once(text, "", format).await;
                // Too many requests is not a refusal of the field.
                if sound.is_ok() && !e.starts_with("синтез речи ответил 429") {
                    let mut refused = NO_INSTRUCTIONS.lock();
                    if !refused.contains(&provider) {
                        refused.push(provider);
                    }
                }
                sound
            }
            other => other,
        }
    }

    /// One request to `/audio/speech`. With `format` the sound is asked for
    /// as `response_format`.
    async fn speak_once(
        &self,
        text: &str,
        instructions: &str,
        format: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        let voice = self.speech_settings();
        let model = voice.tts_model.trim().to_string();
        if model.is_empty() {
            return Err("модель синтеза речи не задана".into());
        }
        let base = self.base(&voice.speech_base_url);
        let name = voice.tts_voice.trim();
        // The same words said in another tone are another sound.
        let voice_key = if instructions.is_empty() {
            name.to_string()
        } else {
            format!("{name}|{instructions}")
        };
        // And so are the same words in another format; a key without a
        // format stays as it was, so the sounds kept before stay valid.
        let voice_key = match format {
            Some(format) => format!("{voice_key}|format={format}"),
            None => voice_key,
        };
        let sound_key = SoundKey {
            base_url: &base,
            model: &model,
            voice: &voice_key,
            text,
        };
        let cache = self.speech_cache().filter(|_| voice.tts_cache);
        let chars = text.chars().count();
        if let Some(sound) = cache.and_then(|c| c.get(&sound_key)) {
            log::debug!(
                target: "voice",
                "synthesis from the cache: chars={chars} bytes={}",
                sound.len()
            );
            return Ok(sound);
        }
        let key = self.key(SPEECH_SECRET_KEY);
        let hide = |text: String| redact(&text, key.as_ref());
        let asked = std::time::Instant::now();
        // What is said is the model's text: the record has its size only.
        let note = |outcome: Result<usize, &str>| {
            let sizes = format!(
                "engine=provider chars={chars} styled={} format={}{}",
                !instructions.is_empty(),
                format.unwrap_or("-"),
                match outcome {
                    Ok(bytes) => format!(" bytes={bytes}"),
                    Err(_) => String::new(),
                }
            );
            let (level, line) = crate::trace::call_line(
                "synthesis",
                &base,
                &model,
                asked.elapsed().as_millis(),
                &sizes,
                outcome.map(|_| ()),
            );
            log::log!(target: "voice", level, "{line}");
        };
        let mut body = json!({ "model": model, "input": text });
        if !name.is_empty() {
            body["voice"] = json!(name);
        }
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        }
        if let Some(format) = format {
            body["response_format"] = json!(format);
        }
        let mut req = self
            .http()
            .post(format!("{base}/audio/speech"))
            .timeout(SPEECH_TIMEOUT)
            .json(&body);
        if let Some(k) = &key {
            req = req.bearer_auth(k.expose());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| hide(format!("нет связи с синтезом речи: {}", e.without_url())))
            .inspect_err(|why| note(Err(why)))?;
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        // RouterAI names the generation: its cost can be asked for by this id.
        let generation = resp
            .headers()
            .get("x-generation-id")
            .and_then(|v| v.to_str().ok())
            .map(|id| id.trim().to_string());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| hide(e.without_url().to_string()))
            .inspect_err(|why| note(Err(why)))?;
        if !status.is_success() {
            let why = hide(format!(
                "синтез речи ответил {}: {}",
                status.as_u16(),
                error_message(&String::from_utf8_lossy(&bytes))
            ));
            note(Err(&why));
            return Err(why);
        }
        if bytes.is_empty() {
            note(Err("empty sound"));
            return Err("синтез речи вернул пустой звук".into());
        }
        note(Ok(bytes.len()));
        // A phrase the provider spoke is counted; one from the cache never gets here.
        self.meter_speech(&base, &model, text, generation, key.clone());
        let sound = playable(&bytes, &content_type);
        if let Some(c) = cache {
            c.put(&sound_key, &sound);
        }
        Ok(sound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_sound_gets_the_rate_its_answer_names() {
        assert_eq!(pcm_rate("audio/pcm; rate=44100; channels=1"), Some(44_100));
        assert_eq!(pcm_rate("audio/pcm;rate=24000"), Some(24_000));
        assert_eq!(pcm_rate("audio/pcm"), None);
        assert_eq!(pcm_rate("audio/pcm; rate=abc"), None);
        let rate = |wav: &[u8]| u32::from_le_bytes(wav[24..28].try_into().unwrap());
        // Fish Audio: raw samples at 44.1 kHz, the first one -1 (0xFFFF).
        let fish = playable(&[0xFF, 0xFF, 1, 0], "audio/pcm; rate=44100; channels=1");
        assert_eq!(&fish[..4], b"RIFF");
        assert_eq!(rate(&fish), 44_100);
        // RouterAI calls a WAV file audio/pcm: it is kept as it is.
        let wav = wav_from_pcm(&[1, 0], 24_000);
        assert_eq!(playable(&wav, "audio/pcm"), wav);
        assert_eq!(rate(&playable(&[1, 0, 2, 0], "audio/pcm")), 24_000);
        let mp3 = [0xFF, 0xFB, 0x90, 0x00];
        assert_eq!(playable(&mp3, "audio/mpeg"), mp3);
    }

    #[test]
    fn samples_that_start_with_minus_one_are_not_taken_for_an_mpeg_frame() {
        let rate = |wav: &[u8]| u32::from_le_bytes(wav[24..28].try_into().unwrap());
        // RouterAI labels the 24 kHz samples of Gemini TTS as plain
        // `audio/pcm`; a quiet start gives the sample -1 (0xFFFF).
        let raw = [0xFF, 0xFF, 0x01, 0x00];
        let out = playable(&raw, "audio/pcm");
        assert_eq!(&out[..4], b"RIFF");
        assert_eq!(rate(&out), 24_000);
        assert_eq!(out[44..], raw);
        assert_eq!(SoundKind::of(&out), SoundKind::Wav);
        // A real MPEG frame is still kept as it is, and so is an M4A file.
        let mp3 = [0xFF, 0xFB, 0x90, 0x00];
        assert_eq!(playable(&mp3, "audio/pcm"), mp3);
        let m4a = [0x00, 0x00, 0x00, 0x20, 0x66, 0x74, 0x79, 0x70];
        assert_eq!(playable(&m4a, "audio/mp4"), m4a);
    }

    #[test]
    fn a_recognition_failure_prints_its_reason() {
        assert_eq!(
            SttFailure::Refused("распознавание ответило 400: формат".into()).to_string(),
            "распознавание ответило 400: формат"
        );
        assert_eq!(
            SttFailure::Other("нет связи".into()).to_string(),
            "нет связи"
        );
    }

    #[test]
    fn known_sound_files_are_recognised_by_their_first_bytes() {
        assert!(has_audio_header(b"RIFF\x24\0\0\0WAVEfmt "));
        assert!(has_audio_header(b"ID3\x04\0"));
        assert!(has_audio_header(&[0xFF, 0xFB, 0x90, 0x00]));
        assert!(has_audio_header(b"OggS\0"));
        assert!(!has_audio_header(&[0x12, 0x00, 0x34, 0x00]));
        assert!(!has_audio_header(b""));
    }

    #[test]
    fn raw_samples_get_a_wav_header() {
        let wav = wav_from_pcm(&[1, 0, 2, 0], 24_000);
        assert_eq!(wav.len(), 48);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert!(has_audio_header(&wav));
    }

    #[test]
    fn transcription_text_is_trimmed_and_errors_are_plain() {
        assert_eq!(
            parse_transcription(r#"{"text":" Привет, Мика. ","usage":{"seconds":4.3}}"#),
            Ok("Привет, Мика.".into())
        );
        assert!(parse_transcription(r#"{"error":"x"}"#).is_err());
        assert!(parse_transcription("<html>").is_err());
        assert_eq!(
            error_message(r#"{"error":{"message":"voice is missing","code":400}}"#),
            "voice is missing"
        );
        assert_eq!(error_message("plain"), "plain");
    }

    #[test]
    fn segments_take_numbers_or_strings_and_skip_empty_text() {
        let body = r#"{"text":"a b","segments":[
            {"start":0.0,"end":1.5,"text":" Привет. "},
            {"start":"1.5","end":"3","text":"Как дела?"},
            {"start":3,"end":4,"text":"  "}
        ]}"#;
        assert_eq!(
            parse_segments(body),
            Ok(Some(vec![
                SpeechSegment {
                    start: 0.0,
                    end: 1.5,
                    text: "Привет.".into()
                },
                SpeechSegment {
                    start: 1.5,
                    end: 3.0,
                    text: "Как дела?".into()
                },
            ]))
        );
        assert_eq!(parse_segments(r#"{"text":"a"}"#), Ok(None));
        assert!(parse_segments("<html>").is_err());
        assert!(refuses_times(
            400,
            r#"{"error":{"message":"response_format 'verbose_json' is not supported"}}"#
        ));
        assert!(!refuses_times(401, "verbose_json"));
        assert!(!refuses_times(400, "file is too short"));
    }

    /// Phrases with times from the user's recognition model, on a WAV file
    /// named by `MIKA_STT_WAV`. Prints `None` when the model gives no times.
    ///
    /// `cargo test -p mika-providers live_transcribe_segments -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "calls the speech recognition provider: paid"]
    async fn live_transcribe_segments() {
        use mika_core::config::Config;
        use mika_core::paths::config_path;
        use mika_core::secrets::KeyringSecretStore;
        use std::sync::Arc;

        let wav = std::fs::read(std::env::var("MIKA_STT_WAV").expect("MIKA_STT_WAV"))
            .expect("the WAV file reads");
        let config = Config::load(&config_path())
            .unwrap_or_else(|e| panic!("the settings of Mika do not load: {e}"));
        let live = Live::new(
            Arc::new(Mutex::new(config)),
            Arc::new(KeyringSecretStore::new()),
        );
        let segments = live
            .transcribe_segments(&wav)
            .await
            .unwrap_or_else(|e| panic!("recognition failed: {e}"));
        println!("{segments:#?}");
    }

    /// The saved settings and keys as the app uses them, for the live tests
    /// of decision 0029; what `edit` changes stays in memory.
    fn saved_live(edit: impl FnOnce(&mut mika_core::config::Config)) -> Live {
        use mika_core::config::Config;
        use mika_core::paths::config_path;
        use mika_core::secrets::KeyringSecretStore;
        use std::sync::Arc;

        let mut config = Config::load(&config_path())
            .unwrap_or_else(|e| panic!("the settings of Mika do not load: {e}"));
        edit(&mut config);
        Live::new(
            Arc::new(Mutex::new(config)),
            Arc::new(KeyringSecretStore::new()),
        )
    }

    /// The OGG file named by `MIKA_STT_OGG`, such as a voice message saved
    /// from Telegram, goes to the user's recognition model as it is. Prints
    /// the text, or the refusal after which the app would try WAV.
    ///
    /// `cargo test -p mika-providers live_voice_message_is_heard -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "calls the speech recognition provider: paid"]
    async fn live_voice_message_is_heard() {
        let ogg = std::fs::read(std::env::var("MIKA_STT_OGG").expect("MIKA_STT_OGG"))
            .expect("the OGG file reads");
        let kind = SoundKind::of(&ogg);
        println!("sound: {kind:?}, {} bytes", ogg.len());
        match saved_live(|_| {}).transcribe_sound(&ogg, kind).await {
            Ok(text) => println!("heard: {text}"),
            Err(SttFailure::Refused(why)) => {
                println!("refused, the app would send WAV next: {why}")
            }
            Err(SttFailure::Other(why)) => panic!("recognition failed: {why}"),
        }
    }

    /// What kind of sound the user's speech model returns when MP3 is asked
    /// for: a voice message for Telegram needs MP3, M4A or OGG/Opus.
    ///
    /// `cargo test -p mika-providers live_speech_format_mp3 -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "calls the speech synthesis provider: paid"]
    async fn live_speech_format_mp3() {
        let live = saved_live(|c| c.voice.tts_cache = false);
        let sound = live
            .synthesize_format("Проверка формата.", "", "mp3")
            .await
            .unwrap_or_else(|e| panic!("speech synthesis failed: {e}"));
        let kind = SoundKind::of(&sound);
        println!("sound: {kind:?}, {} bytes", sound.len());
        println!(
            "Telegram shows it as {}",
            if kind.voice_message() {
                "a voice message"
            } else {
                "a file, unless FFmpeg turns it into OGG/Opus"
            }
        );
    }

    /// «Ну вот, всё готово.» said by Fish Audio Drama 3 with each of the
    /// eight mood directions (decision 0029), into
    /// `%TEMP%\mika-mood-<emotion>.<ext>`. Somebody has to listen: the
    /// direction must be heard in the tone and never read aloud.
    ///
    /// `cargo test -p mika-providers live_mood_directions -- --ignored --nocapture`.
    /// `MIKA_DRAMA_VOICE` names the voice, else the provider's first one is taken.
    #[tokio::test]
    #[ignore = "calls RouterAI: eight paid speech syntheses"]
    async fn live_mood_directions() {
        use mika_core::config::TtsEngine;
        use mika_core::emotion::Emotion;

        const MODEL: &str = "fish-audio/drama-3";
        let live = saved_live(|c| {
            c.voice.tts = TtsEngine::Provider;
            c.voice.tts_model = MODEL.into();
            c.voice.tts_cache = false;
            c.voice.tts_style = String::new();
        });
        let named = std::env::var("MIKA_DRAMA_VOICE")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let voice = match named {
            Some(v) => v,
            None => {
                let base = live.base(&live.speech_settings().speech_base_url);
                let key = live.key(SPEECH_SECRET_KEY);
                crate::catalog::fetch_voices(&base, key.as_ref(), MODEL)
                    .await
                    .into_iter()
                    .next()
                    .expect(
                        "the provider lists no voices for fish-audio/drama-3: \
                         set MIKA_DRAMA_VOICE to the id of a voice",
                    )
            }
        };
        println!("voice: {voice}");
        live.config().lock().voice.tts_voice = voice;

        for emotion in Emotion::ALL {
            let Some(direction) = emotion.direction() else {
                continue;
            };
            let phrase = format!("[{direction}] Ну вот, всё готово.");
            let sound = live
                .synthesize_with(&phrase, "")
                .await
                .unwrap_or_else(|e| panic!("speech synthesis failed for {direction}: {e}"));
            let path = std::env::temp_dir().join(format!(
                "mika-mood-{}.{}",
                emotion.as_str(),
                SoundKind::of(&sound).extension()
            ));
            std::fs::write(&path, &sound).expect("the sound is written to the temporary folder");
            println!("{direction}: {} ({} bytes)", path.display(), sound.len());
        }
    }

    /// The usual extension of a sound file, by its first bytes.
    fn sound_extension(sound: &[u8]) -> &'static str {
        if sound.starts_with(b"RIFF") {
            "wav"
        } else if sound.starts_with(b"OggS") {
            "ogg"
        } else if sound.starts_with(b"fLaC") {
            "flac"
        } else if has_audio_header(sound) {
            "mp3"
        } else {
            "bin"
        }
    }

    #[test]
    fn sound_files_get_their_usual_extension() {
        assert_eq!(sound_extension(&wav_from_pcm(&[1, 0], 24_000)), "wav");
        assert_eq!(sound_extension(b"ID3\x04\0"), "mp3");
        assert_eq!(sound_extension(&[0xFF, 0xFB, 0x90, 0x00]), "mp3");
        assert_eq!(sound_extension(b"OggS\0"), "ogg");
        assert_eq!(sound_extension(&[0x12, 0x00]), "bin");
    }

    /// A phrase marked up for Fish Audio Drama 3 is said by the user's speech
    /// provider and heard back by their recognition model: the marks must be
    /// performed, not read aloud (decision 0026). The saved settings and key
    /// are used as the app uses them; the changes stay in memory.
    ///
    /// `cargo test -p mika-providers live_drama -- --ignored --nocapture`.
    /// `MIKA_DRAMA_VOICE` names the voice, else the provider's first one is taken.
    #[tokio::test]
    #[ignore = "calls RouterAI: paid speech synthesis and recognition"]
    async fn live_drama_marks_are_not_spoken() {
        use mika_core::config::{Config, TtsEngine};
        use mika_core::paths::config_path;
        use mika_core::secrets::KeyringSecretStore;
        use std::sync::Arc;

        const MODEL: &str = "fish-audio/drama-3";
        const PHRASE: &str = "[sighs, tired] Ну что ж. <whisper>Иди без меня.</whisper> Я догоню.";

        let mut config = Config::load(&config_path())
            .unwrap_or_else(|e| panic!("the settings of Mika do not load: {e}"));
        config.voice.tts = TtsEngine::Provider;
        config.voice.tts_model = MODEL.into();
        config.voice.tts_cache = false;
        config.voice.tts_style = String::new();
        let live = Live::new(
            Arc::new(Mutex::new(config)),
            Arc::new(KeyringSecretStore::new()),
        );

        // The voices the settings page offers: the same address and key.
        let named = std::env::var("MIKA_DRAMA_VOICE")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let voice = match named {
            Some(v) => v,
            None => {
                let base = live.base(&live.speech_settings().speech_base_url);
                let key = live.key(SPEECH_SECRET_KEY);
                crate::catalog::fetch_voices(&base, key.as_ref(), MODEL)
                    .await
                    .into_iter()
                    .next()
                    .expect(
                        "the provider lists no voices for fish-audio/drama-3: \
                         set MIKA_DRAMA_VOICE to the id of a voice",
                    )
            }
        };
        println!("voice: {voice}");
        live.config().lock().voice.tts_voice = voice;

        let sound = live
            .synthesize_with(PHRASE, "")
            .await
            .unwrap_or_else(|e| panic!("speech synthesis failed: {e}"));
        let ext = sound_extension(&sound);
        let path = std::env::temp_dir().join(format!("mika-drama-live.{ext}"));
        std::fs::write(&path, &sound).expect("the sound is written to the temporary folder");
        println!("sound: {} ({} bytes)", path.display(), sound.len());

        assert!(
            !live.speech_settings().stt_model.trim().is_empty(),
            "no recognition model in the settings: listen to the file instead"
        );
        let heard = match live.transcribe(&sound).await {
            Ok(heard) => heard,
            // The recognizer is sent a WAV file; another sound may not suit it.
            Err(e) if ext != "wav" => {
                println!(
                    "note: the recognizer did not take the {ext} sound ({e}); \
                     listen to the file instead"
                );
                return;
            }
            Err(e) => panic!("speech recognition failed: {e}"),
        };
        println!("heard: {heard}");
        let heard = heard.to_lowercase().replace('ё', "е");
        assert!(
            heard.contains("без меня"),
            "the words are not heard: {heard}"
        );
        for mark in ["whisper", "виспер", "sigh", "сай", "tired"] {
            assert!(
                !heard.contains(mark),
                "the mark {mark:?} is read aloud: {heard}"
            );
        }
    }
}
