//! MCP over HTTP against a fake server on 127.0.0.1.

use std::sync::Arc;

use mika_core::config::McpServerConfig;
use mika_providers::mcp::McpClient;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One received request: lower-case head and the JSON body.
#[derive(Debug, Clone)]
struct Seen {
    head: String,
    body: Value,
}

/// Serves requests one connection at a time. `reply` gets the request body and
/// returns (status line, extra headers, body).
async fn serve(
    reply: impl Fn(&Value) -> (&'static str, String, String) + Send + 'static,
) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            let (head, body) = loop {
                let n = sock.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let head = text[..end].to_ascii_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map(|v| v.trim().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        let body = serde_json::from_slice(&buf[end + 4..end + 4 + len])
                            .unwrap_or(Value::Null);
                        break (head, body);
                    }
                }
                if n == 0 {
                    break (String::new(), Value::Null);
                }
            };
            let (status, headers, text) = reply(&body);
            log.lock().push(Seen { head, body });
            let response = format!(
                "HTTP/1.1 {status}\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            );
            sock.write_all(response.as_bytes()).await.unwrap();
            sock.shutdown().await.ok();
        }
    });
    (format!("http://{addr}/mcp"), seen)
}

fn json_reply(body: &Value, result: Value) -> (&'static str, String, String) {
    (
        "200 OK",
        "content-type: application/json\r\nmcp-session-id: sess-42\r\n".into(),
        json!({ "jsonrpc": "2.0", "id": body["id"], "result": result }).to_string(),
    )
}

fn config(url: &str) -> McpServerConfig {
    McpServerConfig {
        name: "remote".into(),
        url: url.into(),
        enabled: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn handshake_tools_and_call_over_plain_json() {
    let (url, seen) = serve(|body| match body["method"].as_str() {
        Some("initialize") => json_reply(body, json!({ "protocolVersion": "2025-06-18" })),
        Some("notifications/initialized") => ("202 Accepted", String::new(), String::new()),
        Some("tools/list") => json_reply(
            body,
            json!({ "tools": [{ "name": "ping", "description": "Отвечает",
                "inputSchema": { "type": "object", "properties": {} } }] }),
        ),
        Some("tools/call") => json_reply(
            body,
            json!({ "content": [{ "type": "text", "text": "понг" }] }),
        ),
        _ => ("404 Not Found", String::new(), String::new()),
    })
    .await;
    let client = McpClient::start(&config(&url)).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "ping");
    assert_eq!(
        client.call_tool("ping", &json!({})).await,
        Ok("понг".into())
    );

    let seen = seen.lock();
    assert_eq!(seen[0].body["method"], "initialize");
    assert!(seen[0]
        .head
        .contains("accept: application/json, text/event-stream"));
    assert!(!seen[0].head.contains("mcp-session-id"));
    // The notification carries no id; everything after initialize carries the session.
    assert!(seen[1].body.get("id").is_none());
    for later in &seen[1..] {
        assert!(
            later.head.contains("mcp-session-id: sess-42"),
            "{}",
            later.head
        );
    }
    assert_eq!(seen[3].body["params"]["name"], "ping");
}

#[tokio::test]
async fn answer_is_found_inside_an_event_stream() {
    let (url, _seen) = serve(|body| {
        let id = &body["id"];
        // A server notice first, then the answer, as the spec allows.
        let stream = format!(
            "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
            json!({ "jsonrpc": "2.0", "method": "notifications/progress" }),
            json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": "из потока" }] } })
        );
        ("200 OK", "content-type: text/event-stream\r\n".into(), stream)
    })
    .await;
    let client = McpClient::over_http("remote", &url, None).unwrap();
    assert_eq!(
        client.call_tool("any", &json!({})).await,
        Ok("из потока".into())
    );
}

#[tokio::test]
async fn errors_name_the_server_and_the_reason() {
    let (url, _seen) = serve(|body| match body["method"].as_str() {
        Some("tools/list") => (
            "200 OK",
            "content-type: application/json\r\n".into(),
            json!({ "jsonrpc": "2.0", "id": body["id"], "error": { "code": -32000, "message": "нет доступа" } })
                .to_string(),
        ),
        _ => ("401 Unauthorized", String::new(), "token required".into()),
    })
    .await;
    let client = McpClient::over_http("remote", &url, None).unwrap();
    assert_eq!(client.list_tools().await, Err("нет доступа".into()));
    let err = client.call_tool("x", &json!({})).await.unwrap_err();
    assert!(
        err.contains("сервер «remote»") && err.contains("401") && err.contains("token required"),
        "{err}"
    );
}

#[tokio::test]
async fn token_is_sent_and_never_shown_in_errors() {
    let (url, seen) = serve(|_| {
        (
            "403 Forbidden",
            String::new(),
            "bad token tok-secret-77".into(),
        )
    })
    .await;
    let token = mika_core::secrets::Secret::new("tok-secret-77");
    let client = McpClient::over_http("remote", &url, Some(token)).unwrap();
    let err = client.call_tool("x", &json!({})).await.unwrap_err();
    assert!(err.contains("403"), "{err}");
    assert!(!err.contains("tok-secret-77"), "{err}");
    assert!(seen.lock()[0]
        .head
        .contains("authorization: bearer tok-secret-77"));
}

#[tokio::test]
async fn bad_address_and_dead_server_fail_fast() {
    assert!(McpClient::over_http("x", "ftp://host/mcp", None).is_err());
    let err = McpClient::start(&config("http://127.0.0.1:1/mcp"))
        .await
        .err()
        .unwrap();
    assert!(err.contains("нет связи"), "{err}");
}
