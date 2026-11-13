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
    "CREATE TABLE reminders (
        id INTEGER PRIMARY KEY,
        created_at TEXT NOT NULL,
        due_at INTEGER NOT NULL,
        text TEXT NOT NULL,
        kind TEXT NOT NULL,
        repeat TEXT NOT NULL,
        done INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX reminders_due ON reminders(done, due_at);
    CREATE TABLE todos (
        id INTEGER PRIMARY KEY,
        created_at TEXT NOT NULL,
        text TEXT NOT NULL,
        due TEXT,
        done INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE facts (
        id INTEGER PRIMARY KEY,
        created_at TEXT NOT NULL,
        text TEXT NOT NULL
    );
    CREATE TABLE commands (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        key TEXT NOT NULL UNIQUE,
        prompt TEXT NOT NULL
    );",
    "ALTER TABLE facts ADD COLUMN embedding BLOB;
    ALTER TABLE facts ADD COLUMN embedding_model TEXT;
    CREATE TABLE message_vectors (
        message_id INTEGER PRIMARY KEY,
        model TEXT NOT NULL,
        embedding BLOB NOT NULL
    );",
    "CREATE TABLE app_usage (
        day TEXT NOT NULL,
        app TEXT NOT NULL,
        seconds INTEGER NOT NULL,
        PRIMARY KEY (day, app)
    );
    CREATE TABLE doc_files (
        path TEXT PRIMARY KEY,
        modified INTEGER NOT NULL,
        size INTEGER NOT NULL
    );
    CREATE TABLE doc_chunks (
        id INTEGER PRIMARY KEY,
        path TEXT NOT NULL,
        page INTEGER NOT NULL,
        text TEXT NOT NULL,
        model TEXT,
        embedding BLOB
    );
    CREATE INDEX doc_chunks_path ON doc_chunks(path);",
];

/// Conversations put aside by "new conversation" get this prefix.
pub const ARCHIVE_PREFIX: &str = "archive-";

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

    /// Puts the conversation aside under an archive name. Returns how many messages moved.
    pub fn archive_messages(&self, conversation: &str) -> Result<usize, DbError> {
        let name = format!(
            "{ARCHIVE_PREFIX}{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        self.with(|c| {
            c.execute(
                "UPDATE messages SET conversation = ?2 WHERE conversation = ?1",
                params![conversation, name],
            )
        })
    }

    /// Deletes every archived conversation. Returns how many messages went.
    pub fn clear_archive(&self) -> Result<usize, DbError> {
        self.with(|c| {
            c.execute(
                "DELETE FROM messages WHERE conversation LIKE ?1",
                [format!("{ARCHIVE_PREFIX}%")],
            )
        })
    }

    pub fn archive_size(&self) -> Result<usize, DbError> {
        self.with(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM messages WHERE conversation LIKE ?1",
                [format!("{ARCHIVE_PREFIX}%")],
                |r| r.get::<_, i64>(0),
            )
        })
        .map(|n| n as usize)
    }

    /// The newest user and assistant messages of every conversation: (id, date, role, text).
    pub fn recent_text_messages(
        &self,
        limit: usize,
    ) -> Result<Vec<(i64, String, crate::llm::Role, String)>, DbError> {
        use crate::llm::Role;
        let rows: Vec<(i64, String, String)> = self.with(|c| {
            let mut st =
                c.prepare("SELECT id, created_at, body FROM messages ORDER BY id DESC LIMIT ?1")?;
            let rows = st.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect()
        })?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, at, body)| {
                let m = serde_json::from_str::<ChatMessage>(&body).ok()?;
                (matches!(m.role, Role::User | Role::Assistant) && !m.content.trim().is_empty())
                    .then_some((id, at, m.role, m.content))
            })
            .collect())
    }

    /// User and assistant messages of every conversation that contain all `words`,
    /// newest first: (date, role, text). Case is ignored, Cyrillic included.
    pub fn search_messages(
        &self,
        words: &[String],
        limit: usize,
    ) -> Result<Vec<(String, crate::llm::Role, String)>, DbError> {
        use crate::llm::Role;
        let rows: Vec<(String, String)> = self.with(|c| {
            let mut st = c.prepare("SELECT created_at, body FROM messages ORDER BY id DESC")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })?;
        let mut out = Vec::new();
        for (at, body) in rows {
            let Ok(m) = serde_json::from_str::<ChatMessage>(&body) else {
                continue;
            };
            if !matches!(m.role, Role::User | Role::Assistant) || m.content.trim().is_empty() {
                continue;
            }
            let hay = m.content.to_lowercase();
            if words.iter().all(|w| hay.contains(&w.to_lowercase())) {
                out.push((at, m.role, m.content));
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
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
    fn archived_conversation_leaves_main_but_stays_searchable() {
        let db = Db::open_in_memory().unwrap();
        db.append_message("main", &ChatMessage::user("Мой кот Барсик"))
            .unwrap();
        db.append_message("main", &ChatMessage::assistant("Запомнила"))
            .unwrap();
        assert_eq!(db.archive_messages("main").unwrap(), 2);
        assert!(db.load_messages("main").unwrap().is_empty());
        assert_eq!(db.archive_size().unwrap(), 2);
        let found = db.search_messages(&["барсик".into()], 5).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].2, "Мой кот Барсик");
        assert_eq!(db.clear_archive().unwrap(), 2);
        assert!(db
            .search_messages(&["барсик".into()], 5)
            .unwrap()
            .is_empty());
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
