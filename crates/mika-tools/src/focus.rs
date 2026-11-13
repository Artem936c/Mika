//! Focus: for a while she keeps quiet. Notifications wait instead of sounding,
//! breaks and greetings stay unsaid; when focus ends she lists what came.

use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::util::{arg_u64, schema};

pub const MAX_MINUTES: u64 = 240;
const DEFAULT_MINUTES: u64 = 25;
/// Notifications kept for the summary after focus.
const MAX_HELD: usize = 30;

#[derive(Debug, Default)]
struct Inner {
    /// Unix seconds when focus ends.
    until: Option<i64>,
    held: Vec<String>,
}

/// Focus state shared by the tool, the notification reader and the rhythm loop.
#[derive(Clone, Default)]
pub struct Focus(Arc<Mutex<Inner>>);

impl Focus {
    /// Starts focus, or moves the end of the running one. What waited is kept:
    /// a focus that ran out but was not summed up yet goes on with it, and it
    /// is listed when this one ends.
    pub fn start(&self, minutes: u64, at: i64) -> i64 {
        let until = at + minutes.clamp(1, MAX_MINUTES) as i64 * 60;
        self.0.lock().until = Some(until);
        until
    }

    /// Ends focus now. Returns the notifications that waited, or `None` when
    /// there was no focus.
    pub fn stop(&self) -> Option<Vec<String>> {
        let mut s = self.0.lock();
        s.until.take()?;
        Some(std::mem::take(&mut s.held))
    }

    pub fn active(&self, at: i64) -> bool {
        self.0.lock().until.is_some_and(|u| u > at)
    }

    /// Seconds left, when focus is on.
    pub fn left(&self, at: i64) -> Option<i64> {
        self.0.lock().until.filter(|u| *u > at).map(|u| u - at)
    }

    /// Focus ran out by `at`: ends it and returns what waited. `None` while it
    /// still runs or when there was none.
    pub fn expired(&self, at: i64) -> Option<Vec<String>> {
        let ended = self.0.lock().until.is_some_and(|u| u <= at);
        if ended {
            self.stop()
        } else {
            None
        }
    }

    /// Keeps a notification line for later. False when focus is off: the
    /// caller shows it as usual.
    pub fn hold(&self, line: &str, at: i64) -> bool {
        let mut s = self.0.lock();
        if !s.until.is_some_and(|u| u > at) {
            return false;
        }
        if s.held.len() < MAX_HELD {
            s.held.push(line.to_string());
        }
        true
    }
}

/// What she says when focus ends.
pub fn summary(held: &[String]) -> String {
    if held.is_empty() {
        return "Фокус закончился. За это время ничего не пришло.".into();
    }
    let mut text = format!("Фокус закончился. За это время пришло {}:", held.len());
    for line in held.iter().take(10) {
        text.push_str("\n• ");
        text.push_str(line);
    }
    if held.len() > 10 {
        text.push_str(&format!("\n…и ещё {}", held.len() - 10));
    }
    text
}

/// Notifications read out at the end of focus, at most.
const SPOKEN_LINES: usize = 3;
/// Longest notification read out, characters: as when one comes outside focus.
const SPOKEN_CHARS: usize = 220;

/// What she says aloud when focus ends. The texts of notifications sound only
/// when the user lets notifications be read aloud, a few and cut short; the
/// whole list stays in the chat.
pub fn spoken_summary(held: &[String], read_aloud: bool) -> String {
    if held.is_empty() {
        return summary(held);
    }
    let mut text = format!("Фокус закончился. Уведомлений: {}.", held.len());
    if !read_aloud {
        text.push_str(" Список в чате.");
        return text;
    }
    for line in held.iter().take(SPOKEN_LINES) {
        let line = line.trim();
        text.push(' ');
        text.extend(line.chars().take(SPOKEN_CHARS));
        if line.chars().count() > SPOKEN_CHARS {
            text.push('…');
        } else if !line.ends_with(['.', '!', '?', '…']) {
            text.push('.');
        }
    }
    if held.len() > SPOKEN_LINES {
        text.push_str(" Остальное в чате.");
    }
    text
}

pub struct FocusModeTool(pub Focus);

fn now() -> i64 {
    chrono::Local::now().timestamp()
}

