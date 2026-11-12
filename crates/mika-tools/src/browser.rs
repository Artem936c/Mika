//! The assistant's own browser: open a page, read it, click, type, scroll.
//! It runs in a separate profile, never the user's. Pages with a password field,
//! sign-in, bank or payment pages are in the blind zone: not read, not touched.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::blind::{is_blind_title, is_blind_url, BLIND_REFUSAL};
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::{Browser, Page};
use crate::util::{arg_bool, arg_u64, failed, schema, truncate, url_host};
use crate::web::ensure_enabled;

const MAX_TEXT: usize = 6000;
const MAX_ITEMS: usize = 120;

pub fn is_blind_page(page: &Page) -> bool {
    page.password || is_blind_url(&page.url) || is_blind_title(&page.title)
}

/// Page text for the model: address, numbered elements, then the visible text.
pub fn render_page(page: &Page) -> String {
    if is_blind_page(page) {
        return format!(
            "Открыта страница {}. {BLIND_REFUSAL}. Вход, оплату и пароли пользователь делает сам.",
            url_host(&page.url).unwrap_or_default()
        );
    }
    let mut out = format!("Страница: {}\n{}\n", page.title.trim(), page.url);
    if !page.items.is_empty() {
        out.push_str("\nЭлементы (номер для browser_click и browser_type):\n");
        for it in page.items.iter().take(MAX_ITEMS) {
            let label = if it.label.is_empty() {
                "без подписи"
            } else {
                &it.label
            };
            out.push_str(&format!("[{}] {} «{}»\n", it.n, it.kind, label));
        }
        if page.items.len() > MAX_ITEMS {
            out.push_str(&format!(
                "…и ещё {} элементов ниже, прокрути страницу\n",
                page.items.len() - MAX_ITEMS
            ));
        }
    }
    let text = page.text.trim();
    if !text.is_empty() {
        out.push_str("\nТекст:\n");
        out.push_str(&truncate(text, MAX_TEXT));
    }
    out
}

fn page_host(browser: &dyn Browser) -> String {
    browser
        .last()
        .and_then(|p| url_host(&p.url))
        .unwrap_or_else(|| "страница".into())
}

/// Refuses an action on a page that is in the blind zone.
fn guard(browser: &dyn Browser) -> Result<(), ToolError> {
    match browser.last() {
        Some(p) if is_blind_page(&p) => Err(failed(BLIND_REFUSAL)),
        Some(_) => Ok(()),
        None => Err(failed(
            "браузер ещё не открыт: сначала открой страницу через browser_open",
        )),
    }
}

fn item_label(browser: &dyn Browser, n: u32) -> String {
    browser
        .last()
        .and_then(|p| p.items.into_iter().find(|i| i.n == n))
        .map(|i| format!("{} «{}»", i.kind, i.label))
        .unwrap_or_else(|| format!("элемент {n}"))
}

fn number(args: &Value) -> Result<u32, ToolError> {
    arg_u64(args, "n")
        .filter(|n| *n > 0)
        .map(|n| n as u32)
        .ok_or_else(|| ToolError::BadArguments("нужен номер элемента `n`".into()))
}

fn full_url(raw: &str) -> Result<String, ToolError> {
    let raw = raw.trim();
    let url = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else if raw.contains('.') && !raw.contains(' ') {
        format!("https://{raw}")
    } else {
        return Err(ToolError::BadArguments(
            "нужен адрес сайта, например https://example.com".into(),
        ));
    };
    url_host(&url).ok_or_else(|| ToolError::BadArguments("неверный адрес".into()))?;
    Ok(url)
}

