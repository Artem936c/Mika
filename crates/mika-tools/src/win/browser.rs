//! Edge or Chrome in a profile of its own, driven over the DevTools protocol.
//!
//! The browser starts with `--remote-debugging-port` on a free port of 127.0.0.1 and
//! `--user-data-dir` inside the Mika data folder, so the user's own browser and
//! profile are never touched. Each action opens a WebSocket to the page, runs the
//! commands and reads the page again.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use crate::sys::{Browser, Page};

const START_WAIT: Duration = Duration::from_secs(15);
const LOAD_WAIT: Duration = Duration::from_secs(20);
const CALL_WAIT: Duration = Duration::from_secs(30);
/// A script on a page that is busy or leaving answers in seconds or never.
const EVAL_WAIT: Duration = Duration::from_secs(10);

/// Collects the page: address, title, visible text and numbered elements. Elements get
/// a `data-mika-n` attribute so a later click finds the same one. Password values are
/// never read.
const SNAPSHOT_JS: &str = r#"(() => {
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return false;
    const s = getComputedStyle(el);
    return s.visibility !== 'hidden' && s.display !== 'none';
  };
  document.querySelectorAll('[data-mika-n]').forEach((e) => e.removeAttribute('data-mika-n'));
  const sel = 'a[href],button,input:not([type=hidden]),textarea,select,summary,[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[role=option],[contenteditable=""],[contenteditable=true]';
  const items = [];
  let n = 0;
  for (const el of document.querySelectorAll(sel)) {
    if (items.length >= 200) break;
    if (el.disabled || !visible(el)) continue;
    n += 1;
    el.setAttribute('data-mika-n', String(n));
    const tag = el.tagName.toLowerCase();
    const type = (el.getAttribute('type') || '').toLowerCase();
    const role = el.getAttribute('role') || '';
    let kind = role || tag;
    if (tag === 'a' || role === 'link') kind = 'ссылка';
    else if (tag === 'button' || role === 'button' || type === 'submit' || type === 'button') kind = 'кнопка';
    else if (tag === 'select') kind = 'список';
    else if (type === 'checkbox' || type === 'radio' || role === 'checkbox') kind = 'флажок';
    else if (tag === 'input' || tag === 'textarea' || el.isContentEditable) kind = 'поле';
    const attr = (k) => el.getAttribute(k) || '';
    let label = attr('aria-label') || (tag === 'input' ? '' : el.innerText) || attr('placeholder') || attr('title') || attr('alt') || attr('name');
    if (kind === 'поле' && type !== 'password' && el.value) {
      label = (attr('aria-label') || attr('placeholder') || attr('name')) + ' = ' + String(el.value).slice(0, 60);
    } else if (tag === 'select' && el.selectedIndex >= 0) {
      label = (attr('aria-label') || attr('name') || 'список') + ' = ' + el.options[el.selectedIndex].text;
    } else if (kind === 'кнопка' && !label && el.value) {
      label = String(el.value);
    }
    items.push({ n, kind, label: String(label || '').replace(/\s+/g, ' ').trim().slice(0, 80) });
  }
  const text = (document.body ? document.body.innerText : '').replace(/\n{3,}/g, '\n\n').trim().slice(0, 8000);
  return {
    url: location.href,
    title: document.title,
    text,
    items,
    password: !!document.querySelector('input[type=password]'),
  };
})()"#;

fn find_js(n: u32, then: &str) -> String {
    format!(
        r#"(() => {{
  const el = document.querySelector('[data-mika-n="{n}"]');
  if (!el) return null;
  el.scrollIntoView({{ block: 'center', inline: 'center' }});
  {then}
}})()"#
    )
}

/// Where Edge or Chrome is installed. Edge comes with Windows.
fn browser_exe() -> Option<PathBuf> {
    let env = |k: &str| std::env::var(k).ok().map(PathBuf::from);
    let mut tries = Vec::new();
    for base in [env("ProgramFiles(x86)"), env("ProgramFiles")]
        .into_iter()
        .flatten()
    {
        tries.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
    }
    for base in [
        env("ProgramFiles"),
        env("ProgramFiles(x86)"),
        env("LOCALAPPDATA"),
    ]
    .into_iter()
    .flatten()
    {
        tries.push(base.join(r"Google\Chrome\Application\chrome.exe"));
    }
    tries.into_iter().find(|p| p.exists())
}

/// One WebSocket to one page.
struct Conn {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    id: u64,
}

