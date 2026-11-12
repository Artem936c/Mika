//! MCP client. A server is either started as a child process (stdio transport)
//! or reached by address (streamable HTTP transport).
//!
//! Each server tool becomes a `Tool` in the registry, so it passes the risk
//! policy like any built-in tool.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::config::McpServerConfig;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Tool output passed to the model, characters.
const MAX_OUTPUT: usize = 16_000;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

#[derive(Debug, Clone, PartialEq)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct McpClient {
    server: String,
    writer: tokio::sync::Mutex<Writer>,
    pending: Pending,
    next_id: AtomicU64,
    /// Keeps the child alive; dropping the client kills it.
    _child: Option<tokio::process::Child>,
    /// Set for a server reached over HTTP; then `writer` and `pending` are unused.
    http: Option<HttpTransport>,
}

struct HttpTransport {
    url: String,
    /// Sent as `Authorization: Bearer …` when the server needs it.
    token: Option<mika_core::secrets::Secret>,
    client: reqwest::Client,
    /// `Mcp-Session-Id` the server gave at `initialize`; sent with every later request.
    session: Mutex<Option<String>>,
}

/// The result or the error of a JSON-RPC answer.
fn answer(msg: &Value) -> Result<Value, String> {
    match msg.get("error") {
        Some(e) => Err(e["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| e.to_string())),
        None => Ok(msg["result"].clone()),
    }
}

fn spawn(cfg: &McpServerConfig, through_cmd: bool) -> std::io::Result<tokio::process::Child> {
    let mut cmd = if through_cmd {
        // `npx`, `uvx` and friends are .cmd scripts: they start only through cmd.exe.
        let mut c = tokio::process::Command::new("cmd.exe");
        c.arg("/C").arg(&cfg.command);
        c
    } else {
        tokio::process::Command::new(&cfg.command)
    };
    cmd.args(&cfg.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()
}

impl McpClient {
    /// Starts the server process and performs the MCP handshake.
    pub async fn start(cfg: &McpServerConfig) -> Result<Arc<Self>, String> {
        Self::start_with(cfg, None).await
    }

    /// Like `start`; `token` goes to a server reached over HTTP.
    pub async fn start_with(
        cfg: &McpServerConfig,
        token: Option<mika_core::secrets::Secret>,
    ) -> Result<Arc<Self>, String> {
        let url = cfg.url.trim();
        if !url.is_empty() {
            let client = Self::over_http(&cfg.name, url, token)?;
            tokio::time::timeout(START_TIMEOUT, client.initialize())
                .await
                .map_err(|_| format!("сервер «{}» не ответил за 30 секунд", cfg.name))??;
            return Ok(client);
        }
        if cfg.command.trim().is_empty() {
            return Err("не задана команда запуска сервера".into());
        }
        let mut child = spawn(cfg, false)
            .or_else(|_| spawn(cfg, true))
            .map_err(|e| format!("сервер «{}» не запустился: {e}", cfg.name))?;
        let stdin = child.stdin.take().ok_or("нет входа у процесса сервера")?;
        let stdout = child.stdout.take().ok_or("нет выхода у процесса сервера")?;
        let client = Self::connect(&cfg.name, stdout, Box::new(stdin), Some(child));
        tokio::time::timeout(START_TIMEOUT, client.initialize())
            .await
            .map_err(|_| format!("сервер «{}» не ответил за 30 секунд", cfg.name))??;
        Ok(client)
    }

    /// Wires the client to a pair of streams. Tests use an in-memory pair.
    pub fn connect(
        server: &str,
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: Writer,
        child: Option<tokio::process::Child>,
    ) -> Arc<Self> {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let waiting = pending.clone();
        let name = server.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                // Answers carry an id and no method; requests and notifications from the server are ignored.
                let Some(id) = msg["id"].as_u64().filter(|_| msg.get("method").is_none()) else {
                    continue;
                };
                if let Some(tx) = waiting.lock().remove(&id) {
                    let _ = tx.send(answer(&msg));
                }
            }
            log::warn!("MCP server {name} closed its output");
            // Wake everything that still waits: the server is gone.
            waiting.lock().clear();
        });
        Arc::new(Self {
            server: server.to_string(),
            writer: tokio::sync::Mutex::new(writer),
            pending,
            next_id: AtomicU64::new(1),
            _child: child,
            http: None,
        })
    }

    /// A client for a server reached by address. Nothing is sent until `initialize`.
    pub fn over_http(
        server: &str,
        url: &str,
        token: Option<mika_core::secrets::Secret>,
    ) -> Result<Arc<Self>, String> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("адрес сервера должен начинаться с http:// или https://".into());
        }
        Ok(Arc::new(Self {
            server: server.to_string(),
            writer: tokio::sync::Mutex::new(Box::new(tokio::io::sink())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            _child: None,
            http: Some(HttpTransport {
                url: url.to_string(),
                token: token.filter(|t| !t.is_empty()),
                client: crate::common::client(),
                session: Mutex::new(None),
            }),
        }))
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Posts one JSON-RPC message. With `want` it returns the answer to that id,
    /// read from a JSON body or from an event stream; without it, nothing.
    async fn http_post(
        &self,
        t: &HttpTransport,
        msg: &Value,
        want: Option<u64>,
    ) -> Result<Value, String> {
        use futures::StreamExt;
        let fail = |what: String| format!("сервер «{}»: {what}", self.server);
        let mut req = t
            .client
            .post(&t.url)
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", PROTOCOL_VERSION)
            .json(msg);
        if let Some(id) = t.session.lock().clone() {
            req = req.header("mcp-session-id", id);
        }
        if let Some(token) = &t.token {
            req = req.bearer_auth(token.expose());
        }
        let hide = |text: String| crate::factory::redact(&text, t.token.as_ref());
        let resp = req
            .send()
            .await
            .map_err(|e| fail(hide(format!("нет связи: {}", e.without_url()))))?;
        if let Some(id) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *t.session.lock() = Some(id.to_string());
        }
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(fail(hide(format!(
                "ответ {}: {}",
                status.as_u16(),
                cut(body.trim(), 200)
            ))));
        }
        let Some(want) = want else {
            return Ok(Value::Null);
        };
        let is_stream = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/event-stream"));
        if !is_stream {
            let body = resp
                .text()
                .await
                .map_err(|e| fail(e.without_url().to_string()))?;
            let v: Value =
                serde_json::from_str(&body).map_err(|e| fail(format!("непонятный ответ: {e}")))?;
            return answer(&v);
        }
        // The answer is one of the events; requests and notices from the server come first.
        let mut parser = crate::sse::SseParser::new();
        let mut body = resp.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| fail(e.without_url().to_string()))?;
            for event in parser.push(&chunk) {
                let Ok(v) = serde_json::from_str::<Value>(&event.data) else {
                    continue;
                };
                if v["id"].as_u64() == Some(want) && v.get("method").is_none() {
                    return answer(&v);
                }
            }
        }
        Err(fail("поток закрылся без ответа".into()))
    }

    async fn send(&self, msg: &Value) -> Result<(), String> {
        if let Some(t) = &self.http {
            return self.http_post(t, msg, None).await.map(|_| ());
        }
        let mut line = msg.to_string();
        line.push('\n');
        let mut w = self.writer.lock().await;
        w.write_all(line.as_bytes())
            .await
            .map_err(|e| format!("сервер «{}» не принимает запросы: {e}", self.server))?;
        w.flush().await.map_err(|e| e.to_string())
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Some(t) = &self.http {
            let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
            return tokio::time::timeout(timeout, self.http_post(t, &msg, Some(id)))
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "сервер «{}» не ответил за {} секунд",
                        self.server,
                        timeout.as_secs()
                    ))
                });
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        let sent = self
            .send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        if let Err(e) = sent {
            self.pending.lock().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(format!("сервер «{}» закрылся", self.server)),
            Err(_) => {
                self.pending.lock().remove(&id);
                Err(format!(
                    "сервер «{}» не ответил за {} секунд",
                    self.server,
                    timeout.as_secs()
                ))
            }
        }
    }

    pub async fn initialize(&self) -> Result<(), String> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "Mika", "version": env!("CARGO_PKG_VERSION") },
            }),
            START_TIMEOUT,
        )
        .await?;
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, String> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        // A server may answer in pages; ten is far more than any real one has.
        for _ in 0..10 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let v = self.request("tools/list", params, START_TIMEOUT).await?;
            for t in v["tools"].as_array().into_iter().flatten() {
                let Some(name) = t["name"].as_str() else {
                    continue;
                };
                let schema = match &t["inputSchema"] {
                    s @ Value::Object(_) if s["type"] == "object" => s.clone(),
                    _ => json!({ "type": "object", "properties": {} }),
                };
                out.push(McpToolInfo {
                    name: name.to_string(),
                    description: t["description"].as_str().unwrap_or("").to_string(),
                    input_schema: schema,
                });
            }
            cursor = v["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    /// Text of the tool result. A result marked `isError` comes back as `Err`.
    pub async fn call_tool(&self, name: &str, args: &Value) -> Result<String, String> {
        let v = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": args }),
                CALL_TIMEOUT,
            )
            .await?;
        let mut parts = Vec::new();
        for item in v["content"].as_array().into_iter().flatten() {
            match item["type"].as_str() {
                Some("text") => parts.push(item["text"].as_str().unwrap_or("").to_string()),
                Some(other) => parts.push(format!("[{other}: не показано]")),
                None => {}
            }
        }
        if parts.is_empty() {
            if let Some(s) = v.get("structuredContent") {
                parts.push(s.to_string());
            }
        }
        let text = parts.join("\n");
        if v["isError"].as_bool() == Some(true) {
            return Err(if text.is_empty() {
                "инструмент сообщил об ошибке".into()
            } else {
                text
            });
        }
        Ok(text)
    }
}

