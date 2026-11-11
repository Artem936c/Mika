//! Notes in the `notes` table of `mika.db`.

use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::storage::rusqlite::params;
use mika_core::storage::{now, Db};
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::util::{arg_u64, failed, schema, truncate};

fn db_err(e: impl std::fmt::Display) -> ToolError {
    failed(format!("заметки недоступны: {e}"))
}

pub struct NoteCreateTool(pub Arc<Db>);

#[async_trait]
impl Tool for NoteCreateTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "note_create",
            "Сохранить заметку.",
            schema(
                &[
                    ("text", "string", "Текст заметки"),
                    ("title", "string", "Заголовок, необязательно"),
                ],
                &["text"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let text = arg_str(args, "text")?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Сохранить заметку",
            truncate(text, 200),
            "заметку можно удалить",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let text = arg_str(&args, "text")?.trim().to_string();
        let title = arg_str_opt(&args, "title")
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| truncate(text.lines().next().unwrap_or(""), 60));
        let id = self
            .0
            .with(|c| {
                c.execute(
                    "INSERT INTO notes (created_at, title, body) VALUES (?1, ?2, ?3)",
                    params![now(), title, text],
                )?;
                Ok(c.last_insert_rowid())
            })
            .map_err(db_err)?;
        Ok(ToolOutput::new(format!(
            "Заметка №{id} «{title}» сохранена."
        )))
    }
}

pub struct NoteSearchTool(pub Arc<Db>);

#[async_trait]
impl Tool for NoteSearchTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "note_search",
            "Найти заметки по словам. Пустой запрос — последние заметки.",
            schema(&[("query", "string", "Слова для поиска")], &[]),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Поискать в заметках",
            "заметки",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let q = arg_str_opt(&args, "query")
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let rows: Vec<(i64, String, String, String)> = self
            .0
            .with(|c| {
                let mut st =
                    c.prepare("SELECT id, created_at, title, body FROM notes ORDER BY id DESC")?;
                let rows =
                    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                rows.collect()
            })
            .map_err(db_err)?;
        // SQLite LIKE ignores case only for ASCII, so filter here for Cyrillic.
        let words: Vec<&str> = q.split_whitespace().collect();
        let found: Vec<String> = rows
            .iter()
            .filter(|(_, _, t, b)| {
                let hay = format!("{t} {b}").to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .take(10)
            .map(|(id, at, t, b)| {
                format!(
                    "№{id} от {}: {t} — {}",
                    &at[..10.min(at.len())],
                    truncate(b, 120)
                )
            })
            .collect();
        if found.is_empty() {
            return Ok(ToolOutput::new("Заметок не нашла."));
        }
        Ok(ToolOutput::new(found.join("\n")))
    }
}

pub struct NoteReadTool(pub Arc<Db>);

#[async_trait]
impl Tool for NoteReadTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "note_read",
            "Прочитать заметку целиком по номеру.",
            schema(&[("id", "integer", "Номер заметки")], &["id"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let id = arg_u64(args, "id")
            .ok_or_else(|| ToolError::BadArguments("нужен номер заметки".into()))?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Прочитать заметку",
            format!("№{id}"),
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let id = arg_u64(&args, "id").unwrap_or(0) as i64;
        let row: Option<(String, String)> = self
            .0
            .with(|c| {
                let mut st = c.prepare("SELECT title, body FROM notes WHERE id = ?1")?;
                let mut rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.next().transpose()
            })
            .map_err(db_err)?;
        let (title, body) = row.ok_or_else(|| failed(format!("нет заметки №{id}")))?;
        Ok(ToolOutput::new(format!("{title}\n\n{body}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use serde_json::json;

    #[tokio::test]
    async fn notes_create_search_read() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        run(
            &NoteCreateTool(db.clone()),
            json!({"text": "Купить Молоко и хлеб"}),
        )
        .await
        .unwrap();
        run(
            &NoteCreateTool(db.clone()),
            json!({"text": "Позвонить маме", "title": "Звонки"}),
        )
        .await
        .unwrap();
        let s = run(&NoteSearchTool(db.clone()), json!({"query": "молоко"}))
            .await
            .unwrap()
            .text;
        assert!(s.starts_with("№1 от "), "{s}");
        assert!(s.contains("Купить Молоко и хлеб"));
        assert!(!s.contains("маме"));
        let all = run(&NoteSearchTool(db.clone()), json!({}))
            .await
            .unwrap()
            .text;
        assert!(all.starts_with("№2"), "{all}");
        assert_eq!(
            run(&NoteReadTool(db.clone()), json!({"id": 2}))
                .await
                .unwrap()
                .text,
            "Звонки\n\nПозвонить маме"
        );
        assert!(run(&NoteReadTool(db), json!({"id": 9})).await.is_err());
    }
}