#[async_trait]
impl Tool for FocusModeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "focus_mode",
            "Режим фокуса: на время ты молчишь, уведомления копишь, а после перечисляешь, что пришло. start — начать, stop — закончить, status — сколько осталось.",
            schema(
                &[
                    ("action", "string", "one of: start, stop, status"),
                    (
                        "minutes",
                        "integer",
                        "Сколько минут, по умолчанию 25, не больше 240",
                    ),
                ],
                &["action"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let summary = match arg_str(args, "action")? {
            "start" => "Включить фокус",
            "stop" => "Закончить фокус",
            "status" => "Узнать, идёт ли фокус",
            other => {
                return Err(ToolError::BadArguments(format!(
                    "неизвестное действие {other}"
                )))
            }
        };
        Ok(Assessment::new(
            RiskLevel::Low,
            summary,
            "режим фокуса",
            "меняет только то, когда Мика говорит",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let at = now();
        match arg_str(&args, "action")? {
            "start" => {
                let minutes = arg_u64(&args, "minutes")
                    .unwrap_or(DEFAULT_MINUTES)
                    .clamp(1, MAX_MINUTES);
                let until = self.0.start(minutes, at);
                let end = chrono::DateTime::from_timestamp(until, 0)
                    .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                    .unwrap_or_default();
                Ok(ToolOutput::new(format!(
                    "Фокус на {minutes} мин, до {end}. Уведомления придержу и перечислю потом."
                )))
            }
            "stop" => Ok(ToolOutput::new(match self.0.stop() {
                Some(held) => summary(&held),
                None => "Фокус и так не включён.".into(),
            })),
            _ => Ok(ToolOutput::new(match self.0.left(at) {
                Some(s) => format!("Фокус идёт, осталось {} мин.", (s + 59) / 60),
                None => "Фокус не включён.".into(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use serde_json::json;

    #[test]
    fn notifications_wait_during_focus_and_come_after() {
        let f = Focus::default();
        assert!(!f.hold("Telegram — Аня: привет", 0));
        let until = f.start(10, 1000);
        assert_eq!(until, 1600);
        assert!(f.active(1599) && !f.active(1600));
        assert!(f.hold("Telegram — Аня: привет", 1100));
        assert_eq!(f.expired(1599), None);
        let held = f.expired(1600).unwrap();
        assert_eq!(held, ["Telegram — Аня: привет"]);
        assert_eq!(f.expired(1700), None);
        assert!(summary(&held).contains("пришло 1:\n• Telegram — Аня"));
        assert!(summary(&[]).contains("ничего не пришло"));
    }

    #[test]
    fn starting_again_keeps_what_waited() {
        let f = Focus::default();
        f.start(10, 1000);
        assert!(f.hold("Telegram — Аня: привет", 1100));
        // «Продли фокус ещё на 20 минут» while it runs.
        assert_eq!(f.start(20, 1200), 2400);
        assert!(f.hold("Telegram — Боря: ты где?", 1300));
        // The start button pressed after the end, before the loop summed it up.
        assert_eq!(f.expired(2300), None);
        f.start(5, 2500);
        let held = f.expired(2800).unwrap();
        assert_eq!(held, ["Telegram — Аня: привет", "Telegram — Боря: ты где?"]);
        // A new focus after a summed-up one starts empty.
        f.start(5, 3000);
        assert_eq!(f.expired(3300).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn the_spoken_summary_keeps_texts_quiet_unless_allowed() {
        let long = format!("Telegram — Аня: {}", "очень ".repeat(60));
        let held = vec![
            "Telegram — Аня: мой пароль от почты qwerty".to_string(),
            long.clone(),
            "Почта — Банк: код 1234!".to_string(),
            "Telegram — Боря: привет".to_string(),
        ];
        let quiet = spoken_summary(&held, false);
        assert_eq!(quiet, "Фокус закончился. Уведомлений: 4. Список в чате.");
        let aloud = spoken_summary(&held, true);
        assert!(
            aloud.starts_with(
                "Фокус закончился. Уведомлений: 4. Telegram — Аня: мой пароль от почты qwerty. "
            ),
            "{aloud}"
        );
        assert!(aloud.contains("код 1234! "), "{aloud}");
        assert!(!aloud.contains("Боря"), "{aloud}");
        assert!(aloud.ends_with("Остальное в чате."), "{aloud}");
        assert!(aloud.contains('…') && !aloud.contains(&long), "{aloud}");
        assert!(aloud.chars().count() < 3 * SPOKEN_CHARS + 100);
        assert_eq!(spoken_summary(&[], false), summary(&[]));
    }

    #[tokio::test]
    async fn the_tool_starts_reports_and_stops() {
        let f = Focus::default();
        let tool = FocusModeTool(f.clone());
        let out = run(&tool, json!({"action": "start", "minutes": 5000}))
            .await
            .unwrap()
            .text;
        assert!(out.contains("на 240 мин"), "{out}");
        let s = run(&tool, json!({"action": "status"})).await.unwrap().text;
        assert!(s.contains("осталось 240 мин"), "{s}");
        let s = run(&tool, json!({"action": "stop"})).await.unwrap().text;
        assert!(s.contains("Фокус закончился"), "{s}");
        assert!(!f.active(now()));
        assert!(tool.assess(&json!({"action": "x"})).is_err());
    }
}
