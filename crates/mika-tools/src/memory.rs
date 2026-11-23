//! Long-term memory: facts about the user and search over past conversations.

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::{Role, ToolSpec};
use mika_core::memory::Memory;
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::util::{arg_u64, failed, schema, truncate};

fn db_err(e: impl std::fmt::Display) -> ToolError {
    failed(format!("память недоступна: {e}"))
}

pub struct MemorySaveTool(pub Memory);

#[async_trait]
impl Tool for MemorySaveTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "memory_save",
            "Запомнить факт о пользователе надолго: имя, предпочтения, близкие, привычки, важные даты. Вызывай, когда пользователь сообщил о себе что-то, что пригодится в будущих разговорах, или попросил запомнить. Один факт — одна короткая фраза.",
            schema(&[("text", "string", "Факт одной фразой")], &["text"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let text = arg_str(args, "text")?;
        if text.trim().is_empty() {
            return Err(ToolError::BadArguments("пустой факт".into()));
        }
        Ok(Assessment::new(
            RiskLevel::Low,
            "Запомнить",
            truncate(text, 200),
            "факт можно удалить на странице «Память»",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let text = arg_str(&args, "text")?.trim().to_string();
        let id = self.0.save(&text).await.map_err(db_err)?;
        Ok(ToolOutput::new(format!("Запомнила (№{id}): {text}")))
    }
}

pub struct MemorySearchTool(pub Memory);

#[async_trait]
impl Tool for MemorySearchTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "memory_search",
            "Найти в памяти факты о пользователе. Пустой запрос — последние факты.",
            schema(&[("query", "string", "О чём вспомнить")], &[]),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Вспомнить",
            "память",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let q = arg_str_opt(&args, "query").unwrap_or("");
        let found = self.0.search(q, 15).await.map_err(db_err)?;
        if found.is_empty() {
            return Ok(ToolOutput::new("В памяти об этом ничего нет."));
        }
        let lines: Vec<String> = found
            .iter()
            .map(|f| format!("№{}: {}", f.id, f.text))
            .collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

pub struct MemoryForgetTool(pub Memory);

#[async_trait]
impl Tool for MemoryForgetTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "memory_forget",
            "Забыть факт по номеру из memory_search.",
            schema(&[("id", "integer", "Номер факта")], &["id"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let id = arg_u64(args, "id")
            .ok_or_else(|| ToolError::BadArguments("нужен номер факта".into()))?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Забыть факт",
            format!("№{id}"),
            "факт можно запомнить снова",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let id = arg_u64(&args, "id").unwrap_or(0) as i64;
        if self.0.facts.remove(id).map_err(db_err)? {
            Ok(ToolOutput::new(format!("Факт №{id} забыт.")))
        } else {
            Err(failed(format!("нет факта №{id}")))
        }
    }
}

pub struct HistorySearchTool(pub Memory);

#[async_trait]
impl Tool for HistorySearchTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "history_search",
            "Найти в прошлых разговорах, что обсуждали раньше. Ищет и по завершённым разговорам.",
            schema(&[("query", "string", "Что искать")], &["query"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let q = arg_str(args, "query")?;
        if q.trim().is_empty() {
            return Err(ToolError::BadArguments("пустой запрос".into()));
        }
        Ok(Assessment::new(
            RiskLevel::Low,
            "Поискать в прошлых разговорах",
            truncate(q, 120),
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let q = arg_str(&args, "query")?;
        let hits = self.0.search_history(q, 8).await.map_err(db_err)?;
        if hits.is_empty() {
            return Ok(ToolOutput::new("В прошлых разговорах этого нет."));
        }
        let lines: Vec<String> = hits
            .iter()
            .map(|h| {
                let who = if h.role == Role::User {
                    "пользователь"
                } else {
                    "ты"
                };
                format!(
                    "{} {who}: {}",
                    &h.at[..10.min(h.at.len())],
                    truncate(&h.text, 300)
                )
            })
            .collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use mika_core::llm::ChatMessage;
    use mika_core::storage::Db;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn save_search_forget() {
        let m = Memory::new(Arc::new(Db::open_in_memory().unwrap()), None);
        let out = run(
            &MemorySaveTool(m.clone()),
            json!({"text": " Кота зовут Барсик "}),
        )
        .await
        .unwrap();
        assert_eq!(out.text, "Запомнила (№1): Кота зовут Барсик");
        assert_eq!(
            run(&MemorySearchTool(m.clone()), json!({"query": "кота"}))
                .await
                .unwrap()
                .text,
            "№1: Кота зовут Барсик"
        );
        assert_eq!(
            run(&MemorySearchTool(m.clone()), json!({}))
                .await
                .unwrap()
                .text,
            "№1: Кота зовут Барсик"
        );
        run(&MemoryForgetTool(m.clone()), json!({"id": 1}))
            .await
            .unwrap();
        assert!(run(&MemoryForgetTool(m.clone()), json!({"id": 1}))
            .await
            .is_err());
        assert_eq!(
            run(&MemorySearchTool(m), json!({"query": "кота"}))
                .await
                .unwrap()
                .text,
            "В памяти об этом ничего нет."
        );
    }

    #[tokio::test]
    async fn history_search_reads_other_conversations_and_not_deleted_ones() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.append_message("old", &ChatMessage::user("Купил велосипед Stels"))
            .unwrap();
        db.append_message("gone", &ChatMessage::user("Велосипед продан"))
            .unwrap();
        db.delete_conversation("gone").unwrap();
        let m = Memory::new(db, None);
        let out = run(&HistorySearchTool(m.clone()), json!({"query": "велосипед"}))
            .await
            .unwrap()
            .text;
        assert!(out.contains("пользователь: Купил велосипед Stels"), "{out}");
        assert!(!out.contains("продан"), "{out}");
        assert_eq!(
            run(&HistorySearchTool(m.clone()), json!({"query": "самокат"}))
                .await
                .unwrap()
                .text,
            "В прошлых разговорах этого нет."
        );
        assert!(HistorySearchTool(m).assess(&json!({"query": " "})).is_err());
    }
}
