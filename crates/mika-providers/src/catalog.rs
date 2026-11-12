//! What a provider offers: its model list with prices and abilities, and voices.
//!
//! OpenAI-compatible providers answer `GET /models`. Aggregators such as RouterAI
//! and OpenRouter add modalities, context length, prices and a description; plain
//! OpenAI gives only ids. Anthropic has `GET /v1/models`. Voices have no common
//! endpoint: `GET /audio/voices` is asked, and well-known model families have a
//! built-in list.

use mika_core::secrets::Secret;
use serde::Serialize;
use serde_json::Value;

/// What a model is picked for on the settings pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Chat,
    Vision,
    Image,
    Embedding,
    Speech,
    Transcription,
}

impl Purpose {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "chat" => Purpose::Chat,
            "vision" => Purpose::Vision,
            "image" => Purpose::Image,
            "embedding" => Purpose::Embedding,
            "tts" => Purpose::Speech,
            "stt" => Purpose::Transcription,
            _ => return None,
        })
    }
}

/// One price line: `value` money per `per`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Price {
    /// `prompt`, `completion`, `image_output`, `seconds`, `request`…
    pub kind: String,
    pub value: f64,
    /// `1M tokens`, `image`, `second`, `request`.
    pub per: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub context: Option<u64>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub prices: Vec<Price>,
    /// The model takes tool calls: an agent needs that.
    pub tools: bool,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_lowercase)
                .collect()
        })
        .unwrap_or_default()
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Prices worth showing, in a stable order. Per-token prices become per million tokens.
fn prices(m: &Value) -> Vec<Price> {
    const ORDER: &[&str] = &[
        "prompt",
        "completion",
        "image_output",
        "image",
        "seconds",
        "request",
    ];
    let Some(p) = m.get("pricing").and_then(Value::as_object) else {
        return Vec::new();
    };
    let units = m.get("pricing_units").and_then(Value::as_object);
    ORDER
        .iter()
        .filter_map(|k| {
            let value = number(p.get(*k)?)?;
            if value <= 0.0 {
                return None;
            }
            let unit = units
                .and_then(|u| u.get(*k))
                .and_then(Value::as_str)
                .unwrap_or(match *k {
                    "image_output" | "image" => "image",
                    "seconds" => "second",
                    "request" => "request",
                    _ => "token",
                });
            Some(if unit == "token" {
                Price {
                    kind: k.to_string(),
                    value: value * 1_000_000.0,
                    per: "1M tokens".into(),
                }
            } else {
                Price {
                    kind: k.to_string(),
                    value,
                    per: unit.to_string(),
                }
            })
        })
        .collect()
}

