//! Currency rates and news. Both reach the network, so both obey the web switch.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use parking_lot::RwLock;
use regex::Regex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::Http;
use crate::util::{arg_u64, failed, schema, truncate, url_host};
use crate::web::{ensure_enabled, html_to_text};

pub const RATES_URL: &str = "https://www.cbr-xml-daily.ru/daily_json.js";
const DEFAULT_CODES: &[&str] = &["USD", "EUR", "CNY"];

pub struct RatesTool {
    pub http: Arc<dyn Http>,
    pub enabled: Arc<AtomicBool>,
}

fn codes(args: &Value) -> Vec<String> {
    let listed: Vec<String> = arg_str_opt(args, "codes")
        .unwrap_or("")
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|c| c.len() == 3)
        .map(str::to_uppercase)
        .collect();
    if listed.is_empty() {
        DEFAULT_CODES.iter().map(|c| c.to_string()).collect()
    } else {
        listed
    }
}

/// `81.2543` as `81,25`; values under one rouble keep four digits: `0,1550`.
fn money(v: f64, small: bool) -> String {
    if small {
        format!("{v:.4}").replace('.', ",")
    } else {
        format!("{v:.2}").replace('.', ",")
    }
}

/// Lines for the asked currencies from the Central Bank answer.
pub fn format_rates(body: &str, codes: &[String]) -> Result<String, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("непонятный ответ ЦБ: {e}"))?;
    let date = v["Date"].as_str().unwrap_or("");
    let mut lines = vec![format!("Курсы ЦБ РФ на {}:", &date[..10.min(date.len())])];
    for code in codes {
        match v["Valute"].get(code) {
            Some(c) => {
                let nominal = c["Nominal"].as_f64().unwrap_or(1.0).max(1.0);
                let value = c["Value"].as_f64().unwrap_or_default() / nominal;
                let prev = c["Previous"].as_f64().unwrap_or_default() / nominal;
                let diff = value - prev;
                let sign = if diff >= 0.0 { "+" } else { "−" };
                lines.push(format!(
                    "{code} ({}): {} ₽ ({sign}{} за день)",
                    c["Name"].as_str().unwrap_or(""),
                    money(value, value < 1.0),
                    money(diff.abs(), value < 1.0)
                ));
            }
            None => lines.push(format!("{code}: нет в списке ЦБ")),
        }
    }
    Ok(lines.join("\n"))
}

#[async_trait]
impl Tool for RatesTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "get_rates",
            "Официальные курсы валют ЦБ РФ к рублю. Без codes — доллар, евро и юань.",
            schema(
                &[("codes", "string", "Коды валют через запятую: USD, EUR, KZT")],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Узнать курсы валют",
            codes(args).join(", "),
            "запрос уйдёт на cbr-xml-daily.ru",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        ensure_enabled(&self.enabled)?;
        let body = self.http.get(RATES_URL).await.map_err(failed)?;
        format_rates(&body, &codes(&args))
            .map(ToolOutput::new)
            .map_err(failed)
    }
}

pub struct NewsTool {
    pub http: Arc<dyn Http>,
    pub enabled: Arc<AtomicBool>,
    /// RSS address from the settings.
    pub feed: Arc<RwLock<String>>,
}

fn tag(item: &str, name: &str) -> String {
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let Some(start) = item.find(&open) else {
        return String::new();
    };
    let Some(body_start) = item[start..].find('>').map(|i| start + i + 1) else {
        return String::new();
    };
    let Some(end) = item[body_start..].find(&close).map(|i| body_start + i) else {
        return String::new();
    };
    let raw = item[body_start..end].trim();
    let raw = raw
        .strip_prefix("<![CDATA[")
        .and_then(|r| r.strip_suffix("]]>"))
        .unwrap_or(raw);
    html_to_text(raw).replace('\n', " ")
}

