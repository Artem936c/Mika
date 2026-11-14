//! A text model for tools: one request without tools, to the address and with
//! the key of the LLM.

use async_trait::async_trait;
use mika_core::llm::{complete_capped, TextAnswer, TextFailure, TextModel};

use crate::factory::build_provider;
use crate::live::Live;

#[async_trait]
impl TextModel for Live {
    fn model_id(&self, model: &str) -> String {
        let model = model.trim();
        if model.is_empty() {
            self.config().lock().llm.model.trim().to_string()
        } else {
            model.to_string()
        }
    }

    async fn complete(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_chars: usize,
    ) -> Result<TextAnswer, TextFailure> {
        let mut cfg = self.config().lock().llm.clone();
        cfg.model = self.model_id(model);
        let provider =
            build_provider(&cfg, self.secrets()).map_err(|e| TextFailure::new(e.to_string()))?;
        complete_capped(provider.as_ref(), system, user, max_chars).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mika_core::config::Config;
    use mika_core::secrets::MemorySecretStore;
    use parking_lot::Mutex;

    use super::*;

    fn live(config: Config) -> Live {
        Live::new(
            Arc::new(Mutex::new(config)),
            Arc::new(MemorySecretStore::default()),
        )
    }

    #[test]
    fn empty_model_means_the_chat_model() {
        let mut c = Config::default();
        c.llm.model = " gpt-4o-mini ".into();
        let l = live(c);
        assert_eq!(l.model_id(""), "gpt-4o-mini");
        assert_eq!(l.model_id("   "), "gpt-4o-mini");
        assert_eq!(
            l.model_id(" mistralai/mistral-nemo "),
            "mistralai/mistral-nemo"
        );
    }

    #[tokio::test]
    async fn no_model_at_all_is_an_error_before_any_request() {
        let mut c = Config::default();
        c.llm.model = " ".into();
        let l = live(c);
        let err = l.complete("", "система", "вопрос", 100).await.unwrap_err();
        assert!(err.reason.contains("не указана модель"), "{err}");
        assert_eq!(err.streamed, 0);
    }
}