/// `mcp_<server>_<tool>` with only letters, digits and `_`, at most 64 characters.
pub fn tool_name(server: &str, tool: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    };
    let mut name = format!("mcp_{}_{}", clean(server), clean(tool));
    name.truncate(64);
    name
}

pub struct McpTool {
    client: Arc<McpClient>,
    info: McpToolInfo,
    name: String,
}

impl McpTool {
    pub fn new(client: Arc<McpClient>, info: McpToolInfo) -> Self {
        let name = tool_name(client.server(), &info.name);
        Self { client, info, name }
    }
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "…"
    }
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: format!(
                "[расширение {}] {}",
                self.client.server(),
                cut(&self.info.description, 800)
            ),
            parameters: self.info.input_schema.clone(),
        }
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        if !args.is_object() {
            return Err(ToolError::BadArguments(
                "аргументы должны быть объектом".into(),
            ));
        }
        Ok(Assessment::new(
            RiskLevel::Medium,
            format!(
                "Вызвать «{}» из расширения «{}»",
                self.info.name,
                self.client.server()
            ),
            cut(&args.to_string(), 400),
            "внешний инструмент MCP: что именно он делает, знает только его сервер",
        )
        .with_scope(self.name.clone()))
    }

    async fn run(
        &self,
        args: Value,
        _approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        tokio::select! {
            _ = cancel.cancelled() => Err(ToolError::Cancelled),
            r = self.client.call_tool(&self.info.name, &args) => r
                .map(|text| ToolOutput::new(cut(&text, MAX_OUTPUT)))
                .map_err(ToolError::Failed),
        }
    }
}

