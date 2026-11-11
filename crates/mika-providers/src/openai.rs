//! OpenAI-compatible `/chat/completions` with streaming. Also serves Ollama.

use std::collections::BTreeMap;

use async_trait::async_trait;
use mika_core::llm::{
    ChatMessage, LlmChunk, LlmError, LlmProvider, LlmRequest, LlmStream, Role, ToolCall,
};
use mika_core::secrets::Secret;
use serde_json::{json, Value};

use crate::common::{client, send, sse_stream, Step};

pub struct OpenAiProvider {
    name: String,
    base_url: String,
    model: String,
    key: Option<Secret>,
    http: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(base_url: String, model: String, api_key: Option<Secret>) -> Self {
        Self {
            name: "openai".into(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            key: api_key.filter(|k| !k.is_empty()),
            http: client(),
        }
    }

    pub fn named(mut self, name: &str) -> Self {
        self.name = name.into();
        self
    }
}

fn message_json(m: &ChatMessage) -> Value {
    match m.role {
        Role::System => json!({ "role": "system", "content": m.content }),
        Role::User => json!({ "role": "user", "content": m.content }),
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
            "content": m.content,
        }),
        Role::Assistant if m.tool_calls.is_empty() => {
            json!({ "role": "assistant", "content": m.content })
        }
        Role::Assistant => json!({
            "role": "assistant",
            "content": if m.content.is_empty() { Value::Null } else { json!(m.content) },
            "tool_calls": m.tool_calls.iter().map(|c| json!({
                "id": c.id,
                "type": "function",
                "function": { "name": c.name, "arguments": c.arguments.to_string() },
            })).collect::<Vec<_>>(),
        }),
    }
}

pub fn request_body(model: &str, req: &LlmRequest) -> Value {
    let mut body = json!({
        "model": model,
        "stream": true,
        "messages": req.messages.iter().map(message_json).collect::<Vec<_>>(),
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.parameters },
                })
            })
            .collect();
    }
    body
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    args: String,
}

fn parse_args(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    // Broken JSON goes to the agent as a string; the tool rejects it with a clear message.
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn flush(calls: &mut BTreeMap<u64, PendingCall>, out: &mut Vec<LlmChunk>) {
    for (i, c) in std::mem::take(calls) {
        out.push(LlmChunk::ToolCall(ToolCall {
            id: if c.id.is_empty() {
                format!("call_{i}")
            } else {
                c.id
            },
            name: c.name,
            arguments: parse_args(&c.args),
        }));
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError> {
        let mut http = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .json(&request_body(&self.model, &req));
        if let Some(k) = &self.key {
            http = http.bearer_auth(k.expose());
        }
        let resp = send(http, self.key.as_ref()).await?;

        let mut calls: BTreeMap<u64, PendingCall> = BTreeMap::new();
        Ok(sse_stream(resp, self.key.clone(), move |ev, out| {
            if ev.data.trim() == "[DONE]" {
                flush(&mut calls, out);
                return Ok(Step::Finished);
            }
            let v: Value = serde_json::from_str(&ev.data)
                .map_err(|e| LlmError::Protocol(format!("{e}: {}", ev.data)))?;
            if let Some(err) = v.get("error") {
                return Err(LlmError::Protocol(err.to_string()));
            }
            let Some(choice) = v["choices"].get(0) else {
                return Ok(Step::Continue);
            };
            let delta = &choice["delta"];
            if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
                out.push(LlmChunk::Text(text.to_string()));
            }
            if let Some(list) = delta["tool_calls"].as_array() {
                for (n, tc) in list.iter().enumerate() {
                    let idx = tc["index"].as_u64().unwrap_or(n as u64);
                    let entry = calls.entry(idx).or_default();
                    if let Some(id) = tc["id"].as_str() {
                        entry.id = id.to_string();
                    }
                    if let Some(name) = tc["function"]["name"].as_str() {
                        entry.name.push_str(name);
                    }
                    if let Some(a) = tc["function"]["arguments"].as_str() {
                        entry.args.push_str(a);
                    }
                }
            }
            if choice["finish_reason"].is_string() {
                flush(&mut calls, out);
            }
            Ok(Step::Continue)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::llm::ToolSpec;

    #[test]
    fn request_body_encodes_tool_calls_and_results() {
        let req = LlmRequest {
            messages: vec![
                ChatMessage::system("s"),
                ChatMessage::user("u"),
                ChatMessage::assistant_with_calls(
                    "",
                    vec![ToolCall {
                        id: "c1".into(),
                        name: "echo".into(),
                        arguments: json!({"text": "x"}),
                    }],
                ),
                ChatMessage::tool_result("c1", "x"),
            ],
            tools: vec![ToolSpec {
                name: "echo".into(),
                description: "d".into(),
                parameters: json!({"type": "object"}),
            }],
        };
        let b = request_body("m", &req);
        assert_eq!(b["stream"], true);
        assert_eq!(b["messages"][2]["content"], Value::Null);
        assert_eq!(
            b["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"text\":\"x\"}"
        );
        assert_eq!(b["messages"][3]["tool_call_id"], "c1");
        assert_eq!(b["tools"][0]["function"]["name"], "echo");
    }
}
