//! Internet: open a link, search, read a page, weather.
//! Network tools work only when the user turned them on in settings.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use regex::Regex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::{Http, Launcher};
use crate::util::{failed, schema, truncate, url_decode, url_encode, url_host};

pub const WEB_DISABLED: &str =
    "Интернет-инструменты выключены. Их можно включить в настройках Мики, вкладка «Инструменты».";

fn ensure_enabled(flag: &AtomicBool) -> Result<(), ToolError> {
    if flag.load(Ordering::Relaxed) {
        Ok(())
    } else {
        Err(failed(WEB_DISABLED))
    }
}

fn http_url(args: &Value) -> Result<(String, String), ToolError> {
    let url = arg_str(args, "url")?.trim().to_string();
    let host = url_host(&url)
        .ok_or_else(|| ToolError::BadArguments("нужна ссылка http:// или https://".into()))?;
    Ok((url, host))
}

pub struct OpenUrlTool(pub Arc<dyn Launcher>);

#[async_trait]
impl Tool for OpenUrlTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "open_url",
            "Открыть ссылку в браузере по умолчанию. Для поиска в браузере: https://yandex.ru/search/?text=<запрос>, для погоды: https://yandex.ru/pogoda/<город>.",
            schema(&[("url", "string", "Ссылка http(s)")], &["url"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let (url, host) = http_url(args)?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Открыть сайт в браузере",
            url,
            "браузер откроет страницу",
        )
        .with_scope(host))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let (url, _) = http_url(&args)?;
        self.0.open(&url, None).map_err(failed)?;
        Ok(ToolOutput::new(format!("Открыла {url}")))
    }
}

pub fn html_to_text(html: &str) -> String {
    static DROP: OnceLock<Regex> = OnceLock::new();
    static TAGS: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    let drop = DROP.get_or_init(|| {
        Regex::new(r"(?is)<(script|style|noscript|svg|head)[^>]*>.*?</(script|style|noscript|svg|head)>|<!--.*?-->").unwrap()
    });
    let block = BLOCK.get_or_init(|| {
        Regex::new(r"(?i)</?(p|div|br|li|h[1-6]|tr|section|article)[^>]*>").unwrap()
    });
    let tags = TAGS.get_or_init(|| Regex::new(r"(?s)<[^>]+>").unwrap());
    let spaces = SPACES.get_or_init(|| Regex::new(r"[ \t\r\f]+").unwrap());
    let t = drop.replace_all(html, " ");
    let t = block.replace_all(&t, "\n");
    let t = tags.replace_all(&t, "");
    let t = decode_entities(&t);
    let t = spaces.replace_all(&t, " ");
    t.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&laquo;", "«")
        .replace("&raquo;", "»")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&amp;", "&")
}

