//! User commands: a name and the instruction it stands for.
//!
//! A chat message equal to a command name is replaced by the instruction.
//! The assistant then calls tools as usual, each one through the risk policy.

use std::sync::Arc;

use rusqlite::params;
use serde::Serialize;

use crate::confirm::clean;
use crate::storage::{Db, DbError};

pub const MAX_COMMAND_NAME: usize = 60;
pub const MAX_COMMAND_PROMPT: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Command {
    pub id: i64,
    pub name: String,
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("у команды должно быть имя из букв или цифр, до 60 символов")]
    BadName,
    #[error("у команды должен быть текст, до 2000 символов")]
    BadPrompt,
    #[error("команды недоступны: {0}")]
    Db(String),
}

impl From<DbError> for CommandError {
    fn from(e: DbError) -> Self {
        CommandError::Db(e.to_string())
    }
}

#[derive(Clone)]
pub struct CommandStore {
    db: Arc<Db>,
}

impl CommandStore {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    /// Saves a command; the same name (case and punctuation ignored) replaces the old one.
    pub fn save(&self, name: &str, prompt: &str) -> Result<(), CommandError> {
        let name = name.trim();
        let key = clean(name);
        if key.is_empty() || name.chars().count() > MAX_COMMAND_NAME {
            return Err(CommandError::BadName);
        }
        let prompt = prompt.trim();
        if prompt.is_empty() || prompt.chars().count() > MAX_COMMAND_PROMPT {
            return Err(CommandError::BadPrompt);
        }
        self.db.with(|c| {
            c.execute(
                "INSERT INTO commands (name, key, prompt) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET name = ?1, prompt = ?3",
                params![name, key, prompt],
            )
        })?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<Command>, CommandError> {
        Ok(self.db.with(|c| {
            let mut st = c.prepare("SELECT id, name, prompt FROM commands ORDER BY name")?;
            let rows = st.query_map([], |r| {
                Ok(Command {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    prompt: r.get(2)?,
                })
            })?;
            rows.collect()
        })?)
    }

    pub fn delete(&self, name: &str) -> Result<bool, CommandError> {
        let key = clean(name);
        Ok(self
            .db
            .with(|c| c.execute("DELETE FROM commands WHERE key = ?1", [key]))?
            > 0)
    }

    /// The command whose name equals the whole message.
    pub fn find(&self, message: &str) -> Option<Command> {
        let key = clean(message);
        if key.is_empty() {
            return None;
        }
        self.db
            .with(|c| {
                let mut st = c.prepare("SELECT id, name, prompt FROM commands WHERE key = ?1")?;
                let mut rows = st.query_map([key], |r| {
                    Ok(Command {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        prompt: r.get(2)?,
                    })
                })?;
                rows.next().transpose()
            })
            .unwrap_or_else(|e| {
                log::error!("cannot read commands: {e}");
                None
            })
    }
}

/// The message the assistant gets instead of the command name.
pub fn expand(command: &Command) -> String {
    format!(
        "{}\n\nЭто моя сохранённая команда. Выполни её по шагам: {}",
        command.name, command.prompt
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> CommandStore {
        CommandStore::new(Arc::new(Db::open_in_memory().unwrap()))
    }

    #[test]
    fn command_is_found_by_its_whole_name_only() {
        let s = store();
        s.save("Рабочий режим", "Открой Блокнот и калькулятор")
            .unwrap();
        assert_eq!(
            s.find(" рабочий  режим! ").unwrap().prompt,
            "Открой Блокнот и калькулятор"
        );
        assert!(s.find("включи рабочий режим пожалуйста").is_none());
        assert!(s.find("").is_none());
    }

    #[test]
    fn saving_again_replaces_and_delete_removes() {
        let s = store();
        s.save("Отбой", "Выключи звук").unwrap();
        s.save("отбой", "Выключи звук и заблокируй компьютер")
            .unwrap();
        let all = s.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "отбой");
        assert!(s.delete("ОТБОЙ").unwrap());
        assert!(!s.delete("отбой").unwrap());
    }

    #[test]
    fn empty_name_or_prompt_is_rejected() {
        let s = store();
        assert_eq!(s.save("!!!", "x"), Err(CommandError::BadName));
        assert_eq!(s.save("имя", "  "), Err(CommandError::BadPrompt));
        assert_eq!(
            s.save(&"я".repeat(MAX_COMMAND_NAME + 1), "x"),
            Err(CommandError::BadName)
        );
    }

    #[test]
    fn expand_keeps_name_and_adds_instruction() {
        let c = Command {
            id: 1,
            name: "Отбой".into(),
            prompt: "Выключи звук".into(),
        };
        let text = expand(&c);
        assert!(text.starts_with("Отбой\n\n"));
        assert!(text.ends_with("Выключи звук"));
    }
}