pub struct BrowserOpenTool {
    pub browser: Arc<dyn Browser>,
    pub enabled: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for BrowserOpenTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "browser_open",
            "Открыть адрес в своём браузере Мики (отдельный профиль) и прочитать страницу: текст и пронумерованные кнопки, ссылки, поля. Для поиска: https://duckduckgo.com/?q=<запрос>.",
            schema(&[("url", "string", "Адрес страницы")], &["url"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let url = full_url(arg_str(args, "url")?)?;
        if is_blind_url(&url) {
            return Err(failed(BLIND_REFUSAL));
        }
        let host = url_host(&url).unwrap_or_default();
        Ok(Assessment::new(
            RiskLevel::Low,
            "Открыть страницу в браузере Мики",
            url,
            "только чтение страницы",
        )
        .with_scope(host))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        ensure_enabled(&self.enabled)?;
        let url = full_url(arg_str(&args, "url")?)?;
        let page = self.browser.open(&url).await.map_err(failed)?;
        Ok(ToolOutput::new(render_page(&page)))
    }
}

pub struct BrowserReadTool(pub Arc<dyn Browser>);

#[async_trait]
impl Tool for BrowserReadTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "browser_read",
            "Прочитать текущую страницу браузера Мики заново: текст и пронумерованные элементы. Вызывай после каждого действия на странице.",
            schema(&[], &[]),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Прочитать страницу",
            page_host(self.0.as_ref()),
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let page = self.0.read().await.map_err(failed)?;
        Ok(ToolOutput::new(render_page(&page)))
    }
}

pub struct BrowserClickTool(pub Arc<dyn Browser>);

#[async_trait]
impl Tool for BrowserClickTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "browser_click",
            "Нажать ссылку, кнопку или флажок на странице по номеру из browser_read.",
            schema(&[("n", "integer", "Номер элемента")], &["n"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let n = number(args)?;
        guard(self.0.as_ref())?;
        let host = page_host(self.0.as_ref());
        Ok(Assessment::new(
            RiskLevel::Medium,
            format!("Нажать на сайте {host}"),
            item_label(self.0.as_ref(), n),
            "действие на сайте от имени пользователя",
        )
        .with_scope(host))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let n = number(&args)?;
        guard(self.0.as_ref())?;
        // Numbers change with the page: name the element before the click.
        let what = item_label(self.0.as_ref(), n);
        let page = self.0.click(n).await.map_err(failed)?;
        Ok(ToolOutput::new(format!(
            "Нажала {what}.\n\n{}",
            render_page(&page)
        )))
    }
}

pub struct BrowserTypeTool(pub Arc<dyn Browser>);

#[async_trait]
impl Tool for BrowserTypeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "browser_type",
            "Ввести текст в поле страницы по номеру из browser_read. Старый текст поля заменяется. submit=true нажимает Enter, например чтобы начать поиск.",
            schema(
                &[
                    ("n", "integer", "Номер поля"),
                    ("text", "string", "Текст"),
                    ("submit", "boolean", "Нажать Enter после ввода"),
                ],
                &["n", "text"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let n = number(args)?;
        let text = arg_str(args, "text")?;
        guard(self.0.as_ref())?;
        let host = page_host(self.0.as_ref());
        let submit = arg_bool(args, "submit");
        Ok(Assessment::new(
            RiskLevel::Medium,
            if submit {
                format!("Ввести и отправить на сайте {host}")
            } else {
                format!("Ввести текст на сайте {host}")
            },
            format!(
                "«{}» в {}",
                truncate(text, 120),
                item_label(self.0.as_ref(), n)
            ),
            "текст уходит на сайт",
        )
        .with_scope(host))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let n = number(&args)?;
        let text = arg_str(&args, "text")?;
        guard(self.0.as_ref())?;
        let page = self
            .0
            .type_into(n, text, arg_bool(&args, "submit"))
            .await
            .map_err(failed)?;
        Ok(ToolOutput::new(format!("Ввела.\n\n{}", render_page(&page))))
    }
}

pub struct BrowserScrollTool(pub Arc<dyn Browser>);

