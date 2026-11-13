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
    pub fn start(&self, minutes: u64, at: i64) -> i64 {
        let until = at + minutes.clamp(1, MAX_MINUTES) as i64 * 60;
        let mut s = self.0.lock();
        s.until = Some(until);
        s.held.clear();
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
