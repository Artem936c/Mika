//! SQLite database `mika.db`: chat history, and later rules, audit log and notes.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{params, Connection};

use crate::llm::ChatMessage;

pub use rusqlite;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored message is broken: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Schema steps. Each runs once; `user_version` stores how many ran.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE messages (
        id INTEGER PRIMARY KEY,
        conversation TEXT NOT NULL,
        created_at TEXT NOT NULL,
        body TEXT NOT NULL
    );
    CREATE INDEX messages_conversation ON messages(conversation, id);",
    "CREATE TABLE rules (
        id INTEGER PRIMARY KEY,
        tool TEXT NOT NULL,
        pattern TEXT NOT NULL,
        decision TEXT NOT NULL,
        created_at TEXT NOT NULL,
        UNIQUE(tool, pattern)
    );
    CREATE TABLE audit (
        id INTEGER PRIMARY KEY,
        at TEXT NOT NULL,
        tool TEXT NOT NULL,
        target TEXT NOT NULL,
        risk TEXT NOT NULL,
        decision TEXT NOT NULL,
        decided_by TEXT NOT NULL
    );",
    "CREATE TABLE notes (
        id INTEGER PRIMARY KEY,
        created_at TEXT NOT NULL,
        title TEXT NOT NULL,
        body TEXT NOT NULL
    );",
];

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, DbError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Db, DbError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Db, DbError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let done: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(done.max(0) as usize) {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", (i + 1) as i64)?;
        }
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    /// Runs a closure on the connection. For modules that own their tables.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, DbError> {
        Ok(f(&self.conn.lock())?)
    }

    pub fn append_message(&self, conversation: &str, msg: &ChatMessage) -> Result<(), DbError> {
        let body = serde_json::to_string(msg)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO messages (conversation, created_at, body) VALUES (?1, ?2, ?3)",
                params![conversation, now(), body],
            )
            .map(|_| ())
        })
    }

    pub fn clear_messages(&self, conversation: &str) -> Result<(), DbError> {
        self.with(|c| {
            c.execute(
                "DELETE FROM messages WHERE conversation = ?1",
                [conversation],
            )
            .map(|_| ())
        })
    }

    pub fn load_messages(&self, conversation: &str) -> Result<Vec<ChatMessage>, DbError> {
        let bodies: Vec<String> = self.with(|c| {
            let mut st =
                c.prepare("SELECT body FROM messages WHERE conversation = ?1 ORDER BY id")?;
            let rows = st.query_map([conversation], |r| r.get(0))?;
            rows.collect()
        })?;
        bodies
            .iter()
            .map(|b| serde_json::from_str(b).map_err(DbError::from))
            .collect()
    }
}

pub fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_roundtrip_in_order() {
        let db = Db::open_in_memory().unwrap();
        db.append_message("c", &ChatMessage::user("1")).unwrap();
        db.append_message("c", &ChatMessage::assistant("2"))
            .unwrap();
        assert_eq!(
            db.load_messages("c").unwrap(),
            vec![ChatMessage::user("1"), ChatMessage::assistant("2")]
        );
    }

    #[test]
    fn conversations_are_separate() {
        let db = Db::open_in_memory().unwrap();
        db.append_message("a", &ChatMessage::user("1")).unwrap();
        assert!(db.load_messages("b").unwrap().is_empty());
    }

    #[test]
    fn reopening_file_keeps_data_and_skips_done_migrations() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("mika.db");
        Db::open(&p)
            .unwrap()
            .append_message("c", &ChatMessage::user("x"))
            .unwrap();
        assert_eq!(Db::open(&p).unwrap().load_messages("c").unwrap().len(), 1);
    }
}