/// Starts every enabled server and returns its tools. A server that fails is
/// reported in the second list and skipped.
pub async fn connect_all(
    servers: &[McpServerConfig],
) -> (Vec<Arc<dyn Tool>>, Vec<(String, Result<usize, String>)>) {
    connect_all_with(servers, &|_| None).await
}

/// Like `connect_all`; `token_of` gives the token of a server by its name.
pub async fn connect_all_with(
    servers: &[McpServerConfig],
    token_of: &(dyn Fn(&str) -> Option<mika_core::secrets::Secret> + Sync),
) -> (Vec<Arc<dyn Tool>>, Vec<(String, Result<usize, String>)>) {
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut report = Vec::new();
    for cfg in servers.iter().filter(|s| s.enabled) {
        let token = token_of(&cfg.name);
        let result = async {
            let client = McpClient::start_with(cfg, token).await?;
            let list = client.list_tools().await?;
            Ok::<_, String>((client, list))
        }
        .await;
        match result {
            Ok((client, list)) => {
                report.push((cfg.name.clone(), Ok(list.len())));
                for info in list {
                    tools.push(Arc::new(McpTool::new(client.clone(), info)));
                }
            }
            Err(e) => {
                log::warn!("MCP server {} failed: {e}", cfg.name);
                report.push((cfg.name.clone(), Err(e)));
            }
        }
    }
    (tools, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A tiny MCP server over an in-memory pipe. Returns the client and what the server received.
    fn fake_server() -> (Arc<McpClient>, Arc<Mutex<Vec<Value>>>) {
        let (client_side, server_side) = duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_side);
        let (server_read, mut server_write) = tokio::io::split(server_side);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(server_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let msg: Value = serde_json::from_str(&line).unwrap();
                log.lock().push(msg.clone());
                let Some(id) = msg["id"].as_u64() else {
                    continue;
                };
                let result = match msg["method"].as_str().unwrap() {
                    "initialize" => {
                        json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {} })
                    }
                    "tools/list" => json!({ "tools": [
                        { "name": "add-numbers", "description": "Складывает",
                          "inputSchema": { "type": "object", "properties": { "a": { "type": "number" } } } },
                        { "name": "no_schema" }
                    ]}),
                    "tools/call" => match msg["params"]["name"].as_str().unwrap() {
                        "add-numbers" => json!({ "content": [
                            { "type": "text", "text": "сумма 5" }, { "type": "image", "data": "..." }
                        ]}),
                        "silent" => continue,
                        _ => {
                            json!({ "isError": true, "content": [{ "type": "text", "text": "нет такого" }] })
                        }
                    },
                    _ => {
                        let reply = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "unknown" } });
                        server_write
                            .write_all(format!("{reply}\n").as_bytes())
                            .await
                            .unwrap();
                        continue;
                    }
                };
                // A server notification in between must not confuse the client.
                server_write
                    .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n")
                    .await
                    .unwrap();
                let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                server_write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        (
            McpClient::connect("demo server", client_read, Box::new(client_write), None),
            seen,
        )
    }

    #[tokio::test]
    async fn handshake_lists_tools_and_calls_one() {
        let (client, seen) = fake_server();
        client.initialize().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "add-numbers");
        assert_eq!(tools[1].input_schema["type"], "object");
        assert_eq!(
            client.call_tool("add-numbers", &json!({"a": 2})).await,
            Ok("сумма 5\n[image: не показано]".into())
        );
        assert_eq!(
            client.call_tool("boom", &json!({})).await,
            Err("нет такого".into())
        );
        let seen = seen.lock();
        assert_eq!(seen[0]["params"]["clientInfo"]["name"], "Mika");
        assert_eq!(seen[1]["method"], "notifications/initialized");
        assert!(seen[1].get("id").is_none());
    }

    #[tokio::test]
    async fn server_tool_is_a_medium_risk_tool_with_its_own_scope() {
        let (client, _) = fake_server();
        let info = client.list_tools().await.unwrap().remove(0);
        let tool = McpTool::new(client, info);
        let spec = tool.spec();
        assert_eq!(spec.name, "mcp_demo_server_add_numbers");
        assert!(spec
            .description
            .starts_with("[расширение demo server] Складывает"));
        let a = tool.assess(&json!({"a": 2})).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert_eq!(a.scope, "mcp_demo_server_add_numbers");
        assert!(tool.assess(&json!("строка")).is_err());
        let out = tool
            .run(
                json!({"a": 2}),
                Approval::for_tests(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(out.text.starts_with("сумма 5"));
    }

    #[tokio::test]
    async fn cancel_stops_waiting_for_a_silent_server() {
        let (client, _) = fake_server();
        let tool = McpTool::new(
            client,
            McpToolInfo {
                name: "silent".into(),
                description: String::new(),
                input_schema: json!({ "type": "object" }),
            },
        );
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            c2.cancel();
        });
        assert_eq!(
            tool.run(json!({}), Approval::for_tests(), cancel).await,
            Err(ToolError::Cancelled)
        );
    }

    #[test]
    fn tool_names_are_safe_for_every_provider() {
        assert_eq!(tool_name("files", "read_file"), "mcp_files_read_file");
        assert_eq!(
            tool_name("мой сервер", "do.it"),
            format!("mcp_{}_do_it", "_".repeat(10))
        );
        assert_eq!(tool_name("s", &"x".repeat(100)).len(), 64);
    }

    #[tokio::test]
    async fn missing_command_is_reported_not_panicked() {
        let cfg = McpServerConfig {
            name: "ghost".into(),
            command: "definitely-no-such-program-4821".into(),
            args: vec![],
            enabled: true,
            url: String::new(),
        };
        let (tools, report) = connect_all(&[cfg.clone(), McpServerConfig::default()]).await;
        assert!(tools.is_empty());
        assert_eq!(report.len(), 1);
        assert!(report[0].1.is_err());
        assert!(McpClient::start(&McpServerConfig::default()).await.is_err());
    }
}
