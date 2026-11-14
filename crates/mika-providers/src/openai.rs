//! OpenAI-compatible `/chat/completions` with streaming. Also serves Ollama.

use std::collections::BTreeMap;

use async_trait::async_trait;
use mika_core::llm::{
    ChatMessage, LlmChunk, LlmError, LlmProvider, LlmRequest, LlmStream, Role, ToolCall,
};
use mika_core::secrets::Secret;
use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::common::{client, send, sse_stream, Step};
use crate::metering::{request_kind, StreamMeter};
use crate::trace::RequestLog;

/// What a provider said to `stream_options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageOption {
    /// A request with the field was answered: a later refusal is about
    /// something else and is returned as it is.
    Accepted,
    /// The request went through only without the field: until the app
    /// restarts every request goes without it at once. These requests are
    /// counted without token data.
    Refused,
}

/// By address and model, until the app restarts.
static USAGE_OPTION: Mutex<Vec<((String, String), UsageOption)>> = Mutex::new(Vec::new());

fn usage_option(provider: &(String, String)) -> Option<UsageOption> {
    USAGE_OPTION
        .lock()
        .iter()
        .find(|(p, _)| p == provider)
        .map(|(_, said)| *said)
}

fn note_usage_option(provider: &(String, String), said: UsageOption) {
    let mut known = USAGE_OPTION.lock();
    match known.iter_mut().find(|(p, _)| p == provider) {
        Some(entry) => entry.1 = said,
        None => known.push((provider.clone(), said)),
    }
}

/// A refusal of the request for usage rather than of the request itself.
fn refuses_usage_option(body: &str) -> bool {
    let body = body.to_lowercase();
    body.contains("stream_options") || body.contains("include_usage")
}

/// Whether paid calls are counted now. Only then a request asks for usage:
/// with the counting off nothing is added to a request.
fn counting(costs: Option<mika_core::costs::Costs>) -> bool {
    costs.is_some_and(|c| c.enabled())
}

