//! Reminders, timers, alarms and scheduled tasks.

use async_trait::async_trait;
use chrono::Local;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::schedule::{format_due, parse_when, ReminderKind, ReminderStore, Repeat};
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::util::{arg_u64, failed, truncate};

/// A reminder can be set at most this far ahead, minutes (one year).
const MAX_MINUTES: u64 = 366 * 24 * 60;

fn db_err(e: impl std::fmt::Display) -> ToolError {
    failed(format!("напоминания недоступны: {e}"))
}

/// Seconds as «1 ч 5 мин», «40 с».
pub fn human_span(seconds: i64) -> String {
    let s = seconds.max(0);
    let (d, h, m) = (s / 86_400, s % 86_400 / 3600, s % 3600 / 60);
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(format!("{d} д"));
    }
    if h > 0 {
        parts.push(format!("{h} ч"));
    }
    if m > 0 {
        parts.push(format!("{m} мин"));
    }
    if parts.is_empty() {
        parts.push(format!("{s} с"));
    }
    parts.join(" ")
}

struct Plan {
    due_at: i64,
    text: String,
    kind: ReminderKind,
    repeat: Repeat,
}

fn plan(args: &Value) -> Result<Plan, ToolError> {
    let text = arg_str(args, "text")?.trim().to_string();
    if text.is_empty() {
        return Err(ToolError::BadArguments("пустой текст напоминания".into()));
    }
    let now = Local::now();
    let at = arg_str_opt(args, "at")
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let due_at = match (arg_u64(args, "in_minutes"), at) {
        (Some(m), None) if (1..=MAX_MINUTES).contains(&m) => now.timestamp() + m as i64 * 60,
        (Some(_), None) => {
            return Err(ToolError::BadArguments(
                "in_minutes должно быть от 1 до 527040".into(),
            ))
        }
        (None, Some(at)) => parse_when(at, now).ok_or_else(|| {
            ToolError::BadArguments(format!(
                "не поняла время «{at}»: нужно ЧЧ:ММ или ГГГГ-ММ-ДД ЧЧ:ММ"
            ))
        })?,
        (Some(_), Some(_)) => {
            return Err(ToolError::BadArguments(
                "укажи что-то одно: in_minutes или at".into(),
            ))
        }
        (None, None) => return Err(ToolError::BadArguments("укажи in_minutes или at".into())),
    };
    if due_at <= now.timestamp() {
        return Err(ToolError::BadArguments("это время уже прошло".into()));
    }
    let repeat = match arg_str_opt(args, "repeat").map(str::trim).unwrap_or("none") {
        "every" => Repeat::Every(
            arg_u64(args, "every_minutes")
                .filter(|m| (1..=MAX_MINUTES).contains(m))
                .ok_or_else(|| {
                    ToolError::BadArguments("для repeat=every нужен every_minutes".into())
                })? as u32,
        ),
        other => Repeat::parse(other)
            .ok_or_else(|| ToolError::BadArguments(format!("неизвестный повтор {other}")))?,
    };
    let kind = match arg_str_opt(args, "kind").map(str::trim).unwrap_or("remind") {
        "" | "remind" => ReminderKind::Remind,
        "task" => ReminderKind::Task,
        other => return Err(ToolError::BadArguments(format!("неизвестный вид {other}"))),
    };
    Ok(Plan {
        due_at,
        text,
        kind,
        repeat,
    })
}

pub struct ReminderSetTool(pub ReminderStore);

#[async_trait]
impl Tool for ReminderSetTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "reminder_set",
            "Поставить напоминание, таймер или будильник. Время: in_minutes (через сколько минут) или at (ЧЧ:ММ либо ГГГГ-ММ-ДД ЧЧ:ММ). kind=task — задача по расписанию: в это время text будет выполнен как поручение помощнице.",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "О чём напомнить или что сделать" },
                    "in_minutes": { "type": "integer", "description": "Через сколько минут" },
                    "at": { "type": "string", "description": "Когда: ЧЧ:ММ или ГГГГ-ММ-ДД ЧЧ:ММ" },
                    "repeat": { "type": "string", "enum": ["none", "daily", "weekly", "every"] },
                    "every_minutes": { "type": "integer", "description": "Шаг повтора для repeat=every" },
                    "kind": { "type": "string", "enum": ["remind", "task"] }
                },
                "required": ["text"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let p = plan(args)?;
        let mut target = format!("{} — {}", format_due(p.due_at), truncate(&p.text, 160));
        if p.repeat != Repeat::None {
            target.push_str(&format!(", {}", p.repeat.describe()));
        }
        Ok(match p.kind {
            ReminderKind::Remind => Assessment::new(
                RiskLevel::Low,
                "Поставить напоминание",
                target,
                "напоминание можно отменить",
            ),
            ReminderKind::Task => Assessment::new(
                RiskLevel::Medium,
                "Запланировать задачу",
                target,
                "задача выполнится сама; её действия всё равно спросят разрешение",
            ),
        })
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let p = plan(&args)?;
        let id = self
            .0
            .add(p.due_at, &p.text, p.kind, p.repeat)
            .map_err(db_err)?;
        let left = human_span(p.due_at - Local::now().timestamp());
        let what = match p.kind {
            ReminderKind::Remind => "Напоминание",
            ReminderKind::Task => "Задача",
        };
        let repeat = match p.repeat {
            Repeat::None => String::new(),
            r => format!(", повтор: {}", r.describe()),
        };
        Ok(ToolOutput::new(format!(
            "{what} №{id} на {} (через {left}){repeat}: {}",
            format_due(p.due_at),
            p.text
        ))
        .with_data(json!({
            "kind": "reminder",
            "id": id,
            "due_at": p.due_at,
            "text": p.text,
        })))
    }
}

