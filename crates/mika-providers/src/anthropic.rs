//! Anthropic Messages API with streaming.

use std::collections::BTreeMap;

use async_trait::async_trait;
use mika_core::llm::{LlmChunk, LlmError, LlmProvider, LlmRequest, LlmStream, Role, ToolCall};
use mika_core::secrets::Secret;
use serde_json::{json, Value};

use crate::common::{client, send, sse_stream, Step};
use crate::metering::{request_kind, StreamMeter};
use crate::trace::RequestLog;

pub const API_VERSION: &str = "2023-06-01";
pub const MAX_TOKENS: u32 = 4096;

pub struct AnthropicProvider {
    base_url: String,
    model: String,
    key: Option<Secret>,
    http: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(base_url: String, model: String, api_key: Option<Secret>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            key: api_key.filter(|k| !k.is_empty()),
            http: client(),
        }
    }
}

/// Anthropic wants alternating user/assistant turns, the system prompt apart,
/// and tool results as `tool_result` blocks inside a user turn.
pub fn request_body(model: &str, req: &LlmRequest) -> Value {
    let system: Vec<&str> = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect();

    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for m in req.messages.iter().filter(|m| m.role != Role::System) {
        let (role, blocks) = match m.role {
            Role::User => {
                let mut b = vec![json!({ "type": "text", "text": m.content })];
                for png in &m.images {
                    b.push(json!({
                        "type": "image",
                        "source": { "type": "base64", "media_type": "image/png", "data": png },
                    }));
                }
                ("user", b)
            }
            Role::Tool => (
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                })],
            ),
            Role::Assistant => {
                let mut b = Vec::new();
                if !m.content.is_empty() {
                    b.push(json!({ "type": "text", "text": m.content }));
                }
                for c in &m.tool_calls {
                    b.push(json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.arguments }));
                }
                ("assistant", b)
            }
            Role::System => unreachable!(),
        };
        match turns.last_mut() {
            Some((last, existing)) if *last == role => existing.extend(blocks),
            _ => turns.push((role, blocks)),
        }
    }

    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "messages": turns
            .into_iter()
            .map(|(role, content)| json!({ "role": role, "content": content }))
            .collect::<Vec<_>>(),
    });
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.parameters }))
            .collect();
    }
    body
}

struct PendingUse {
    id: String,
    name: String,
    json: String,
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError> {
        let mut http = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", API_VERSION)
            .json(&request_body(&self.model, &req));
        if let Some(k) = &self.key {
            http = http.header("x-api-key", k.expose());
        }
        // Written once, when the request is over, however it ends.
        let mut log = RequestLog::new(
            "anthropic",
            &self.base_url,
            &self.model,
            request_kind(&req),
            &req,
        );
        let resp = match send(http, self.key.as_ref()).await {
            Ok(resp) => resp,
            Err(e) => {
                log.failed(&e);
                return Err(e);
            }
        };
        log.answered(resp.status().as_u16());

        let mut uses: BTreeMap<u64, PendingUse> = BTreeMap::new();
        // Counts the request once, when the stream is over or dropped (decision 0028).
        let mut meter = StreamMeter::new(request_kind(&req), &self.base_url, &self.model, false);
        Ok(sse_stream(
            resp,
            self.key.clone(),
            log,
            move |ev, out, log| {
                let v: Value = serde_json::from_str(&ev.data).map_err(|e| LlmError::BadJson {
                    why: e.to_string(),
                    data: ev.data.clone(),
                })?;
                let kind = v["type"].as_str().or(ev.event.as_deref()).unwrap_or("");
                match kind {
                    "message_start" => {
                        meter.anthropic_start(&v["message"]["usage"]);
                        log.tokens(meter.tokens());
                    }
                    "message_delta" => {
                        meter.anthropic_delta(&v["usage"]);
                        log.tokens(meter.tokens());
                        if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                            log.stop(reason);
                        }
                    }
                    "content_block_start" => {
                        let block = &v["content_block"];
                        if block["type"] == "tool_use" {
                            uses.insert(
                                v["index"].as_u64().unwrap_or(0),
                                PendingUse {
                                    id: block["id"].as_str().unwrap_or_default().to_string(),
                                    name: block["name"].as_str().unwrap_or_default().to_string(),
                                    json: String::new(),
                                },
                            );
                        }
                    }
                    "content_block_delta" => {
                        let d = &v["delta"];
                        match d["type"].as_str() {
                            Some("text_delta") => {
                                if let Some(t) = d["text"].as_str().filter(|t| !t.is_empty()) {
                                    out.push(LlmChunk::Text(t.to_string()));
                                }
                            }
                            Some("input_json_delta") => {
                                if let Some(u) = uses.get_mut(&v["index"].as_u64().unwrap_or(0)) {
                                    u.json
                                        .push_str(d["partial_json"].as_str().unwrap_or_default());
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        if let Some(u) = uses.remove(&v["index"].as_u64().unwrap_or(0)) {
                            let arguments = if u.json.trim().is_empty() {
                                json!({})
                            } else {
                                serde_json::from_str(&u.json).unwrap_or(Value::String(u.json))
                            };
                            out.push(LlmChunk::ToolCall(ToolCall {
                                id: u.id,
                                name: u.name,
                                arguments,
                            }));
                        }
                    }
                    "message_stop" => return Ok(Step::Finished),
                    // The provider's own refusal, sent after the status 200:
                    // overloaded, the request is too large.
                    "error" => return Err(LlmError::Provider(v["error"].to_string())),
                    _ => {}
                }
                Ok(Step::Continue)
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::llm::ChatMessage;

    #[test]
    fn anthropic_sends_system_separately_and_tool_results_as_user_blocks() {
        let req = LlmRequest {
            messages: vec![
                ChatMessage::system("будь краткой"),
                ChatMessage::user("время?"),
                ChatMessage::assistant_with_calls(
                    "Смотрю.",
                    vec![
                        ToolCall {
                            id: "t1".into(),
                            name: "get_time".into(),
                            arguments: json!({}),
                        },
                        ToolCall {
                            id: "t2".into(),
                            name: "echo".into(),
                            arguments: json!({"text": "a"}),
                        },
                    ],
                ),
                ChatMessage::tool_result("t1", "12:00"),
                ChatMessage::tool_result("t2", "a"),
            ],
            tools: vec![],
        };
        let b = request_body("claude", &req);
        assert_eq!(b["system"], "будь краткой");
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[2]["content"][1]["tool_use_id"], "t2");
    }
}
