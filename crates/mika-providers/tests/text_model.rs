//! The text model for tools against a fake HTTP server on 127.0.0.1.

use std::sync::Arc;

use mika_core::config::Config;
use mika_core::llm::TextModel;
use mika_core::secrets::{MemorySecretStore, Secret, SecretStore};
use mika_providers::live::Live;
use parking_lot::Mutex;
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
                // The head is ASCII, so its byte length equals its length in `text`.
                if buf.len() >= end + 4 + len {
                    let _ = tx.send(String::from_utf8_lossy(&buf).to_string());
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
    (format!("http://{addr}/v1"), rx)
}

fn sse_response(events: &[&str]) -> String {
    let body: String = events.iter().map(|e| format!("{e}\n\n")).collect();
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
}

fn live(base: &str, chat_model: &str) -> Live {
    let mut config = Config::default();
    config.llm.base_url = base.to_string();
    config.llm.model = chat_model.to_string();
    let secrets = MemorySecretStore::default();
    secrets
        .set(&config.llm.kind.secret_key(), &Secret::new("sk-test"))
        .unwrap();
    Live::new(Arc::new(Mutex::new(config)), Arc::new(secrets))
}

fn body_of(request: &str) -> serde_json::Value {
    let (_, body) = request.split_once("\r\n\r\n").expect("a request body");
    serde_json::from_str(body).expect("a JSON body")
}

#[tokio::test]
async fn request_has_no_tools_and_uses_the_named_model() {
    let (url, req) = serve(sse_response(&[
        r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"- Факт "}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"один."},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ]))
    .await;
    let l = live(&url, "chat-model");
    let answer = l
        .complete("cheap-model", "Ты выписываешь факты.", "Вопрос: что?", 2000)
        .await;
    let answer = answer.unwrap();
    assert_eq!(answer.text, "- Факт один.");
    assert_eq!(answer.streamed, 12);

    let request = req.await.unwrap();
    assert!(
        request.starts_with("POST /v1/chat/completions"),
        "{request}"
    );
    let body = body_of(&request);
    assert!(body.get("tools").is_none(), "{body}");
    assert_eq!(body["model"], "cheap-model");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], "Ты выписываешь факты.");
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "Вопрос: что?");
}

#[tokio::test]
async fn empty_model_goes_to_the_chat_model_and_tool_calls_are_dropped() {
    let (url, req) = serve(sse_response(&[
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"delete_file","arguments":"{}"}}]}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"<think>выполню</think>Запрос один"},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ]))
    .await;
    let l = live(&url, "chat-model");
    let answer = l.complete("", "система", "вопрос", 800).await;
    let answer = answer.unwrap();
    assert_eq!(answer.text, "Запрос один");
    // The reasoning block is not in the answer and is in the count.
    assert_eq!(answer.streamed, 33);
    assert_eq!(body_of(&req.await.unwrap())["model"], "chat-model");
}

#[tokio::test]
async fn answer_is_cut_at_the_character_cap() {
    let (url, _req) = serve(sse_response(&[
        r#"data: {"choices":[{"index":0,"delta":{"content":"абвгдеёжзи"}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":"клмнопрсту"},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ]))
    .await;
    let l = live(&url, "chat-model");
    let answer = l.complete("", "система", "вопрос", 4).await;
    assert_eq!(answer.unwrap().text, "абвг");
}

#[tokio::test]
async fn provider_error_comes_back_without_the_key() {
    let (url, _req) = serve(
        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\":\"bad key sk-test\"}"
            .to_string(),
    )
    .await;
    let l = live(&url, "chat-model");
    let err = l.complete("", "система", "вопрос", 100).await.unwrap_err();
    assert!(err.reason.contains("401"), "{err}");
    assert!(!err.reason.contains("sk-test"), "{err}");
}

/// Serves one request with an answer that never ends. Returns the base URL
/// and a signal that fires when the client hangs up.
async fn serve_endless(events: &[&str]) -> (String, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    let head = sse_response(events);
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        // The request head is enough: the body is small and comes with it or
        // right after, and is read below as bytes nobody looks at.
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut tmp).await.unwrap();
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        sock.write_all(head.as_bytes()).await.unwrap();
        sock.flush().await.unwrap();
        // The answer stays open. The end of the connection is the client's.
        loop {
            match sock.read(&mut tmp).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = tx.send(());
    });
    (format!("http://{addr}/v1"), rx)
}

#[tokio::test]
async fn dropped_request_closes_the_connection_while_the_model_only_reasons() {
    // Reasoning in its own field gives no chunk, and then the model is silent.
    let (url, hung_up) = serve_endless(&[
        r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"думаю"}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"reasoning_content":" дальше"}}]}"#,
    ])
    .await;
    let l = live(&url, "chat-model");
    let cut = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        l.complete("", "система", "вопрос", 100),
    )
    .await;
    assert!(cut.is_err(), "the request must still be waiting: {cut:?}");
    // The caller gave up: the reader must not keep the provider generating.
    tokio::time::timeout(std::time::Duration::from_secs(5), hung_up)
        .await
        .expect("the connection stays open after the request was dropped")
        .unwrap();
}
