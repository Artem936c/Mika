//! Time in programs: seconds per program and day, from the window in front.
//! Only process names are kept, never window titles, and only on this computer.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, NaiveDate};
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::storage::rusqlite::params;
use mika_core::storage::{Db, DbError};
use mika_core::tools::{arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::util::{failed, schema};

/// Days of history kept.
pub const KEEP_DAYS: i64 = 60;

#[derive(Clone)]
pub struct UsageStore(pub Arc<Db>);

fn day_text(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

impl UsageStore {
    /// Adds seconds to programs of one day.
    pub fn add(&self, day: NaiveDate, seconds: &HashMap<String, u64>) -> Result<(), DbError> {
        let day = day_text(day);
        self.0.with(|c| {
            for (app, s) in seconds {
                c.execute(
                    "INSERT INTO app_usage (day, app, seconds) VALUES (?1, ?2, ?3)
                     ON CONFLICT(day, app) DO UPDATE SET seconds = seconds + excluded.seconds",
                    params![day, app, *s as i64],
                )?;
            }
            Ok(())
        })
    }

    /// Seconds per program from `from` to `to` inclusive, the most used first.
    pub fn report(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<(String, u64)>, DbError> {
        self.0.with(|c| {
            let mut st = c.prepare(
                "SELECT app, SUM(seconds) AS s FROM app_usage WHERE day >= ?1 AND day <= ?2
                 GROUP BY app ORDER BY s DESC",
            )?;
            let rows = st.query_map(params![day_text(from), day_text(to)], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
            })?;
            rows.collect()
        })
    }

    /// Forgets days older than `KEEP_DAYS` before `today`.
    pub fn prune(&self, today: NaiveDate) -> Result<usize, DbError> {
        let edge = day_text(today - Duration::days(KEEP_DAYS));
        self.0
            .with(|c| c.execute("DELETE FROM app_usage WHERE day < ?1", [edge]))
    }

    pub fn clear(&self) -> Result<usize, DbError> {
        self.0.with(|c| c.execute("DELETE FROM app_usage", []))
    }
}

/// A readable program name from its executable: `chrome.exe` → `Chrome`.
pub fn app_label(process: &str) -> String {
    let p = process.to_lowercase();
    let known = [
        ("chrome.exe", "Chrome"),
        ("msedge.exe", "Edge"),
        ("firefox.exe", "Firefox"),
        ("browser.exe", "Яндекс Браузер"),
        ("telegram.exe", "Telegram"),
        ("winword.exe", "Word"),
        ("excel.exe", "Excel"),
        ("powerpnt.exe", "PowerPoint"),
        ("outlook.exe", "Outlook"),
        ("code.exe", "VS Code"),
        ("explorer.exe", "Проводник"),
        ("steam.exe", "Steam"),
        ("discord.exe", "Discord"),
        ("1cv8.exe", "1С"),
        ("1cv8c.exe", "1С"),
        ("windowsterminal.exe", "Терминал"),
        ("mstsc.exe", "Удалённый рабочий стол"),
    ];
    if let Some((_, name)) = known.iter().find(|(exe, _)| *exe == p) {
        return name.to_string();
    }
    let stem = p.strip_suffix(".exe").unwrap_or(&p);
    let mut chars = stem.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => process.to_string(),
    }
}

/// `1 ч 05 мин`, `12 мин`, `меньше минуты`.
pub fn duration_text(seconds: u64) -> String {
    let minutes = seconds / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "меньше минуты".into(),
        (0, m) => format!("{m} мин"),
        (h, m) => format!("{h} ч {m:02} мин"),
    }
}

/// The days a period covers, ending today.
pub fn period(name: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate, &'static str)> {
    Some(match name.trim() {
        "" | "today" => (today, today, "сегодня"),
        "yesterday" => {
            let y = today - Duration::days(1);
            (y, y, "вчера")
        }
        "week" => (today - Duration::days(6), today, "за неделю"),
        "month" => (today - Duration::days(29), today, "за месяц"),
        _ => return None,
    })
}

/// The report text: the total and the programs that took most of it.
pub fn report_text(rows: &[(String, u64)], label: &str) -> String {
    let total: u64 = rows.iter().map(|r| r.1).sum();
    if total == 0 {
        return format!("Данных {label} нет.");
    }
    let mut merged: Vec<(String, u64)> = Vec::new();
    for (app, s) in rows {
        let name = app_label(app);
        match merged.iter_mut().find(|m| m.0 == name) {
            Some(m) => m.1 += s,
            None => merged.push((name, *s)),
        }
    }
    merged.sort_by_key(|m| std::cmp::Reverse(m.1));
    let lines: Vec<String> = merged
        .iter()
        .take(10)
        .map(|(name, s)| {
            format!(
                "{name} — {} ({}%)",
                duration_text(*s),
                (*s * 100 + total / 2) / total
            )
        })
        .collect();
    format!(
        "За компьютером {label}: {}.\n{}",
        duration_text(total),
        lines.join("\n")
    )
}

