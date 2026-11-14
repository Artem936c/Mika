//! Builds the configured provider and measures it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use mika_core::config::{LlmConfig, ProviderKind};
use mika_core::llm::{ChatMessage, LlmError, LlmProvider, LlmRequest};
use mika_core::secrets::{Secret, SecretStore};

use crate::anthropic::AnthropicProvider;
use crate::openai::OpenAiProvider;

const MAX_BODY: usize = 300;

/// Cuts a server answer to 300 characters and hides the key if the server echoed it.
pub fn redact(body: &str, secret: Option<&Secret>) -> String {
    let mut text = body.trim().to_string();
    if let Some(s) = secret.filter(|s| !s.is_empty()) {
        text = text.replace(s.expose(), "***");
    }
    if text.chars().count() > MAX_BODY {
        text = text.chars().take(MAX_BODY).collect::<String>() + "…";
    }
    text
}

pub fn build_provider(
    cfg: &LlmConfig,
    secrets: &dyn SecretStore,
) -> Result<Arc<dyn LlmProvider>, LlmError> {
    let key = secrets
        .get(&cfg.kind.secret_key())
        .map_err(|e| LlmError::NotConfigured(e.to_string()))?;
    let url = cfg.effective_base_url();
    if cfg.model.trim().is_empty() {
        return Err(LlmError::NotConfigured("не указана модель".into()));
    }
    let model = cfg.model.trim().to_string();
    Ok(match cfg.kind {
        ProviderKind::Openai => Arc::new(OpenAiProvider::new(url, model, key)),
        ProviderKind::Ollama => Arc::new(OpenAiProvider::new(url, model, key).named("ollama")),
        ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(url, model, key)),
    })
}

/// Stands in when the provider cannot be built, so the chat explains what to fix.
pub struct UnconfiguredProvider {
    reason: String,
}

#[async_trait::async_trait]
impl LlmProvider for UnconfiguredProvider {
    fn name(&self) -> &str {
        "unconfigured"
    }

    async fn stream(&self, _req: LlmRequest) -> Result<mika_core::llm::LlmStream, LlmError> {
        Err(LlmError::NotConfigured(self.reason.clone()))
    }
}

/// Like `build_provider`, but never fails.
pub fn build_or_placeholder(cfg: &LlmConfig, secrets: &dyn SecretStore) -> Arc<dyn LlmProvider> {
    build_provider(cfg, secrets).unwrap_or_else(|e| {
        log::warn!(
            "the model provider is not ready, the chat will say what to fix: kind={:?} error={e}",
            cfg.kind
        );
        Arc::new(UnconfiguredProvider {
            reason: format!("{e}. Открой настройки и проверь провайдера"),
        })
    })
}

/// Time to the first chunk of a tiny request.
pub async fn probe(provider: &dyn LlmProvider) -> Result<Duration, LlmError> {
    let start = Instant::now();
    let mut stream = provider
        .stream(LlmRequest {
            messages: vec![ChatMessage::user("Ответь одним словом: ок")],
            tools: Vec::new(),
        })
        .await?;
    match stream.next().await {
        Some(Ok(_)) => Ok(start.elapsed()),
        Some(Err(e)) => Err(e),
        None => Err(LlmError::Protocol("пустой ответ".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::secrets::MemorySecretStore;

    #[test]
    fn redact_hides_key_and_cuts_long_body() {
        let key = Secret::new("sk-secret");
        assert_eq!(redact("bad key sk-secret", Some(&key)), "bad key ***");
        let long = "x".repeat(400);
        assert_eq!(redact(&long, None).chars().count(), 301);
    }

    #[test]
    fn build_provider_requires_model() {
        let cfg = LlmConfig {
            model: " ".into(),
            ..LlmConfig::default()
        };
        assert!(matches!(
            build_provider(&cfg, &MemorySecretStore::default()),
            Err(LlmError::NotConfigured(_))
        ));
    }

    #[test]
    fn build_provider_names_ollama() {
        let cfg = LlmConfig {
            kind: ProviderKind::Ollama,
            model: "qwen3".into(),
            ..LlmConfig::default()
        };
        let p = build_provider(&cfg, &MemorySecretStore::default()).unwrap();
        assert_eq!(p.name(), "ollama");
    }
}