impl Conn {
    async fn open(url: &str) -> Result<Self, String> {
        let (ws, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .map_err(|_| "браузер не отвечает".to_string())?
        .map_err(|e| format!("нет связи с браузером: {e}"))?;
        Ok(Self { ws, id: 0 })
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.call_within(method, params, CALL_WAIT).await
    }

    async fn call_within(
        &mut self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<Value, String> {
        self.id += 1;
        let id = self.id;
        let msg = json!({ "id": id, "method": method, "params": params }).to_string();
        self.ws
            .send(Message::Text(msg.into()))
            .await
            .map_err(|e| format!("браузер закрыл связь: {e}"))?;
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let next = tokio::time::timeout(left, self.ws.next())
                .await
                .map_err(|_| format!("браузер не ответил на {method}"))?;
            let Some(frame) = next else {
                return Err("браузер закрыл связь".into());
            };
            let Message::Text(text) = frame.map_err(|e| e.to_string())? else {
                continue;
            };
            let v: Value = serde_json::from_str(text.as_str()).map_err(|e| e.to_string())?;
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // an event
            }
            if let Some(err) = v.get("error") {
                return Err(format!(
                    "браузер: {}",
                    err.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("ошибка")
                ));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn eval(&mut self, expr: &str) -> Result<Value, String> {
        let r = self
            .call_within(
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true, "awaitPromise": true }),
                EVAL_WAIT,
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let text = ex
                .pointer("/exception/description")
                .or_else(|| ex.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("ошибка скрипта");
            return Err(format!("страница: {}", text.lines().next().unwrap_or("")));
        }
        Ok(r.pointer("/result/value").cloned().unwrap_or(Value::Null))
    }

    /// Waits for the document to load, then a moment for scripts to draw.
    async fn settle(&mut self) {
        let deadline = Instant::now() + LOAD_WAIT;
        tokio::time::sleep(Duration::from_millis(300)).await;
        while Instant::now() < deadline {
            match self.eval("document.readyState").await {
                Ok(Value::String(s)) if s == "complete" => break,
                // A navigation in progress drops the context for a moment.
                _ => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    async fn snapshot(&mut self) -> Result<Page, String> {
        let v = self.eval(SNAPSHOT_JS).await?;
        serde_json::from_value(v).map_err(|e| format!("не разобрала страницу: {e}"))
    }

    async fn mouse_click(&mut self, x: f64, y: f64) -> Result<(), String> {
        for kind in ["mousePressed", "mouseReleased"] {
            self.call(
                "Input.dispatchMouseEvent",
                json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 }),
            )
            .await?;
        }
        Ok(())
    }
}

pub struct CdpBrowser {
    profile: PathBuf,
    port: Mutex<Option<u16>>,
    last: Mutex<Option<Page>>,
    /// Target id of the tab Mika works in; a click that opens a tab moves here.
    tab: Mutex<Option<String>>,
    http: reqwest::Client,
    headless: bool,
}

/// A page tab: target id and WebSocket address.
struct Tab {
    id: String,
    socket: String,
}

impl CdpBrowser {
    pub fn new(profile: PathBuf) -> Self {
        Self {
            profile,
            port: Mutex::new(None),
            last: Mutex::new(None),
            tab: Mutex::new(None),
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            headless: false,
        }
    }

    /// Without a window: for tests on a machine nobody watches.
    #[cfg(test)]
    fn headless(mut self) -> Self {
        self.headless = true;
        self
    }

    /// Asks the browser to quit. Tests only: the user closes the real one.
    #[cfg(test)]
    async fn quit(&self) {
        let Some(port) = *self.port.lock() else {
            return;
        };
        let Ok(r) = self
            .http
            .get(format!("http://127.0.0.1:{port}/json/version"))
            .send()
            .await
        else {
            return;
        };
        let Ok(v) = r.json::<Value>().await else {
            return;
        };
        if let Some(url) = v.get("webSocketDebuggerUrl").and_then(Value::as_str) {
            if let Ok(mut c) = Conn::open(url).await {
                let _ = c.call("Browser.close", json!({})).await;
            }
        }
    }

    async fn targets(&self, port: u16) -> Result<Vec<Value>, String> {
        let r = self
            .http
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        r.json::<Vec<Value>>().await.map_err(|e| e.to_string())
    }

    /// Port the running browser of this profile listens on. The browser writes it
    /// into the profile when started with port 0.
    fn active_port(&self) -> Option<u16> {
        std::fs::read_to_string(self.profile.join("DevToolsActivePort"))
            .ok()
            .and_then(|s| s.lines().next().and_then(|l| l.trim().parse().ok()))
    }

    fn launch(&self) -> Result<(), String> {
        let exe = browser_exe().ok_or("не нашла Edge или Chrome на этом компьютере")?;
        std::fs::create_dir_all(&self.profile).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(self.profile.join("DevToolsActivePort"));
        let mut args = vec![
            "--remote-debugging-port=0".to_string(),
            "--remote-allow-origins=http://127.0.0.1".to_string(),
            format!("--user-data-dir=\"{}\"", self.profile.display()),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            "--disable-features=msEdgeSidebarV2".to_string(),
            // Sites hide their search box in a narrow window (Wikipedia
            // does below 1000 px): a desktop width keeps it on the page.
            "--window-size=1280,900".to_string(),
        ];
        if self.headless {
            args.insert(0, "--headless=new".to_string());
        }
        args.push("about:blank".to_string());
        // Through the shell, not CreateProcess with inherited handles: the
        // browser outlives the turn and must not hold Mika's pipes open.
        use crate::sys::Launcher;
        super::launcher::ShellLauncher
            .open(&exe.display().to_string(), Some(&args.join(" ")))
            .map_err(|e| format!("не смогла запустить браузер: {e}"))?;
        log::info!("browser started: {}", exe.display());
        Ok(())
    }

    /// Port of the running browser, starting it when it is not running. A browser
    /// left open by an earlier run of Mika is reused.
    async fn port(&self) -> Result<u16, String> {
        let known = (*self.port.lock()).or_else(|| self.active_port());
        if let Some(p) = known {
            if self.targets(p).await.is_ok() {
                *self.port.lock() = Some(p);
                return Ok(p);
            }
        }
        self.launch()?;
        let deadline = Instant::now() + START_WAIT;
        loop {
            if let Some(p) = self.active_port() {
                if self.targets(p).await.is_ok() {
                    *self.port.lock() = Some(p);
                    return Ok(p);
                }
            }
            if Instant::now() > deadline {
                return Err(
                    "браузер Мики не ответил: если его окно открыто, закрой его и повтори".into(),
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Page tabs of the running browser, opening one when there is none.
    async fn tabs(&self) -> Result<Vec<Tab>, String> {
        let port = self.port().await?;
        let read = |list: Vec<Value>| -> Vec<Tab> {
            list.into_iter()
                .filter(|t| t.get("type").and_then(Value::as_str) == Some("page"))
                .filter_map(|t| {
                    Some(Tab {
                        id: t.get("id")?.as_str()?.to_string(),
                        socket: t.get("webSocketDebuggerUrl")?.as_str()?.to_string(),
                    })
                })
                .collect()
        };
        let tabs = read(self.targets(port).await?);
        if !tabs.is_empty() {
            return Ok(tabs);
        }
        let _ = self
            .http
            .put(format!("http://127.0.0.1:{port}/json/new?about:blank"))
            .send()
            .await;
        let tabs = read(self.targets(port).await?);
        if tabs.is_empty() {
            return Err("в браузере нет вкладки".into());
        }
        Ok(tabs)
    }

    /// Connection to Mika's tab: the remembered one while it is open, else the first.
    async fn conn(&self) -> Result<Conn, String> {
        let tabs = self.tabs().await?;
        let want = self.tab.lock().clone();
        let tab = tabs
            .iter()
            .find(|t| Some(&t.id) == want.as_ref())
            .unwrap_or(&tabs[0]);
        *self.tab.lock() = Some(tab.id.clone());
        Conn::open(&tab.socket).await
    }

    /// A tab that appeared after `before` was listed, made Mika's tab.
    async fn new_tab(&self, before: &[String]) -> Option<Conn> {
        let tabs = self.tabs().await.ok()?;
        let fresh = tabs.into_iter().find(|t| !before.contains(&t.id))?;
        *self.tab.lock() = Some(fresh.id.clone());
        let mut c = Conn::open(&fresh.socket).await.ok()?;
        let _ = c.call("Page.bringToFront", json!({})).await;
        Some(c)
    }

    fn keep(&self, page: Page) -> Page {
        *self.last.lock() = Some(page.clone());
        page
    }

    async fn center_of(conn: &mut Conn, n: u32) -> Result<(f64, f64), String> {
        let v = conn
            .eval(&find_js(
                n,
                "const r = el.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2];",
            ))
            .await?;
        let xy = v
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or_else(|| format!("на странице нет элемента {n}: прочитай её заново"))?;
        Ok((xy[0].as_f64().unwrap_or(0.0), xy[1].as_f64().unwrap_or(0.0)))
    }
}

#[async_trait]
impl Browser for CdpBrowser {
    fn last(&self) -> Option<Page> {
        self.last.lock().clone()
    }

    async fn open(&self, url: &str) -> Result<Page, String> {
        let mut c = self.conn().await?;
        let _ = c.call("Page.bringToFront", json!({})).await;
        let r = c.call("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = r.get("errorText").and_then(Value::as_str) {
            return Err(format!("страница не открылась: {err}"));
        }
        c.settle().await;
        Ok(self.keep(c.snapshot().await?))
    }

    async fn read(&self, wait_for: Option<&str>, wait: Duration) -> Result<Page, String> {
        let mut c = self.conn().await?;
        let deadline = Instant::now() + wait;
        let want = wait_for.map(str::to_lowercase);
        loop {
            let page = c.snapshot().await?;
            let found = want
                .as_ref()
                .is_none_or(|w| page.text.to_lowercase().contains(w));
            if found || Instant::now() >= deadline {
                return Ok(self.keep(page));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn back(&self) -> Result<Page, String> {
        let mut c = self.conn().await?;
        c.eval("history.back()").await?;
        c.settle().await;
        Ok(self.keep(c.snapshot().await?))
    }

    async fn click(&self, n: u32) -> Result<Page, String> {
        let mut c = self.conn().await?;
        let before: Vec<String> = self.tabs().await?.into_iter().map(|t| t.id).collect();
        let (x, y) = Self::center_of(&mut c, n).await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        c.mouse_click(x, y).await?;
        tokio::time::sleep(Duration::from_millis(600)).await;
        if let Some(mut tab) = self.new_tab(&before).await {
            tab.settle().await;
            return Ok(self.keep(tab.snapshot().await?));
        }
        c.settle().await;
        Ok(self.keep(c.snapshot().await?))
    }

    async fn type_into(&self, n: u32, text: &str, submit: bool) -> Result<Page, String> {
        let mut c = self.conn().await?;
        let wanted = serde_json::to_string(&text.to_lowercase()).map_err(|e| e.to_string())?;
        let chosen = c
            .eval(&find_js(
                n,
                &format!(
                    "if (el.tagName !== 'SELECT') return 'not-a-list'; \
                     const want = {wanted}; \
                     const opts = Array.from(el.options); \
                     const opt = opts.find((o) => o.text.trim().toLowerCase() === want) \
                       || opts.find((o) => o.text.toLowerCase().includes(want)) \
                       || opts.find((o) => o.value.toLowerCase() === want); \
                     if (!opt) return 'no-option'; \
                     el.value = opt.value; \
                     el.dispatchEvent(new Event('input', {{ bubbles: true }})); \
                     el.dispatchEvent(new Event('change', {{ bubbles: true }})); \
                     return 'chosen';"
                ),
            ))
            .await?;
        match chosen.as_str() {
            Some("chosen") => {
                c.settle().await;
                return Ok(self.keep(c.snapshot().await?));
            }
            Some("no-option") => {
                return Err(format!(
                    "в списке {n} нет пункта «{text}»: прочитай страницу заново"
                ))
            }
            _ => {}
        }
        let focused = c
            .eval(&find_js(
                n,
                "const t = (el.getAttribute('type') || 'text').toLowerCase(); \
                 const typing = el.isContentEditable || el.tagName === 'TEXTAREA' || (el.tagName === 'INPUT' && \
                   ['text','search','email','url','tel','number','password',''].includes(t)); \
                 if (!typing) return 'not-a-field'; \
                 el.focus(); if (typeof el.select === 'function') el.select(); \
                 else if (el.isContentEditable) document.execCommand('selectAll'); \
                 return document.activeElement === el || el.contains(document.activeElement);",
            ))
            .await?;
        if focused.as_str() == Some("not-a-field") {
            return Err(format!(
                "элемент {n} не поле для текста (флажок, кнопка или ссылка): возьми из browser_read номер с видом «поле»"
            ));
        }
        if focused != Value::Bool(true) {
            return Err(format!(
                "поле {n} не нашлось или не принимает ввод: прочитай страницу заново"
            ));
        }
        let before = c.eval("location.href").await.unwrap_or(Value::Null);
        c.call("Input.insertText", json!({ "text": text })).await?;
        if submit {
            for kind in ["keyDown", "keyUp"] {
                c.call(
                    "Input.dispatchKeyEvent",
                    json!({
                        "type": kind, "key": "Enter", "code": "Enter",
                        "windowsVirtualKeyCode": 13, "nativeVirtualKeyCode": 13,
                        "text": if kind == "keyDown" { "\r" } else { "" },
                    }),
                )
                .await?;
            }
            // A search box that waits for its own suggestions ignores Enter:
            // when the page stayed, its form is sent the way a button would.
            tokio::time::sleep(Duration::from_millis(800)).await;
            let now = c.eval("location.href").await.unwrap_or(Value::Null);
            if now == before {
                let _ = c
                    .eval(&find_js(
                        n,
                        "const f = el.form; if (!f) return false; \
                         if (typeof f.requestSubmit === 'function') f.requestSubmit(); else f.submit(); \
                         return true;",
                    ))
                    .await;
            }
        }
        c.settle().await;
        Ok(self.keep(c.snapshot().await?))
    }

    async fn scroll(&self, down: bool) -> Result<Page, String> {
        let dir = if down { 1 } else { -1 };
        let script = format!("window.scrollBy(0, {dir} * innerHeight * 0.8)");
        // A page that was leaving may not answer: one more try on a fresh connection.
        let mut c = self.conn().await?;
        if c.eval(&script).await.is_err() {
            c = self.conn().await?;
            c.eval(&script).await?;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        Ok(self.keep(c.snapshot().await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives a real headless Edge or Chrome. Run by hand: `cargo test -p mika-tools live_browser -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_browser_reads_types_and_clicks() {
        let dir = tempfile::tempdir().unwrap();
        let b = CdpBrowser::new(dir.path().join("profile")).headless();
        let html = "data:text/html;charset=utf-8,<title>Тест</title><h1>Привет</h1><input id=q placeholder=Поиск><button onclick=\"document.title='нажата '+q.value\">Найти</button><a href=%23x>Ссылка</a>\
<select name=city><option>Москва</option><option>Пермь</option></select>\
<a href=about:blank target=_blank>Новая вкладка</a>";
        let page = b.open(html).await.unwrap();
        assert_eq!(page.title, "Тест");
        assert!(page.text.contains("Привет"));
        let field = page.items.iter().find(|i| i.kind == "поле").unwrap().n;
        let button = page.items.iter().find(|i| i.kind == "кнопка").unwrap().n;
        let typed = b.type_into(field, "кошки", false).await.unwrap();
        assert!(
            typed.items.iter().any(|i| i.label.contains("кошки")),
            "{:?}",
            typed.items
        );
        let clicked = b.click(button).await.unwrap();
        assert_eq!(clicked.title, "нажата кошки");
        let list = clicked.items.iter().find(|i| i.kind == "список").unwrap().n;
        let chosen = b.type_into(list, "пермь", false).await.unwrap();
        assert!(
            chosen.items.iter().any(|i| i.label == "city = Пермь"),
            "{:?}",
            chosen.items
        );
        assert!(b.type_into(list, "Казань", false).await.is_err());
        let link = chosen
            .items
            .iter()
            .find(|i| i.label == "Новая вкладка")
            .unwrap()
            .n;
        let opened = b.click(link).await.unwrap();
        assert_eq!(opened.url, "about:blank", "moved to the new tab");
        let back = b.read(None, Duration::ZERO).await.unwrap();
        assert_eq!(back.url, "about:blank");
        // A search box that swallows Enter still sends its form; a flag is no field.
        let search = b
            .open(
                "data:text/html;charset=utf-8,<title>Поиск</title><input type=checkbox id=c>\
<form action=\"about:blank\"><input name=s onkeydown=\"if(event.key==='Enter')event.preventDefault()\"></form>",
            )
            .await
            .unwrap();
        let flag = search.items.iter().find(|i| i.kind == "флажок").unwrap().n;
        assert!(b.type_into(flag, "Пермь", true).await.is_err());
        let field = search.items.iter().find(|i| i.kind == "поле").unwrap().n;
        let sent = b.type_into(field, "Пермь", true).await.unwrap();
        assert!(sent.url.starts_with("about:blank?s="), "{}", sent.url);
        let login = b
            .open("data:text/html,<title>x</title><input type=password value=secret>")
            .await
            .unwrap();
        b.quit().await;
        assert!(login.password);
        assert!(!login.text.contains("secret"));
        assert!(login.items.iter().all(|i| !i.label.contains("secret")));
    }
}
