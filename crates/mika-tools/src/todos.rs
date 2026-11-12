//! To-do list with optional dates: the calendar of the assistant.

use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::schedule::parse_date;
use mika_core::storage::rusqlite::params;
use mika_core::storage::{now, Db};
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::util::{arg_bool, arg_u64, failed, schema, truncate};

fn db_err(e: impl std::fmt::Display) -> ToolError {
    failed(format!("список дел недоступен: {e}"))
}

/// `YYYY-MM-DD` from the argument, or nothing when it is absent.
fn date_arg(args: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match arg_str_opt(args, key)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => Ok(None),
        Some(raw) => parse_date(raw)
            .map(|d| Some(d.format("%Y-%m-%d").to_string()))
            .ok_or_else(|| {
                ToolError::BadArguments(format!("не поняла дату «{raw}»: нужно ГГГГ-ММ-ДД"))
            }),
    }
}

fn id_arg(args: &Value) -> Result<i64, ToolError> {
    arg_u64(args, "id")
        .map(|v| v as i64)
        .ok_or_else(|| ToolError::BadArguments("нужен номер дела".into()))
}

pub struct TodoAddTool(pub Arc<Db>);

#[async_trait]
impl Tool for TodoAddTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "todo_add",
            "Добавить дело в список дел. С датой оно попадает в календарь.",
            schema(
                &[
                    ("text", "string", "Что сделать"),
                    ("date", "string", "Дата ГГГГ-ММ-ДД, необязательно"),
                ],
                &["text"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let text = arg_str(args, "text")?;
        if text.trim().is_empty() {
            return Err(ToolError::BadArguments("пустое дело".into()));
        }
        date_arg(args, "date")?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Добавить дело",
            truncate(text, 200),
            "дело можно удалить",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let text = arg_str(&args, "text")?.trim().to_string();
        let date = date_arg(&args, "date")?;
        let id = self
            .0
            .with(|c| {
                c.execute(
                    "INSERT INTO todos (created_at, text, due) VALUES (?1, ?2, ?3)",
                    params![now(), text, date],
                )?;
                Ok(c.last_insert_rowid())
            })
            .map_err(db_err)?;
        let on = date.map(|d| format!(" на {d}")).unwrap_or_default();
        Ok(ToolOutput::new(format!("Дело №{id}{on}: {text}")))
    }
}

pub struct TodoListTool(pub Arc<Db>);

#[async_trait]
impl Tool for TodoListTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "todo_list",
            "Показать список дел. С date — дела на этот день (календарь).",
            schema(
                &[
                    ("date", "string", "Только дела на дату ГГГГ-ММ-ДД"),
                    ("done", "boolean", "Показать и выполненные"),
                ],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        date_arg(args, "date")?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть список дел",
            "список дел",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let date = date_arg(&args, "date")?;
        let with_done = arg_bool(&args, "done");
        let rows: Vec<(i64, String, Option<String>, bool)> = self
            .0
            .with(|c| {
                // Dated first by date, then the undated in the order they were added.
                let mut st = c.prepare(
                    "SELECT id, text, due, done FROM todos ORDER BY due IS NULL, due, id",
                )?;
                let rows = st.query_map([], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0))
                })?;
                rows.collect()
            })
            .map_err(db_err)?;
        let lines: Vec<String> = rows
            .into_iter()
            .filter(|(_, _, due, done)| (with_done || !done) && (date.is_none() || *due == date))
            .take(50)
            .map(|(id, text, due, done)| {
                let mark = if done { "[x]" } else { "[ ]" };
                let due = due.map(|d| format!(" ({d})")).unwrap_or_default();
                format!("{mark} №{id}{due}: {}", truncate(&text, 160))
            })
            .collect();
        if lines.is_empty() {
            return Ok(ToolOutput::new(match date {
                Some(d) => format!("На {d} дел нет."),
                None => "Список дел пуст.".to_string(),
            }));
        }
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

pub struct TodoDoneTool(pub Arc<Db>);

#[async_trait]
impl Tool for TodoDoneTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "todo_done",
            "Отметить дело выполненным по номеру из todo_list.",
            schema(&[("id", "integer", "Номер дела")], &["id"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Отметить дело выполненным",
            format!("№{}", id_arg(args)?),
            "отметку легко вернуть",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let id = id_arg(&args)?;
        let n = self
            .0
            .with(|c| c.execute("UPDATE todos SET done = 1 WHERE id = ?1", [id]))
            .map_err(db_err)?;
        if n == 0 {
            return Err(failed(format!("нет дела №{id}")));
        }
        Ok(ToolOutput::new(format!("Дело №{id} выполнено.")))
    }
}

pub struct TodoRemoveTool(pub Arc<Db>);

#[async_trait]
impl Tool for TodoRemoveTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "todo_remove",
            "Удалить дело из списка по номеру.",
            schema(&[("id", "integer", "Номер дела")], &["id"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Удалить дело",
            format!("№{}", id_arg(args)?),
            "дело можно добавить снова",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let id = id_arg(&args)?;
        let n = self
            .0
            .with(|c| c.execute("DELETE FROM todos WHERE id = ?1", [id]))
            .map_err(db_err)?;
        if n == 0 {
            return Err(failed(format!("нет дела №{id}")));
        }
        Ok(ToolOutput::new(format!("Дело №{id} удалено.")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use serde_json::json;

    #[tokio::test]
    async fn add_list_done_remove() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        run(&TodoAddTool(db.clone()), json!({"text": "купить хлеб"}))
            .await
            .unwrap();
        let out = run(
            &TodoAddTool(db.clone()),
            json!({"text": "к врачу", "date": "12.11.2026"}),
        )
        .await
        .unwrap();
        assert_eq!(out.text, "Дело №2 на 2026-11-12: к врачу");

        let all = run(&TodoListTool(db.clone()), json!({}))
            .await
            .unwrap()
            .text;
        assert_eq!(all, "[ ] №2 (2026-11-12): к врачу\n[ ] №1: купить хлеб");
        let day = run(&TodoListTool(db.clone()), json!({"date": "2026-11-12"}))
            .await
            .unwrap()
            .text;
        assert_eq!(day, "[ ] №2 (2026-11-12): к врачу");
        assert_eq!(
            run(&TodoListTool(db.clone()), json!({"date": "2026-11-13"}))
                .await
                .unwrap()
                .text,
            "На 2026-11-13 дел нет."
        );

        run(&TodoDoneTool(db.clone()), json!({"id": 1}))
            .await
            .unwrap();
        let open = run(&TodoListTool(db.clone()), json!({}))
            .await
            .unwrap()
            .text;
        assert!(!open.contains("хлеб"));
        let with_done = run(&TodoListTool(db.clone()), json!({"done": true}))
            .await
            .unwrap()
            .text;
        assert!(with_done.contains("[x] №1: купить хлеб"));

        run(&TodoRemoveTool(db.clone()), json!({"id": 2}))
            .await
            .unwrap();
        assert!(run(&TodoRemoveTool(db.clone()), json!({"id": 2}))
            .await
            .is_err());
        assert!(run(&TodoDoneTool(db), json!({"id": 9})).await.is_err());
    }

    #[test]
    fn bad_date_is_rejected() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        assert!(matches!(
            TodoAddTool(db.clone()).assess(&json!({"text": "x", "date": "завтра"})),
            Err(ToolError::BadArguments(_))
        ));
        assert!(matches!(
            TodoListTool(db).assess(&json!({"date": "32.13.2026"})),
            Err(ToolError::BadArguments(_))
        ));
    }
}