pub struct FetchPageTool {
    pub http: Arc<dyn Http>,
    pub enabled: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for FetchPageTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "fetch_page",
            "Скачать страницу по ссылке и вернуть её текст без разметки.",
            schema(&[("url", "string", "Ссылка http(s)")], &["url"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let (url, host) = http_url(args)?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Прочитать страницу",
            url,
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
        let (url, _) = http_url(&args)?;
        let html = self.http.get(&url).await.map_err(failed)?;
        Ok(ToolOutput::new(truncate(&html_to_text(&html), 8000)))
    }
}

pub struct WebSearchTool {
    pub http: Arc<dyn Http>,
    pub enabled: Arc<AtomicBool>,
}

/// (title, url, snippet) from the DuckDuckGo HTML page.
pub fn parse_duckduckgo(html: &str) -> Vec<(String, String, String)> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    static SNIPPET: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| {
        Regex::new(r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#).unwrap()
    });
    let snippet = SNIPPET
        .get_or_init(|| Regex::new(r#"(?s)class="result__snippet"[^>]*>(.*?)</a>"#).unwrap());
    let snippets: Vec<String> = snippet
        .captures_iter(html)
        .map(|c| html_to_text(&c[1]))
        .collect();
    link.captures_iter(html)
        .enumerate()
        .map(|(i, c)| {
            let mut href = c[1].replace("&amp;", "&");
            if let Some(pos) = href.find("uddg=") {
                let enc = href[pos + 5..].split('&').next().unwrap_or("");
                href = url_decode(enc);
            }
            (
                html_to_text(&c[2]),
                href,
                snippets.get(i).cloned().unwrap_or_default(),
            )
        })
        .collect()
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "web_search",
            "Поиск в интернете. Возвращает заголовки, ссылки и короткие описания.",
            schema(&[("query", "string", "Запрос")], &["query"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let q = arg_str(args, "query")?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Поискать в интернете",
            q,
            "запрос уйдёт в DuckDuckGo",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        ensure_enabled(&self.enabled)?;
        let q = arg_str(&args, "query")?;
        let html = self
            .http
            .get(&format!(
                "https://html.duckduckgo.com/html/?q={}",
                url_encode(q)
            ))
            .await
            .map_err(failed)?;
        let results = parse_duckduckgo(&html);
        if results.is_empty() {
            return Err(failed(
                "поиск не вернул результатов или изменилась страница поиска",
            ));
        }
        let lines: Vec<String> = results
            .iter()
            .take(6)
            .map(|(t, u, s)| format!("{t}\n{u}\n{s}"))
            .collect();
        Ok(ToolOutput::new(lines.join("\n\n")))
    }
}

pub struct WeatherTool {
    pub http: Arc<dyn Http>,
    pub enabled: Arc<AtomicBool>,
}

pub fn weather_code(code: i64) -> &'static str {
    match code {
        0 => "ясно",
        1 => "в основном ясно",
        2 => "переменная облачность",
        3 => "пасмурно",
        45 | 48 => "туман",
        51 | 53 | 55 => "морось",
        56 | 57 => "ледяная морось",
        61 => "небольшой дождь",
        63 => "дождь",
        65 => "сильный дождь",
        66 | 67 => "ледяной дождь",
        71 => "небольшой снег",
        73 => "снег",
        75 => "сильный снег",
        77 => "снежная крупа",
        80..=82 => "ливень",
        85 | 86 => "снегопад",
        95 => "гроза",
        96 | 99 => "гроза с градом",
        _ => "без данных",
    }
}

#[async_trait]
impl Tool for WeatherTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "get_weather",
            "Погода сейчас и на три дня для города.",
            schema(&[("city", "string", "Город, например «Москва»")], &["city"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let city = arg_str(args, "city")?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Узнать погоду",
            city,
            "запрос уйдёт в Open-Meteo",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        ensure_enabled(&self.enabled)?;
        let city = arg_str(&args, "city")?.trim();
        let geo: Value = serde_json::from_str(
            &self
                .http
                .get(&format!(
                    "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=ru&format=json",
                    url_encode(city)
                ))
                .await
                .map_err(failed)?,
        )
        .map_err(|e| failed(format!("непонятный ответ геокодера: {e}")))?;
        let place = geo["results"]
            .get(0)
            .ok_or_else(|| failed(format!("не нашла город «{city}»")))?;
        let (lat, lon) = (
            place["latitude"].as_f64().unwrap_or_default(),
            place["longitude"].as_f64().unwrap_or_default(),
        );
        let name = place["name"].as_str().unwrap_or(city);
        let country = place["country"].as_str().unwrap_or("");
        let f: Value = serde_json::from_str(
            &self
                .http
                .get(&format!(
                    "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
&current=temperature_2m,apparent_temperature,weather_code,wind_speed_10m,relative_humidity_2m\
&daily=temperature_2m_max,temperature_2m_min,weather_code,precipitation_probability_max\
&wind_speed_unit=ms&timezone=auto&forecast_days=3"
                ))
                .await
                .map_err(failed)?,
        )
        .map_err(|e| failed(format!("непонятный ответ прогноза: {e}")))?;
        let c = &f["current"];
        let mut text = format!(
            "{name}, {country}. Сейчас {:+.0}°C, ощущается как {:+.0}°C, {}, ветер {:.0} м/с, влажность {:.0}%.",
            c["temperature_2m"].as_f64().unwrap_or_default(),
            c["apparent_temperature"].as_f64().unwrap_or_default(),
            weather_code(c["weather_code"].as_i64().unwrap_or(-1)),
            c["wind_speed_10m"].as_f64().unwrap_or_default(),
            c["relative_humidity_2m"].as_f64().unwrap_or_default(),
        );
        let d = &f["daily"];
        if let Some(days) = d["time"].as_array() {
            for (i, day) in days.iter().enumerate() {
                text.push_str(&format!(
                    "\n{}: от {:+.0} до {:+.0}°C, {}, осадки {}%",
                    day.as_str().unwrap_or(""),
                    d["temperature_2m_min"][i].as_f64().unwrap_or_default(),
                    d["temperature_2m_max"][i].as_f64().unwrap_or_default(),
                    weather_code(d["weather_code"][i].as_i64().unwrap_or(-1)),
                    d["precipitation_probability_max"][i].as_i64().unwrap_or(0),
                ));
            }
        }
        Ok(ToolOutput::new(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env, run};
    use serde_json::json;

    #[tokio::test]
    async fn web_tools_refuse_when_disabled() {
        let (e, f) = env();
        e.web_enabled.store(false, Ordering::Relaxed);
        let w = WeatherTool {
            http: e.http.clone(),
            enabled: e.web_enabled.clone(),
        };
        let err = run(&w, json!({"city": "Москва"})).await.unwrap_err();
        assert_eq!(err.to_string(), WEB_DISABLED);
        assert!(f.http_calls.lock().is_empty());
        // Opening a link in the browser does not use the app's network and still works.
        run(&OpenUrlTool(e.launcher), json!({"url": "https://ya.ru"}))
            .await
            .unwrap();
    }

    #[test]
    fn open_url_rejects_non_http() {
        let (e, _) = env();
        let t = OpenUrlTool(e.launcher);
        assert!(t.assess(&json!({"url": "file:///C:/Windows"})).is_err());
        assert!(t.assess(&json!({"url": "javascript:alert(1)"})).is_err());
        assert_eq!(
            t.assess(&json!({"url": "https://Yandex.ru/pogoda"}))
                .unwrap()
                .scope,
            "yandex.ru"
        );
    }

    #[tokio::test]
    async fn weather_parses_open_meteo() {
        let (e, f) = env();
        f.http.lock().insert(
            "https://geocoding-api.open-meteo.com".into(),
            json!({"results": [{"name": "Москва", "country": "Россия", "latitude": 55.75, "longitude": 37.62}]}).to_string(),
        );
        f.http.lock().insert(
            "https://api.open-meteo.com".into(),
            json!({
                "current": {"temperature_2m": -3.4, "apparent_temperature": -8.0, "weather_code": 71, "wind_speed_10m": 4.2, "relative_humidity_2m": 80},
                "daily": {"time": ["2026-11-11"], "temperature_2m_min": [-6.0], "temperature_2m_max": [-1.0], "weather_code": [3], "precipitation_probability_max": [40]}
            })
            .to_string(),
        );
        let out = run(
            &WeatherTool {
                http: e.http,
                enabled: e.web_enabled,
            },
            json!({"city": "москва"}),
        )
        .await
        .unwrap()
        .text;
        assert!(
            out.starts_with("Москва, Россия. Сейчас -3°C, ощущается как -8°C, небольшой снег"),
            "{out}"
        );
        assert!(
            out.contains("2026-11-11: от -6 до -1°C, пасмурно, осадки 40%"),
            "{out}"
        );
        assert!(f.http_calls.lock()[0].contains("name=%D0%BC%D0%BE%D1%81%D0%BA%D0%B2%D0%B0"));
    }

    #[tokio::test]
    async fn weather_unknown_city_is_clear_error() {
        let (e, f) = env();
        f.http
            .lock()
            .insert("https://geocoding-api.open-meteo.com".into(), "{}".into());
        let err = run(
            &WeatherTool {
                http: e.http,
                enabled: e.web_enabled,
            },
            json!({"city": "Нигдеград"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "не нашла город «Нигдеград»");
    }

    #[test]
    fn search_parses_duckduckgo_html() {
        let html = r#"<div><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&amp;rut=x">Rust <b>Programming</b> Language</a>
            <a class="result__snippet" href="x">A language <b>empowering</b> everyone</a></div>"#;
        assert_eq!(
            parse_duckduckgo(html),
            vec![(
                "Rust Programming Language".into(),
                "https://www.rust-lang.org/".into(),
                "A language empowering everyone".into()
            )]
        );
    }

    #[tokio::test]
    async fn fetch_page_strips_tags() {
        let (e, f) = env();
        f.http.lock().insert(
            "https://example.com".into(),
            "<html><head><title>x</title><style>p{}</style></head><body><h1>Заголовок</h1><p>Текст &amp; ещё</p><script>alert(1)</script></body></html>".into(),
        );
        let out = run(
            &FetchPageTool {
                http: e.http,
                enabled: e.web_enabled,
            },
            json!({"url": "https://example.com/a"}),
        )
        .await
        .unwrap()
        .text;
        assert_eq!(out, "Заголовок\nТекст & ещё");
    }
}