/// Models from a `/models` answer of any of the known shapes.
pub fn parse_models(v: &Value) -> Vec<ModelInfo> {
    let list = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(Value::as_array)
        .or_else(|| v.as_array());
    let Some(list) = list else {
        return Vec::new();
    };
    let mut out: Vec<ModelInfo> = list
        .iter()
        .filter_map(|m| {
            let id = m
                .get("id")
                .or_else(|| m.get("name"))
                .or_else(|| m.get("model"))
                .and_then(Value::as_str)?
                .to_string();
            let arch = m.get("architecture");
            let name = m
                .get("name")
                .or_else(|| m.get("display_name"))
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            Some(ModelInfo {
                description: m
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                context: m
                    .get("context_length")
                    .and_then(Value::as_u64)
                    .filter(|c| *c > 0),
                inputs: strings(arch.and_then(|a| a.get("input_modalities"))),
                outputs: strings(arch.and_then(|a| a.get("output_modalities"))),
                prices: prices(m),
                tools: strings(m.get("supported_parameters"))
                    .iter()
                    .any(|p| p == "tools"),
                name,
                id,
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Whether a model suits `purpose`. A list without modalities (plain OpenAI) cannot
/// be judged by them, so the id decides, and anything unclear stays in the list.
pub fn fits(m: &ModelInfo, purpose: Purpose) -> bool {
    let has = |list: &[String], what: &str| list.iter().any(|x| x == what);
    if m.inputs.is_empty() && m.outputs.is_empty() {
        let id = m.id.to_lowercase();
        let speechy = id.contains("tts") || id.contains("speech");
        let hearing = id.contains("whisper") || id.contains("transcri") || id.contains("asr");
        let embedding = id.contains("embed");
        let image = id.contains("dall-e") || id.contains("image") || id.contains("flux");
        return match purpose {
            Purpose::Speech => speechy,
            Purpose::Transcription => hearing,
            Purpose::Embedding => embedding,
            Purpose::Image => image,
            Purpose::Chat | Purpose::Vision => !(speechy || hearing || embedding || image),
        };
    }
    match purpose {
        Purpose::Chat => has(&m.inputs, "text") && has(&m.outputs, "text"),
        Purpose::Vision => has(&m.inputs, "image") && has(&m.outputs, "text"),
        Purpose::Image => has(&m.outputs, "image"),
        Purpose::Embedding => has(&m.outputs, "embeddings"),
        Purpose::Speech => has(&m.outputs, "speech") || has(&m.outputs, "audio"),
        // Chat models that hear audio do not serve `/audio/transcriptions`:
        // only speech-to-text models do.
        Purpose::Transcription => {
            let id = m.id.to_lowercase();
            has(&m.outputs, "transcription")
                || (has(&m.inputs, "audio")
                    && (id.contains("whisper") || id.contains("transcri") || id.contains("asr")))
        }
    }
}

/// Voices from an answer of `GET /audio/voices`: a list of names or of objects.
pub fn parse_voices(v: &Value) -> Vec<String> {
    let list = v
        .get("voices")
        .or_else(|| v.get("data"))
        .and_then(Value::as_array)
        .or_else(|| v.as_array());
    let mut out: Vec<String> = list
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.as_str()
                        .or_else(|| x.get("voice_id").and_then(Value::as_str))
                        .or_else(|| x.get("id").and_then(Value::as_str))
                        .or_else(|| x.get("name").and_then(Value::as_str))
                        .map(str::to_string)
                })
                .filter(|s| !s.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    out.dedup();
    out
}

const GEMINI_VOICES: &[&str] = &[
    "Zephyr",
    "Puck",
    "Charon",
    "Kore",
    "Fenrir",
    "Leda",
    "Orus",
    "Aoede",
    "Callirrhoe",
    "Autonoe",
    "Enceladus",
    "Iapetus",
    "Umbriel",
    "Algieba",
    "Despina",
    "Erinome",
    "Algenib",
    "Rasalgethi",
    "Laomedeia",
    "Achernar",
    "Alnilam",
    "Schedar",
    "Gacrux",
    "Pulcherrima",
    "Achird",
    "Zubenelgenubi",
    "Vindemiatrix",
    "Sadachbia",
    "Sadaltager",
    "Sulafat",
];
const OPENAI_TTS1_VOICES: &[&str] = &[
    "alloy", "ash", "coral", "echo", "fable", "onyx", "nova", "sage", "shimmer",
];
const OPENAI_VOICES: &[&str] = &[
    "alloy", "ash", "ballad", "coral", "echo", "fable", "nova", "onyx", "sage", "shimmer", "verse",
];

/// Voices documented for a model family, for providers that do not list them.
pub fn known_voices(model: &str) -> Option<&'static [&'static str]> {
    let m = model.to_lowercase();
    if m.contains("gemini") && m.contains("tts") {
        Some(GEMINI_VOICES)
    } else if m.contains("tts-1") {
        Some(OPENAI_TTS1_VOICES)
    } else if m.contains("gpt") && m.contains("tts") {
        Some(OPENAI_VOICES)
    } else {
        None
    }
}

/// Currency sign of a provider's prices, when it is known by its address.
pub fn currency(base_url: &str) -> &'static str {
    let b = base_url.to_lowercase();
    if b.contains("routerai.ru") {
        "₽"
    } else if b.contains("openrouter.ai")
        || b.contains("api.openai.com")
        || b.contains("anthropic.com")
    {
        "$"
    } else {
        ""
    }
}

async fn get_json(url: &str, key: Option<&Secret>, anthropic: bool) -> Result<Value, String> {
    let mut req = crate::common::client()
        .get(url)
        .timeout(std::time::Duration::from_secs(30));
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        req = if anthropic {
            req.header("x-api-key", k.expose())
                .header("anthropic-version", "2023-06-01")
        } else {
            req.bearer_auth(k.expose())
        };
    }
    let resp = req
        .send()
        .await
        .map_err(|e| crate::redact(&e.without_url().to_string(), key))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("провайдер ответил {}", status.as_u16()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("непонятный ответ провайдера: {}", e.without_url()))
}

/// The provider's model list. `base` is the API address without the trailing slash.
pub async fn fetch_models(
    base: &str,
    key: Option<&Secret>,
    anthropic: bool,
) -> Result<Vec<ModelInfo>, String> {
    let url = if anthropic {
        format!("{base}/v1/models?limit=1000")
    } else {
        format!("{base}/models")
    };
    let v = get_json(&url, key, anthropic).await?;
    let models = parse_models(&v);
    if models.is_empty() {
        return Err("провайдер не отдал список моделей".into());
    }
    Ok(models)
}

/// Voices the provider lists for `model`, if it has such an endpoint.
pub async fn fetch_voices(base: &str, key: Option<&Secret>, model: &str) -> Vec<String> {
    let model_q: String = model
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    for url in [
        format!("{base}/audio/voices?model={model_q}"),
        format!("{base}/audio/voices"),
    ] {
        if let Ok(v) = get_json(&url, key, false).await {
            let voices = parse_voices(&v);
            if !voices.is_empty() {
                return voices;
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn routerai() -> Value {
        json!({"data": [
            {"id": "openai/gpt-oss-20b", "name": "OpenAI: gpt-oss-20b", "description": "MoE",
             "context_length": 131072,
             "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]},
             "pricing": {"prompt": 2.68e-06, "completion": 3.97e-06},
             "pricing_units": {"prompt": "token", "completion": "token"},
             "supported_parameters": ["max_tokens", "tools"]},
            {"id": "google/gemini-3.8-flash-lite-tts", "name": "Gemini TTS",
             "architecture": {"input_modalities": ["text"], "output_modalities": ["speech"]},
             "pricing": {"prompt": "0.00005", "completion": 0.0006}},
            {"id": "qwen/qwen3-asr-0.6b", "context_length": 0,
             "architecture": {"input_modalities": ["audio"], "output_modalities": ["transcription"]},
             "pricing": {"seconds": 0.00036, "completion": 0.0},
             "pricing_units": {"seconds": "second"}},
            {"id": "black-forest-labs/flux.2-klein-4b",
             "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["image"]},
             "pricing": {"image_output": 0.37}},
            {"id": "google/gemma-4-26b-a4b-it",
             "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]}},
            {"id": "perplexity/pplx-embed-v1-0.6b",
             "architecture": {"input_modalities": ["text"], "output_modalities": ["embeddings"]}},
            {"id": "google/gemini-2.5-flash-lite",
             "architecture": {"input_modalities": ["text", "audio"], "output_modalities": ["text"]}}
        ]})
    }

    #[test]
    fn aggregator_list_gives_abilities_and_prices() {
        let ms = parse_models(&routerai());
        assert_eq!(ms.len(), 7);
        let chat = ms.iter().find(|m| m.id == "openai/gpt-oss-20b").unwrap();
        assert_eq!(chat.context, Some(131072));
        assert!(chat.tools);
        assert_eq!(chat.prices[0].per, "1M tokens");
        assert!((chat.prices[0].value - 2.68).abs() < 1e-9);
        let asr = ms.iter().find(|m| m.id == "qwen/qwen3-asr-0.6b").unwrap();
        assert_eq!(asr.context, None);
        assert_eq!(
            asr.prices,
            vec![Price {
                kind: "seconds".into(),
                value: 0.00036,
                per: "second".into()
            }]
        );
        let tts = ms.iter().find(|m| m.id.ends_with("tts")).unwrap();
        assert!(
            (tts.prices[0].value - 50.0).abs() < 1e-6,
            "string prices are read"
        );
    }

    #[test]
    fn purposes_pick_the_right_models() {
        let ms = parse_models(&routerai());
        let ids = |p: Purpose| -> Vec<String> {
            ms.iter()
                .filter(|m| fits(m, p))
                .map(|m| m.id.clone())
                .collect()
        };
        assert_eq!(
            ids(Purpose::Chat),
            [
                "google/gemini-2.5-flash-lite",
                "google/gemma-4-26b-a4b-it",
                "openai/gpt-oss-20b"
            ]
        );
        assert_eq!(ids(Purpose::Vision), ["google/gemma-4-26b-a4b-it"]);
        assert_eq!(ids(Purpose::Image), ["black-forest-labs/flux.2-klein-4b"]);
        assert_eq!(ids(Purpose::Embedding), ["perplexity/pplx-embed-v1-0.6b"]);
        assert_eq!(ids(Purpose::Speech), ["google/gemini-3.8-flash-lite-tts"]);
        assert_eq!(ids(Purpose::Transcription), ["qwen/qwen3-asr-0.6b"]);
    }

    #[test]
    fn plain_openai_and_anthropic_lists_are_judged_by_id() {
        let openai = parse_models(&json!({"object": "list", "data": [
            {"id": "gpt-4o-mini"}, {"id": "tts-1"}, {"id": "whisper-1"},
            {"id": "text-embedding-3-small"}, {"id": "dall-e-3"}
        ]}));
        let pick = |p| -> Vec<&str> {
            openai
                .iter()
                .filter(|m| fits(m, p))
                .map(|m| m.id.as_str())
                .collect()
        };
        assert_eq!(pick(Purpose::Chat), ["gpt-4o-mini"]);
        assert_eq!(pick(Purpose::Speech), ["tts-1"]);
        assert_eq!(pick(Purpose::Transcription), ["whisper-1"]);
        assert_eq!(pick(Purpose::Embedding), ["text-embedding-3-small"]);
        assert_eq!(pick(Purpose::Image), ["dall-e-3"]);
        let anthropic = parse_models(&json!({"data": [
            {"id": "claude-x", "display_name": "Claude X", "type": "model"}
        ]}));
        assert_eq!(anthropic[0].name, "Claude X");
        assert!(parse_models(&json!({"error": "no"})).is_empty());
    }

    #[test]
    fn voices_from_provider_and_known_families() {
        assert_eq!(
            parse_voices(&json!({"voices": ["af_bella", "am_adam"]})),
            ["af_bella", "am_adam"]
        );
        assert_eq!(
            parse_voices(
                &json!({"voices": [{"voice_id": "v1", "name": "Rachel"}, {"name": "Adam"}]})
            ),
            ["v1", "Adam"]
        );
        assert!(parse_voices(&json!({"data": []})).is_empty());
        let g = known_voices("google/gemini-3.8-flash-lite-tts").unwrap();
        assert_eq!(g.len(), 30);
        assert!(g.contains(&"Kore"));
        assert_eq!(known_voices("tts-1-hd").unwrap().len(), 9);
        assert!(known_voices("openai/gpt-4o-mini-tts")
            .unwrap()
            .contains(&"verse"));
        assert!(known_voices("qwen/qwen3-asr").is_none());
        assert_eq!(currency("https://routerai.ru/api/v1"), "₽");
        assert_eq!(currency("http://localhost:11434/v1"), "");
    }
}
