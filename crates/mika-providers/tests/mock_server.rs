//! Adapters against a fake HTTP server on 127.0.0.1.

use futures::StreamExt;
use mika_core::llm::{ChatMessage, LlmChunk, LlmError, LlmProvider, LlmRequest, ToolCall};
use mika_core::secrets::Secret;
use mika_providers::anthropic::AnthropicProvider;
use mika_providers::openai::OpenAiProvider;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Serves one request. Returns the base URL and the raw request text.
async fn serve(response: String) -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = sock.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let len = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    let _ = tx.send(text);
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        sock.write_all(response.as_bytes()).await.unwrap();
        sock.shutdown().await.ok();
    });
    (format!("http://{addr}"), rx)
}

fn sse_response(events: &[&str]) -> String {
    let body: String = events.iter().map(|e| format!("{e}\n\n")).collect();
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
}

fn request() -> LlmRequest {
    LlmRequest {
        messages: vec![ChatMessage::user("привет")],
        tools: vec![],
    }
}

async fn collect(p: &dyn LlmProvider) -> Vec<Result<LlmChunk, LlmError>> {
    p.stream(request()).await.unwrap().collect().await
}

#[tokio::test]
async fn openai_streams_text_deltas() {
    let (url, req) = serve(sse_response(&[
        r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"При"}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"вет"},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ]))
    .await;
    let p = OpenAiProvider::new(url, "m".into(), Some(Secret::new("sk-1")));
    let chunks = collect(&p).await;
    assert_eq!(
        chunks,
        vec![
            Ok(LlmChunk::Text("При".into())),
            Ok(LlmChunk::Text("вет".into()))
        ]
    );
    let raw = req.await.unwrap();
    assert!(raw.starts_with("POST /chat/completions"));
    assert!(raw
        .to_ascii_lowercase()
        .contains("authorization: bearer sk-1"));
    assert!(raw.contains("\"stream\":true"));
    // No tracker of paid calls is installed in this test program: nothing is
    // added to the request for the sake of counting.
    assert!(!raw.contains("stream_options"), "{raw}");
}

#[tokio::test]
async fn openai_assembles_tool_call_from_fragments() {
    let (url, _req) = serve(sse_response(&[
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_9","type":"function","function":{"name":"echo","arguments":""}}]}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"text\":"}}]}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"hi\"}"}}]}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ]))
    .await;
    let p = OpenAiProvider::new(url, "m".into(), None);
    assert_eq!(
        collect(&p).await,
        vec![Ok(LlmChunk::ToolCall(ToolCall {
            id: "call_9".into(),
            name: "echo".into(),
            arguments: json!({"text": "hi"}),
        }))]
    );
}

#[tokio::test]
async fn openai_http_401_is_redacted() {
    let body = r#"{"error":{"message":"Incorrect API key provided: sk-very-secret"}}"#;
    let (url, _req) = serve(format!(
        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    ))
    .await;
    let p = OpenAiProvider::new(url, "m".into(), Some(Secret::new("sk-very-secret")));
    let err = match p.stream(request()).await {
        Err(e) => e,
        Ok(_) => panic!("expected error"),
    };
    let text = err.to_string();
    assert!(matches!(err, LlmError::Http { status: 401, .. }));
    assert!(!text.contains("sk-very-secret"), "{text}");
    assert!(text.contains("***"));
}

#[tokio::test]
async fn openai_disconnect_mid_stream_yields_network_error() {
    let resp = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n"
        .to_string()
        + "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Нача\"}}]}\n\n";
    let (url, _req) = serve(resp).await;
    let p = OpenAiProvider::new(url, "m".into(), None);
    let chunks = collect(&p).await;
    assert_eq!(chunks[0], Ok(LlmChunk::Text("Нача".into())));
    assert!(matches!(chunks.last(), Some(Err(LlmError::Network(_)))));
}

#[tokio::test]
async fn openai_unreachable_server_is_network_error() {
    let p = OpenAiProvider::new("http://127.0.0.1:1".into(), "m".into(), None);
    assert!(matches!(
        p.stream(request()).await,
        Err(LlmError::Network(_))
    ));
}

#[tokio::test]
async fn anthropic_streams_text() {
    let (url, req) = serve(sse_response(&[
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}",
        "event: ping\ndata: {\"type\":\"ping\"}",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Хай\"}}",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}",
    ]))
    .await;
    let p = AnthropicProvider::new(url, "claude".into(), Some(Secret::new("ak")));
    assert_eq!(collect(&p).await, vec![Ok(LlmChunk::Text("Хай".into()))]);
    let raw = req.await.unwrap().to_ascii_lowercase();
    assert!(raw.starts_with("post /v1/messages"));
    assert!(raw.contains("x-api-key: ak"));
    assert!(raw.contains("anthropic-version: 2023-06-01"));
}

#[tokio::test]
async fn anthropic_assembles_tool_use() {
    let (url, _req) = serve(sse_response(&[
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"echo\",\"input\":{}}}",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"text\\\": \"}}",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a\\\"}\"}}",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}",
    ]))
    .await;
    let p = AnthropicProvider::new(url, "claude".into(), None);
    assert_eq!(
        collect(&p).await,
        vec![Ok(LlmChunk::ToolCall(ToolCall {
            id: "toolu_1".into(),
            name: "echo".into(),
            arguments: json!({"text": "a"}),
        }))]
    );
}

#[tokio::test]
async fn anthropic_error_event_is_reported() {
    let (url, _req) = serve(sse_response(&[
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}",
    ]))
    .await;
    let p = AnthropicProvider::new(url, "claude".into(), None);
    assert!(matches!(
        collect(&p).await.last(),
        Some(Err(LlmError::Protocol(_)))
    ));
}
