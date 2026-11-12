//! Embeddings, vision and pictures that follow the settings as they change.
//!
//! Every call reads the current config and keys, so a change in settings
//! takes effect without a restart.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use futures::StreamExt;
use mika_core::config::{Config, EMBEDDING_SECRET_KEY, IMAGE_SECRET_KEY};
use mika_core::llm::{ChatMessage, ImageGen, LlmChunk, LlmRequest, Vision};
use mika_core::memory::Embedder;
use mika_core::secrets::{Secret, SecretStore};
use mika_core::speech_cache::SpeechCache;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::common::client;
use crate::factory::{build_provider, redact};

/// Longest wait for a picture or a batch of vectors.
const SLOW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

#[derive(Clone)]
pub struct Live {
    config: Arc<Mutex<Config>>,
    secrets: Arc<dyn SecretStore>,
    http: reqwest::Client,
    speech_cache: Option<Arc<SpeechCache>>,
}

fn base64_engine() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

impl Live {
    pub fn new(config: Arc<Mutex<Config>>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            config,
            secrets,
            http: client(),
            speech_cache: None,
        }
    }

    /// Keeps synthesized phrases in `cache`, while the settings allow it.
    pub fn with_speech_cache(mut self, cache: Arc<SpeechCache>) -> Self {
        self.speech_cache = Some(cache);
        self
    }

    pub(crate) fn speech_cache(&self) -> Option<&SpeechCache> {
        self.speech_cache.as_deref()
    }

    /// The key stored under `own`, or the LLM key when there is none.
    pub(crate) fn key(&self, own: &str) -> Option<Secret> {
        let llm = self.config.lock().llm.kind.secret_key();
        [own, llm.as_str()]
            .iter()
            .find_map(|k| self.secrets.get(k).ok().flatten().filter(|s| !s.is_empty()))
    }

    pub(crate) fn config(&self) -> &Arc<Mutex<Config>> {
        &self.config
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// `base_url` of a section, or the LLM address when it is empty.
    pub(crate) fn base(&self, own: &str) -> String {
        let own = own.trim().trim_end_matches('/');
        if own.is_empty() {
            self.config.lock().llm.effective_base_url()
        } else {
            own.to_string()
        }
    }

    async fn post(&self, url: String, key: Option<Secret>, body: Value) -> Result<Value, String> {
        let mut req = self.http.post(url).timeout(SLOW_TIMEOUT).json(&body);
        if let Some(k) = &key {
            req = req.bearer_auth(k.expose());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| redact(&e.without_url().to_string(), key.as_ref()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| redact(&e.without_url().to_string(), key.as_ref()))?;
        if !status.is_success() {
            return Err(format!(
                "провайдер ответил {}: {}",
                status.as_u16(),
                redact(&text, key.as_ref())
            ));
        }
        serde_json::from_str(&text).map_err(|e| format!("непонятный ответ провайдера: {e}"))
    }
}

/// Vectors from an OpenAI-style `/embeddings` answer, in input order.
pub fn parse_embeddings(v: &Value, expected: usize) -> Result<Vec<Vec<f32>>, String> {
    let data = v["data"]
        .as_array()
        .ok_or("в ответе модели эмбеддингов нет поля data")?;
    let mut rows: Vec<(u64, Vec<f32>)> = data
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let vector: Vec<f32> = d["embedding"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_f64)
                        .map(|x| x as f32)
                        .collect()
                })
                .unwrap_or_default();
            (d["index"].as_u64().unwrap_or(i as u64), vector)
        })
        .collect();
    rows.sort_by_key(|(i, _)| *i);
    if rows.len() != expected || rows.iter().any(|(_, v)| v.is_empty()) {
        return Err(format!(
            "модель эмбеддингов вернула {} векторов вместо {expected}",
            rows.len()
        ));
    }
    Ok(rows.into_iter().map(|(_, v)| v).collect())
}

#[async_trait]
impl Embedder for Live {
    fn model(&self) -> Option<String> {
        let m = self.config.lock().embedding.model.trim().to_string();
        (!m.is_empty()).then_some(m)
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let (model, base) = {
            let c = self.config.lock();
            (
                c.embedding.model.trim().to_string(),
                c.embedding.base_url.clone(),
            )
        };
        if model.is_empty() {
            return Err("модель эмбеддингов не задана".into());
        }
        let v = self
            .post(
                format!("{}/embeddings", self.base(&base)),
                self.key(EMBEDDING_SECRET_KEY),
                json!({ "model": model, "input": texts }),
            )
            .await?;
        parse_embeddings(&v, texts.len())
    }
}