/// Whether a refused request is sent once more without `stream_options`.
/// The field is new to the request, and a strict provider may refuse it
/// without naming it: until a provider has answered a request that carried
/// it, any 400 or 422 is tried again without it. A provider that took the
/// field before is tried again only when its refusal names the field.
fn retry_without_usage(known: Option<UsageOption>, refusal: &str) -> bool {
    known.is_none() || refuses_usage_option(refusal)
}

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
        Role::User if m.images.is_empty() => json!({ "role": "user", "content": m.content }),
        Role::User => {
            let mut parts = vec![json!({ "type": "text", "text": m.content })];
            for png in &m.images {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": format!("data:image/png;base64,{png}") },
                }));
            }
            json!({ "role": "user", "content": parts })
        }
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
        let post = |body: &Value| {
            let mut http = self
                .http
                .post(format!("{}/chat/completions", self.base_url))
                .json(body);
            if let Some(k) = &self.key {
                http = http.bearer_auth(k.expose());
            }
            http
        };
        // The last chunk of the stream then carries `usage`: tokens, and with
        // RouterAI and OpenRouter the cost (decision 0028). Asked for only
        // while paid calls are counted, and never a reason for a request to
        // fail: a refusal is tried once more without the field.
        let mut body = request_body(&self.model, &req);
        // Written once, when the request is over, however it ends.
        let mut log = RequestLog::new(
            &self.name,
            &self.base_url,
            &self.model,
            request_kind(&req),
            &req,
        );
        let provider = (self.base_url.clone(), self.model.clone());
        let known = usage_option(&provider);
        let ask_usage =
            counting(mika_core::costs::installed()) && known != Some(UsageOption::Refused);
        if ask_usage {
            body["stream_options"] = json!({ "include_usage": true });
        }
        let resp = match send(post(&body), self.key.as_ref()).await {
            Err(LlmError::Http {
                status: status @ (400 | 422),
                body: refusal,
            }) if ask_usage && retry_without_usage(known, &refusal) => {
                if let Some(fields) = body.as_object_mut() {
                    fields.remove("stream_options");
                }
                log.retried();
                match send(post(&body), self.key.as_ref()).await {
                    Ok(resp) => {
                        log::warn!(
                            "provider refused stream_options, tokens are not counted until the app restarts: host={} model={} http={status}",
                            mika_core::costs::provider_host(&self.base_url),
                            self.model
                        );
                        note_usage_option(&provider, UsageOption::Refused);
                        resp
                    }
                    // The request itself is refused: the first answer says why.
                    Err(_) => {
                        let error = LlmError::Http {
                            status,
                            body: refusal,
                        };
                        log.failed(&error);
                        return Err(error);
                    }
                }
            }
            Ok(resp) => {
                if ask_usage && known.is_none() {
                    note_usage_option(&provider, UsageOption::Accepted);
                }
                resp
            }
            Err(e) => {
                log.failed(&e);
                return Err(e);
            }
        };
        log.answered(resp.status().as_u16());

        let mut calls: BTreeMap<u64, PendingCall> = BTreeMap::new();
        // Counts the request once, when the stream is over or dropped. Ollama
        // takes no money on this computer or in the home network; at a public
        // address it may be a paid service, and is counted as any other.
        let free = self.name == "ollama"
            && mika_core::costs::is_private(&mika_core::costs::provider_host(&self.base_url));
        let mut meter = StreamMeter::new(request_kind(&req), &self.base_url, &self.model, free);
        Ok(sse_stream(
            resp,
            self.key.clone(),
            log,
            move |ev, out, log| {
                if ev.data.trim() == "[DONE]" {
                    flush(&mut calls, out);
                    return Ok(Step::Finished);
                }
                let v: Value = serde_json::from_str(&ev.data).map_err(|e| LlmError::BadJson {
                    why: e.to_string(),
                    data: ev.data.clone(),
                })?;
                // The provider's own refusal, sent after the status 200: the
                // limit of requests, no credits, the context is too long.
                if let Some(err) = v.get("error") {
                    return Err(LlmError::Provider(err.to_string()));
                }
                // The usage chunk has an empty `choices` array.
                if v["usage"].is_object() {
                    meter.openai(&v["usage"]);
                    log.tokens(meter.tokens());
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
                if let Some(reason) = choice["finish_reason"].as_str() {
                    log.stop(reason);
                    flush(&mut calls, out);
                }
                Ok(Step::Continue)
            },
        ))
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

    #[test]
    fn a_refusal_of_the_usage_option_is_told_from_other_errors() {
        assert!(refuses_usage_option(
            r#"{"error":{"message":"Unknown parameter: 'stream_options'"}}"#
        ));
        assert!(refuses_usage_option("Extra inputs: Include_Usage"));
        assert!(!refuses_usage_option(
            r#"{"error":{"message":"model not found"}}"#
        ));
        // The body the adapter builds itself does not ask for usage.
        let req = LlmRequest {
            messages: vec![ChatMessage::user("u")],
            tools: Vec::new(),
        };
        assert!(request_body("m", &req).get("stream_options").is_none());
    }

    #[test]
    fn usage_is_asked_for_only_while_paid_calls_are_counted() {
        use mika_core::costs::{Costs, CostsConfig};
        // A tracker of this test alone: the one of the process is not touched.
        let costs = Costs::new(std::sync::Arc::new(
            mika_core::storage::Db::open_in_memory().unwrap(),
        ));
        assert!(!counting(None), "no tracker, nothing is added to a request");
        assert!(counting(Some(costs.clone())));
        costs.set_settings(
            &CostsConfig {
                enabled: false,
                ..Default::default()
            },
            "",
        );
        assert!(!counting(Some(costs)), "counting is off");
    }

    #[test]
    fn a_refusal_is_tried_without_the_field_until_the_provider_took_it_once() {
        let named = r#"{"error":{"message":"Unknown parameter: 'stream_options'"}}"#;
        let other = r#"{"error":{"message":"Unsupported parameter"}}"#;
        // Nothing is known of the provider: any refusal may be about the field.
        assert!(retry_without_usage(None, named));
        assert!(retry_without_usage(None, other));
        // It answered a request with the field before: only a refusal that
        // names the field is about it.
        assert!(retry_without_usage(Some(UsageOption::Accepted), named));
        assert!(!retry_without_usage(Some(UsageOption::Accepted), other));

        let provider = ("https://usage-option.test/v1".to_string(), "m".to_string());
        assert_eq!(usage_option(&provider), None);
        note_usage_option(&provider, UsageOption::Accepted);
        assert_eq!(usage_option(&provider), Some(UsageOption::Accepted));
        note_usage_option(&provider, UsageOption::Refused);
        assert_eq!(usage_option(&provider), Some(UsageOption::Refused));
        let same_address = (provider.0.clone(), "other".to_string());
        assert_eq!(usage_option(&same_address), None);
    }

    #[test]
    fn picture_goes_as_a_data_url_part() {
        let req = LlmRequest {
            messages: vec![ChatMessage::user_with_image("что тут?", "QUJD")],
            tools: Vec::new(),
        };
        let b = request_body("m", &req);
        assert_eq!(b["messages"][0]["content"][0]["text"], "что тут?");
        assert_eq!(
            b["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,QUJD"
        );
    }
}
