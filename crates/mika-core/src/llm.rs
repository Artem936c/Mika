//! Provider-neutral LLM types.

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// PNG pictures in base64, for models that can see.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

impl ChatMessage {
    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            images: Vec::new(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::plain(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::plain(Role::User, content)
    }

    /// A user message with one PNG picture in base64.
    pub fn user_with_image(content: impl Into<String>, png_base64: impl Into<String>) -> Self {
        Self {
            images: vec![png_base64.into()],
            ..Self::plain(Role::User, content)
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::plain(Role::Assistant, content)
    }

    pub fn assistant_with_calls(content: impl Into<String>, calls: Vec<ToolCall>) -> Self {
        Self {
            tool_calls: calls,
            ..Self::plain(Role::Assistant, content)
        }
    }

    pub fn tool_result(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(id.into()),
            ..Self::plain(Role::Tool, content)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmChunk {
    Text(String),
    ToolCall(ToolCall),
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LlmError {
    #[error("нет связи с провайдером: {0}")]
    Network(String),
    #[error("провайдер ответил {status}: {body}")]
    Http { status: u16, body: String },
    #[error("непонятный ответ провайдера: {0}")]
    Protocol(String),
    #[error("остановлено")]
    Cancelled,
    #[error("провайдер не настроен: {0}")]
    NotConfigured(String),
}

pub type LlmStream = BoxStream<'static, Result<LlmChunk, LlmError>>;

/// A model that can look at a picture.
#[async_trait]
pub trait Vision: Send + Sync {
    async fn ask(&self, png: &[u8], question: &str) -> Result<String, String>;
}

/// Speech recognition. Takes a WAV file, returns the text that was said.
#[async_trait]
pub trait SpeechToText: Send + Sync {
    async fn transcribe(&self, wav: &[u8]) -> Result<String, String>;
}

/// Speech synthesis. Returns an audio file: WAV, MP3 or Ogg.
#[async_trait]
pub trait TextToSpeech: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>, String>;
}

/// A model that draws pictures. Returns PNG, JPEG or WebP bytes.
#[async_trait]
pub trait ImageGen: Send + Sync {
    async fn generate(&self, prompt: &str) -> Result<Vec<u8>, String>;
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError>;
}

/// One short answer without tools: the whole text the model streams back,
/// trimmed. `None` on an error, an empty answer or after `timeout`.
pub async fn complete(
    provider: &dyn LlmProvider,
    system: &str,
    user: &str,
    timeout: std::time::Duration,
) -> Option<String> {
    use futures::StreamExt;
    let req = LlmRequest {
        messages: vec![ChatMessage::system(system), ChatMessage::user(user)],
        tools: Vec::new(),
    };
    let collect = async {
        let mut stream = provider.stream(req).await.ok()?;
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(LlmChunk::Text(t)) => text.push_str(&t),
                Ok(LlmChunk::ToolCall(_)) => {}
                Err(_) => return None,
            }
        }
        let text = text.trim().to_string();
        (!text.is_empty()).then_some(text)
    };
    tokio::time::timeout(timeout, collect).await.ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message_serde_roundtrip() {
        let msgs = vec![
            ChatMessage::user("привет"),
            ChatMessage::assistant_with_calls(
                "",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "get_time".into(),
                    arguments: serde_json::json!({}),
                }],
            ),
            ChatMessage::tool_result("c1", "12:00"),
        ];
        let json = serde_json::to_string(&msgs).unwrap();
        let back: Vec<ChatMessage> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msgs);
        assert!(!json.contains("tool_call_id\":null"));
    }
}