pub struct ReminderListTool(pub ReminderStore);

#[async_trait]
impl Tool for ReminderListTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "reminder_list",
            "Показать активные напоминания, таймеры и задачи по расписанию.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть напоминания",
            "напоминания",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let list = self.0.list().map_err(db_err)?;
        if list.is_empty() {
            return Ok(ToolOutput::new("Активных напоминаний нет."));
        }
        let now = Local::now().timestamp();
        let lines: Vec<String> = list
            .iter()
            .take(30)
            .map(|r| {
                let kind = match r.kind {
                    ReminderKind::Remind => "",
                    ReminderKind::Task => " [задача]",
                };
                let repeat = match r.repeat {
                    Repeat::None => String::new(),
                    rep => format!(", {}", rep.describe()),
                };
                format!(
                    "№{}{kind} {} (через {}){repeat}: {}",
                    r.id,
                    format_due(r.due_at),
                    human_span(r.due_at - now),
                    truncate(&r.text, 120)
                )
            })
            .collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

pub struct ReminderCancelTool(pub ReminderStore);

#[async_trait]
impl Tool for ReminderCancelTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "reminder_cancel",
            "Отменить напоминание, таймер или задачу по номеру из reminder_list.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "integer", "description": "Номер напоминания" } },
                "required": ["id"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let id = arg_u64(args, "id")
            .ok_or_else(|| ToolError::BadArguments("нужен номер напоминания".into()))?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Отменить напоминание",
            format!("№{id}"),
            "напоминание можно поставить снова",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let id = arg_u64(&args, "id").unwrap_or(0) as i64;
        if self.0.cancel(id).map_err(db_err)? {
            Ok(ToolOutput::new(format!("Напоминание №{id} отменено.")))
        } else {
            Err(failed(format!("нет активного напоминания №{id}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use mika_core::storage::Db;
    use std::sync::Arc;

    fn store() -> ReminderStore {
        ReminderStore::new(Arc::new(Db::open_in_memory().unwrap()))
    }

    #[tokio::test]
    async fn set_list_cancel() {
        let s = store();
        let out = run(
            &ReminderSetTool(s.clone()),
            json!({"text": "выключить чайник", "in_minutes": 10}),
        )
        .await
        .unwrap();
        assert!(out.text.starts_with("Напоминание №1 на "), "{}", out.text);
        assert!(out.text.contains("через 10 мин") || out.text.contains("через 9 мин"));
        assert_eq!(out.data.unwrap()["kind"], "reminder");
        let due = s.list().unwrap()[0].due_at - Local::now().timestamp();
        assert!((595..=600).contains(&due), "{due}");

        let list = run(&ReminderListTool(s.clone()), json!({})).await.unwrap();
        assert!(list.text.contains("№1") && list.text.contains("выключить чайник"));

        run(&ReminderCancelTool(s.clone()), json!({"id": 1}))
            .await
            .unwrap();
        assert!(run(&ReminderCancelTool(s.clone()), json!({"id": 1}))
            .await
            .is_err());
        assert_eq!(
            run(&ReminderListTool(s), json!({})).await.unwrap().text,
            "Активных напоминаний нет."
        );
    }

    #[tokio::test]
    async fn task_with_repeat_is_medium_risk() {
        let s = store();
        let tool = ReminderSetTool(s.clone());
        let args = json!({
            "text": "скажи курс доллара", "at": "2099-01-01 09:00",
            "repeat": "daily", "kind": "task"
        });
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert!(a.target.contains("каждый день"), "{}", a.target);
        run(&tool, args).await.unwrap();
        let r = &s.list().unwrap()[0];
        assert_eq!((r.kind, r.repeat), (ReminderKind::Task, Repeat::Daily));
        assert_eq!(format_due(r.due_at), "2099-01-01 09:00");
    }

    #[test]
    fn bad_time_arguments_are_rejected() {
        let tool = ReminderSetTool(store());
        for args in [
            json!({"text": "x"}),
            json!({"text": "x", "in_minutes": 0}),
            json!({"text": "x", "in_minutes": 5, "at": "10:00"}),
            json!({"text": "x", "at": "вечером"}),
            json!({"text": "x", "at": "2001-01-01 10:00"}),
            json!({"text": " ", "in_minutes": 5}),
            json!({"text": "x", "in_minutes": 5, "repeat": "every"}),
            json!({"text": "x", "in_minutes": 5, "kind": "boom"}),
        ] {
            assert!(
                matches!(tool.assess(&args), Err(ToolError::BadArguments(_))),
                "{args}"
            );
        }
        assert!(tool
            .assess(&json!({"text": "x", "in_minutes": 5, "repeat": "every", "every_minutes": 30}))
            .is_ok());
    }

    #[test]
    fn human_span_formats() {
        assert_eq!(human_span(40), "40 с");
        assert_eq!(human_span(600), "10 мин");
        assert_eq!(human_span(3900), "1 ч 5 мин");
        assert_eq!(human_span(90_000), "1 д 1 ч");
        assert_eq!(human_span(-5), "0 с");
    }
}