#[async_trait]
impl Tool for BrowserScrollTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "browser_scroll",
            "Прокрутить страницу браузера Мики на экран вниз или вверх и прочитать её.",
            schema(&[("direction", "string", "down или up")], &[]),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        guard(self.0.as_ref())?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Прокрутить страницу",
            page_host(self.0.as_ref()),
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        guard(self.0.as_ref())?;
        let down = !matches!(
            args.get("direction").and_then(Value::as_str),
            Some("up" | "вверх")
        );
        let page = self.0.scroll(down).await.map_err(failed)?;
        Ok(ToolOutput::new(render_page(&page)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::PageItem;
    use crate::testing::{env, run};
    use serde_json::json;
    use std::sync::atomic::Ordering;

    fn search_page() -> Page {
        Page {
            url: "https://duckduckgo.com/?q=rust".into(),
            title: "rust at DuckDuckGo".into(),
            text: "Rust is a language".into(),
            items: vec![
                PageItem {
                    n: 1,
                    kind: "поле".into(),
                    label: "Search".into(),
                },
                PageItem {
                    n: 2,
                    kind: "ссылка".into(),
                    label: "Rust Programming Language".into(),
                },
            ],
            password: false,
        }
    }

    #[test]
    fn render_lists_elements_and_text() {
        let text = render_page(&search_page());
        assert!(text.contains("[2] ссылка «Rust Programming Language»"));
        assert!(text.contains("Rust is a language"));
    }

    #[test]
    fn blind_pages_are_not_rendered() {
        let mut p = search_page();
        p.password = true;
        let text = render_page(&p);
        assert!(text.contains("слепой зоне") && !text.contains("Rust is a language"));
        let mut p = search_page();
        p.url = "https://online.sberbank.ru/".into();
        assert!(is_blind_page(&p));
    }

    #[tokio::test]
    async fn open_needs_internet_and_refuses_blind_addresses() {
        let (env, f) = env();
        let tool = BrowserOpenTool {
            browser: env.browser.clone(),
            enabled: env.shared.web_enabled.clone(),
        };
        assert!(tool
            .assess(&json!({"url": "https://example.com/login"}))
            .is_err());
        let a = tool.assess(&json!({"url": "duckduckgo.com"})).unwrap();
        assert_eq!(
            (a.level, a.scope.as_str()),
            (RiskLevel::Low, "duckduckgo.com")
        );
        *f.page.lock() = Some(search_page());
        let out = run(&tool, json!({"url": "duckduckgo.com/?q=rust"}))
            .await
            .unwrap();
        assert!(out.text.contains("[1] поле «Search»"));
        assert_eq!(
            f.browser_log.lock().first().unwrap(),
            "open https://duckduckgo.com/?q=rust"
        );
        env.shared.web_enabled.store(false, Ordering::Relaxed);
        assert!(run(&tool, json!({"url": "https://example.com"}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn click_and_type_are_medium_and_guarded_by_the_blind_zone() {
        let (env, f) = env();
        let click = BrowserClickTool(env.browser.clone());
        assert!(click.assess(&json!({"n": 1})).is_err(), "nothing open yet");
        *f.page.lock() = Some(search_page());
        let a = click.assess(&json!({"n": 2})).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert_eq!(a.scope, "duckduckgo.com");
        assert!(a.target.contains("Rust Programming Language"));

        let typing = BrowserTypeTool(env.browser.clone());
        let a = typing
            .assess(&json!({"n": 1, "text": "погода", "submit": true}))
            .unwrap();
        assert!(a.summary.contains("отправить"));
        run(&typing, json!({"n": 1, "text": "погода", "submit": true}))
            .await
            .unwrap();
        assert!(f
            .browser_log
            .lock()
            .contains(&"type 1 погода true".to_string()));

        f.page.lock().as_mut().unwrap().password = true;
        assert!(typing.assess(&json!({"n": 1, "text": "x"})).is_err());
        assert!(click.assess(&json!({"n": 2})).is_err());
        assert!(BrowserScrollTool(env.browser.clone())
            .assess(&json!({}))
            .is_err());
    }
}