pub struct ScreenTimeTool {
    pub store: UsageStore,
    /// `tracking.app_time` from the config.
    pub enabled: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl Tool for ScreenTimeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "screen_time",
            "Сколько времени пользователь провёл в программах: всего и по программам. Учёт идёт, только если пользователь включил его в настройках.",
            schema(
                &[(
                    "period",
                    "string",
                    "one of: today, yesterday, week, month",
                )],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let name = arg_str_opt(args, "period").unwrap_or("");
        if period(name, chrono::Local::now().date_naive()).is_none() {
            return Err(ToolError::BadArguments(format!(
                "неизвестный период «{name}»: today, yesterday, week, month"
            )));
        }
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть время в программах",
            "учёт времени",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        // Turned off, it is not used either: what was counted before stays on
        // this computer until it is wiped or grows old.
        if !self.enabled.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(ToolOutput::new(
                "Учёт времени выключен: включить можно в настройках, раздел «Инструменты».",
            ));
        }
        let today = chrono::Local::now().date_naive();
        let name = arg_str_opt(&args, "period").unwrap_or("");
        let (from, to, label) = period(name, today).unwrap_or((today, today, "сегодня"));
        let rows = self
            .store
            .report(from, to)
            .map_err(|e| failed(format!("учёт времени недоступен: {e}")))?;
        Ok(ToolOutput::new(report_text(&rows, label)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use serde_json::json;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn seconds_add_up_per_day_and_old_days_go() {
        let s = UsageStore(Arc::new(Db::open_in_memory().unwrap()));
        let mut a = HashMap::new();
        a.insert("chrome.exe".to_string(), 600);
        a.insert("telegram.exe".to_string(), 120);
        s.add(day("2026-11-13"), &a).unwrap();
        s.add(day("2026-11-13"), &a).unwrap();
        s.add(day("2026-08-01"), &a).unwrap();
        let r = s.report(day("2026-11-13"), day("2026-11-13")).unwrap();
        assert_eq!(
            r,
            [("chrome.exe".into(), 1200), ("telegram.exe".into(), 240)]
        );
        assert_eq!(s.prune(day("2026-11-13")).unwrap(), 2);
        assert_eq!(
            s.report(day("2026-01-01"), day("2026-12-31"))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn names_durations_and_the_report_read_well() {
        assert_eq!(app_label("CHROME.EXE"), "Chrome");
        assert_eq!(app_label("obsidian.exe"), "Obsidian");
        assert_eq!(duration_text(30), "меньше минуты");
        assert_eq!(duration_text(12 * 60), "12 мин");
        assert_eq!(duration_text(65 * 60), "1 ч 05 мин");
        let text = report_text(
            &[
                ("chrome.exe".into(), 3600),
                ("1cv8.exe".into(), 1800),
                ("1cv8c.exe".into(), 1800),
            ],
            "сегодня",
        );
        assert!(
            text.starts_with("За компьютером сегодня: 2 ч 00 мин."),
            "{text}"
        );
        assert!(
            text.contains("Chrome — 1 ч 00 мин (50%)\n1С — 1 ч 00 мин (50%)"),
            "{text}"
        );
        assert_eq!(report_text(&[], "вчера"), "Данных вчера нет.");
        let today = day("2026-11-13");
        assert_eq!(period("week", today).unwrap().0, day("2026-11-07"));
        assert!(period("год", today).is_none());
    }

    #[tokio::test]
    async fn the_tool_says_when_counting_is_off() {
        let tool = ScreenTimeTool {
            store: UsageStore(Arc::new(Db::open_in_memory().unwrap())),
            enabled: Default::default(),
        };
        let out = run(&tool, json!({"period": "today"})).await.unwrap().text;
        assert!(out.contains("Учёт времени выключен"), "{out}");
        assert!(tool.assess(&json!({"period": "год"})).is_err());
    }

    #[tokio::test]
    async fn counted_time_is_not_told_while_counting_is_off() {
        let store = UsageStore(Arc::new(Db::open_in_memory().unwrap()));
        let today = chrono::Local::now().date_naive();
        store
            .add(today, &HashMap::from([("telegram.exe".to_string(), 3600)]))
            .unwrap();
        let tool = ScreenTimeTool {
            store,
            enabled: Default::default(),
        };
        let out = run(&tool, json!({"period": "month"})).await.unwrap().text;
        assert!(!out.contains("Telegram"), "{out}");
        assert!(out.contains("Учёт времени выключен"), "{out}");
        tool.enabled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let out = run(&tool, json!({"period": "month"})).await.unwrap().text;
        assert!(out.contains("Telegram — 1 ч 00 мин"), "{out}");
    }
}