/// (title, link) of each RSS or Atom item, in feed order.
pub fn parse_feed(xml: &str) -> Vec<(String, String)> {
    static ITEM: OnceLock<Regex> = OnceLock::new();
    static HREF: OnceLock<Regex> = OnceLock::new();
    let item =
        ITEM.get_or_init(|| Regex::new(r"(?s)<(item|entry)[\s>].*?</(item|entry)>").unwrap());
    let href = HREF.get_or_init(|| Regex::new(r#"<link[^>]*href="([^"]+)""#).unwrap());
    item.find_iter(xml)
        .filter_map(|m| {
            let body = m.as_str();
            let title = tag(body, "title");
            let mut link = tag(body, "link");
            if link.is_empty() {
                link = href
                    .captures(body)
                    .map(|c| c[1].to_string())
                    .unwrap_or_default();
            }
            (!title.is_empty()).then_some((title, link))
        })
        .collect()
}

#[async_trait]
impl Tool for NewsTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "get_news",
            "Свежие новости: заголовки и ссылки из новостной ленты. С query — только заголовки с этими словами.",
            schema(
                &[
                    ("query", "string", "Слова в заголовке, необязательно"),
                    ("count", "integer", "Сколько новостей, до 15"),
                ],
                &[],
            ),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        let feed = self.feed.read().clone();
        let host = url_host(&feed).unwrap_or_else(|| feed.clone());
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть новости",
            host.clone(),
            "только чтение из интернета",
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
        let feed = self.feed.read().trim().to_string();
        if url_host(&feed).is_none() {
            return Err(failed(
                "в настройках не задан адрес новостной ленты (http или https)",
            ));
        }
        let count = arg_u64(&args, "count").unwrap_or(7).clamp(1, 15) as usize;
        let words: Vec<String> = arg_str_opt(&args, "query")
            .unwrap_or("")
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let xml = self.http.get(&feed).await.map_err(failed)?;
        let lines: Vec<String> = parse_feed(&xml)
            .into_iter()
            .filter(|(title, _)| {
                let t = title.to_lowercase();
                words.iter().all(|w| t.contains(w))
            })
            .take(count)
            .map(|(title, link)| format!("{}\n{link}", truncate(&title, 200)))
            .collect();
        if lines.is_empty() {
            return Ok(ToolOutput::new("Подходящих новостей в ленте нет."));
        }
        Ok(ToolOutput::new(lines.join("\n\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env, run};
    use serde_json::json;
    use std::sync::atomic::Ordering;

    const CBR: &str = r#"{"Date":"2026-11-11T11:30:00+03:00","Valute":{
        "USD":{"CharCode":"USD","Nominal":1,"Name":"Доллар США","Value":81.2543,"Previous":80.9},
        "KZT":{"CharCode":"KZT","Nominal":100,"Name":"Казахстанских тенге","Value":15.5,"Previous":15.6}}}"#;

    const RSS: &str = r#"<?xml version="1.0"?><rss><channel><title>Лента</title>
        <item><title><![CDATA[Учёные нашли воду &amp; лёд]]></title><link>https://n.example/1</link></item>
        <item><title>Курс рубля вырос</title><link>https://n.example/2</link></item>
        </channel></rss>"#;

    #[tokio::test]
    async fn rates_are_per_one_unit_with_daily_change() {
        let (e, f) = env();
        f.http.lock().insert(RATES_URL.into(), CBR.into());
        let tool = RatesTool {
            http: e.http.clone(),
            enabled: e.shared.web_enabled.clone(),
        };
        let out = run(&tool, json!({"codes": "usd, kzt; xyz"}))
            .await
            .unwrap()
            .text;
        assert_eq!(
            out,
            "Курсы ЦБ РФ на 2026-11-11:\n\
             USD (Доллар США): 81,25 ₽ (+0,35 за день)\n\
             KZT (Казахстанских тенге): 0,1550 ₽ (−0,0010 за день)\n\
             XYZ: нет в списке ЦБ"
        );
        assert_eq!(tool.assess(&json!({})).unwrap().target, "USD, EUR, CNY");
    }

    #[tokio::test]
    async fn news_lists_titles_and_filters_by_words() {
        let (e, f) = env();
        f.http
            .lock()
            .insert("https://feed.example/rss".into(), RSS.into());
        let tool = NewsTool {
            http: e.http.clone(),
            enabled: e.shared.web_enabled.clone(),
            feed: Arc::new(RwLock::new("https://feed.example/rss".into())),
        };
        let all = run(&tool, json!({})).await.unwrap().text;
        assert_eq!(
            all,
            "Учёные нашли воду & лёд\nhttps://n.example/1\n\nКурс рубля вырос\nhttps://n.example/2"
        );
        let one = run(&tool, json!({"query": "РУБЛЯ"})).await.unwrap().text;
        assert_eq!(one, "Курс рубля вырос\nhttps://n.example/2");
        assert_eq!(
            run(&tool, json!({"query": "футбол"})).await.unwrap().text,
            "Подходящих новостей в ленте нет."
        );
        assert_eq!(tool.assess(&json!({})).unwrap().scope, "feed.example");
    }

    #[tokio::test]
    async fn both_refuse_while_web_tools_are_off() {
        let (e, f) = env();
        e.shared.web_enabled.store(false, Ordering::Relaxed);
        let rates = RatesTool {
            http: e.http.clone(),
            enabled: e.shared.web_enabled.clone(),
        };
        let news = NewsTool {
            http: e.http.clone(),
            enabled: e.shared.web_enabled.clone(),
            feed: Arc::new(RwLock::new("https://feed.example/rss".into())),
        };
        assert!(run(&rates, json!({})).await.is_err());
        assert!(run(&news, json!({})).await.is_err());
        assert!(f.http_calls.lock().is_empty());
    }

    #[test]
    fn atom_entries_use_link_href() {
        let atom = r#"<feed><entry><title>Запуск</title><link rel="alternate" href="https://a.example/x"/></entry></feed>"#;
        assert_eq!(
            parse_feed(atom),
            vec![("Запуск".to_string(), "https://a.example/x".to_string())]
        );
    }
}