#[async_trait]
impl Vision for Live {
    async fn ask(&self, png: &[u8], question: &str) -> Result<String, String> {
        let mut cfg = self.config.lock().llm.clone();
        if !cfg.vision_model.trim().is_empty() {
            cfg.model = cfg.vision_model.trim().to_string();
        }
        let provider = build_provider(&cfg, self.secrets.as_ref()).map_err(|e| e.to_string())?;
        let req = LlmRequest {
            messages: vec![
                ChatMessage::system(
                    "Ты смотришь на снимок экрана компьютера. Отвечай по-русски, коротко и точно, только о том, что видно.",
                ),
                ChatMessage::user_with_image(question, base64_engine().encode(png)),
            ],
            tools: Vec::new(),
        };
        let mut stream = provider.stream(req).await.map_err(|e| e.to_string())?;
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            if let LlmChunk::Text(t) = chunk.map_err(|e| e.to_string())? {
                text.push_str(&t);
            }
        }
        if text.trim().is_empty() {
            return Err(format!(
                "модель {} ничего не ответила про картинку: возможно, она не умеет смотреть. Модель для зрения задаётся в настройках провайдера",
                cfg.model
            ));
        }
        Ok(text)
    }
}

#[async_trait]
impl ImageGen for Live {
    async fn generate(&self, prompt: &str) -> Result<Vec<u8>, String> {
        let (model, base) = {
            let c = self.config.lock();
            (c.image.model.trim().to_string(), c.image.base_url.clone())
        };
        if model.is_empty() {
            return Err(
                "модель для картинок не задана: укажи её в настройках, раздел «Провайдеры»".into(),
            );
        }
        let key = self.key(IMAGE_SECRET_KEY);
        let v = self
            .post(
                format!("{}/images/generations", self.base(&base)),
                key.clone(),
                json!({ "model": model, "prompt": prompt, "n": 1 }),
            )
            .await?;
        let first = &v["data"][0];
        if let Some(b64) = first["b64_json"].as_str() {
            return base64_engine()
                .decode(b64.trim())
                .map_err(|e| format!("картинка пришла повреждённой: {e}"));
        }
        let url = first["url"]
            .as_str()
            .ok_or("в ответе провайдера нет картинки")?;
        if let Some(data) = url.strip_prefix("data:") {
            let b64 = data.split_once("base64,").map(|(_, b)| b).unwrap_or("");
            return base64_engine()
                .decode(b64.trim())
                .map_err(|e| format!("картинка пришла повреждённой: {e}"));
        }
        let resp = self
            .http
            .get(url)
            .timeout(SLOW_TIMEOUT)
            .send()
            .await
            .map_err(|e| redact(&e.without_url().to_string(), key.as_ref()))?;
        if !resp.status().is_success() {
            return Err(format!(
                "картинка не скачалась: ответ {}",
                resp.status().as_u16()
            ));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| redact(&e.without_url().to_string(), key.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::secrets::MemorySecretStore;

    fn live(config: Config, secrets: MemorySecretStore) -> Live {
        Live::new(Arc::new(Mutex::new(config)), Arc::new(secrets))
    }

    #[test]
    fn embeddings_are_sorted_by_index_and_checked() {
        let v = json!({"data": [
            {"index": 1, "embedding": [0.5, 1.0]},
            {"index": 0, "embedding": [2.0, 3.0]}
        ]});
        assert_eq!(
            parse_embeddings(&v, 2),
            Ok(vec![vec![2.0, 3.0], vec![0.5, 1.0]])
        );
        assert!(parse_embeddings(&v, 3).is_err());
        assert!(parse_embeddings(&json!({"error": "x"}), 1).is_err());
        assert!(parse_embeddings(&json!({"data": [{"embedding": []}]}), 1).is_err());
    }

    #[test]
    fn embedder_is_off_without_a_model() {
        let mut c = Config::default();
        assert_eq!(live(c.clone(), MemorySecretStore::default()).model(), None);
        c.embedding.model = " text-embedding-3-small ".into();
        assert_eq!(
            live(c, MemorySecretStore::default()).model(),
            Some("text-embedding-3-small".into())
        );
    }

    #[test]
    fn own_key_wins_and_llm_key_is_the_fallback() {
        let secrets = MemorySecretStore::default();
        secrets
            .set("llm.openai.api_key", &Secret::new("llm-key"))
            .unwrap();
        let l = live(Config::default(), secrets);
        assert_eq!(l.key(IMAGE_SECRET_KEY).unwrap().expose(), "llm-key");
        l.secrets
            .set(IMAGE_SECRET_KEY, &Secret::new("image-key"))
            .unwrap();
        assert_eq!(l.key(IMAGE_SECRET_KEY).unwrap().expose(), "image-key");
        assert_eq!(l.key(EMBEDDING_SECRET_KEY).unwrap().expose(), "llm-key");
    }

    #[test]
    fn empty_section_address_falls_back_to_the_llm_address() {
        let mut c = Config::default();
        c.llm.base_url = "https://example.test/api/v1/".into();
        let l = live(c, MemorySecretStore::default());
        assert_eq!(l.base(""), "https://example.test/api/v1");
        assert_eq!(l.base(" https://img.test/v1/ "), "https://img.test/v1");
    }

    #[tokio::test]
    async fn picture_and_embedding_need_a_model() {
        let l = live(Config::default(), MemorySecretStore::default());
        assert!(l.generate("кот").await.unwrap_err().contains("не задана"));
        assert!(l.embed(&["x".into()]).await.is_err());
    }
}
