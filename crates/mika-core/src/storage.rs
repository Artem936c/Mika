//! SQLite database `mika.db`: chat history, and later rules, audit log and notes.

use std::path::{Path, PathBuf};

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
    "CREATE TABLE cost_usage (
        day TEXT NOT NULL,
        kind TEXT NOT NULL,
        provider TEXT NOT NULL,
        model TEXT NOT NULL,
        priced INTEGER NOT NULL,
        currency TEXT NOT NULL,
        requests INTEGER NOT NULL DEFAULT 0,
        blind INTEGER NOT NULL DEFAULT 0,
        tokens_in INTEGER NOT NULL DEFAULT 0,
        tokens_cached INTEGER NOT NULL DEFAULT 0,
        tokens_out INTEGER NOT NULL DEFAULT 0,
        chars INTEGER NOT NULL DEFAULT 0,
        seconds REAL NOT NULL DEFAULT 0,
        images INTEGER NOT NULL DEFAULT 0,
        cost REAL NOT NULL DEFAULT 0,
        PRIMARY KEY (day, kind, provider, model, priced, currency)
    );
    CREATE TABLE cost_prices (
        provider TEXT NOT NULL,
        model TEXT NOT NULL,
        prices TEXT NOT NULL,
        fetched_at INTEGER NOT NULL,
        PRIMARY KEY (provider, model)
    );
    CREATE TABLE cost_alerts (
        period TEXT PRIMARY KEY,
        level INTEGER NOT NULL
    );
    CREATE TABLE day_log (
        id INTEGER PRIMARY KEY,
        at INTEGER NOT NULL,
        day TEXT NOT NULL,
        kind TEXT NOT NULL,
        text TEXT NOT NULL,
        amount INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX day_log_day ON day_log(day, id);",
    // Skills (decision 0047): `params` and `steps` are JSON.
    "CREATE TABLE skills (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        key TEXT NOT NULL UNIQUE,
        description TEXT NOT NULL,
        params TEXT NOT NULL,
        steps TEXT NOT NULL
    );",
    // The list of conversations (decision 0052). `main` and every `archive-*`
    // of the earlier versions become rows here; their titles are filled from
    // the first message of the user when the database opens. `opened_at`
    // tells which one was open last: it opens again at the next start.
    "CREATE TABLE conversations (
        id TEXT PRIMARY KEY,
        title TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        pinned INTEGER NOT NULL DEFAULT 0,
        opened_at TEXT
    );
    INSERT INTO conversations (id, created_at, updated_at, opened_at)
        SELECT conversation, MIN(created_at), MAX(created_at),
               CASE WHEN conversation = 'main' THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now') END
        FROM messages GROUP BY conversation;",
];

/// Longest title of a conversation, characters (decision 0052).
pub const MAX_TITLE: usize = 40;

/// One conversation of the list (decision 0052).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Conversation {
    pub id: String,
    /// Empty until the user writes the first message.
    pub title: String,
    pub created_at: String,
    /// The time of the last message, or of the making for an empty one.
    pub updated_at: String,
    pub pinned: bool,
    pub messages: usize,
}

/// A title from a text: one line, at most `MAX_TITLE` characters, an ellipsis
/// where it was cut.
pub fn title_of(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= MAX_TITLE {
        return line;
    }
    let cut: String = line.chars().take(MAX_TITLE - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The date of a stored time; `None` for a text that is not one.
fn parse_time(at: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(at).ok()
}

/// Gives every conversation of `messages` a row and every row without a
/// title the start of its first message of the user. Messages restored from
/// a backup of an earlier version come without rows.
fn sync_conversations(c: &Connection) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR IGNORE INTO conversations (id, created_at, updated_at)
         SELECT conversation, MIN(created_at), MAX(created_at) FROM messages GROUP BY conversation",
        [],
    )?;
    let firsts: Vec<(String, String)> = {
        let mut st = c.prepare(
            "SELECT c.id, (SELECT json_extract(m.body, '$.content') FROM messages m
                           WHERE m.conversation = c.id
                             AND (CASE WHEN json_valid(m.body) THEN json_extract(m.body, '$.role') END) = 'user'
                           ORDER BY m.id LIMIT 1)
             FROM conversations c WHERE c.title = ''",
        )?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        rows.filter_map(|r| match r {
            Ok((id, Some(text))) => Some(Ok((id, text))),
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<rusqlite::Result<_>>()?
    };
    for (id, text) in firsts {
        let title = title_of(&text);
        if !title.is_empty() {
            c.execute(
                "UPDATE conversations SET title = ?2 WHERE id = ?1 AND title = ''",
                params![id, title],
            )?;
        }
    }
    Ok(())
}

/// Deletes a conversation with its messages and their vectors. Returns how
/// many messages went.
fn delete_one(c: &Connection, id: &str) -> rusqlite::Result<usize> {
    c.execute(
        "DELETE FROM message_vectors WHERE message_id IN
             (SELECT id FROM messages WHERE conversation = ?1)",
        [id],
    )?;
    let n = c.execute("DELETE FROM messages WHERE conversation = ?1", [id])?;
    c.execute("DELETE FROM conversations WHERE id = ?1", [id])?;
    Ok(n)
}

pub struct Db {
    conn: Mutex<Connection>,
    /// Copies of the attached files, a folder per conversation (decision
    /// 0053): `attachments` next to the database file. None in memory.
    attachments: Option<PathBuf>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Db, DbError> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut db = Self::init(Connection::open(path)?)?;
        db.attachments = Some(
            path.parent()
                .unwrap_or(Path::new(""))
                .join(crate::attach::FOLDER),
        );
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Db, DbError> {
        Self::init(Connection::open_in_memory()?)
    }

    /// The folder of attached files, next to the database file; `None` for
    /// a database in memory.
    pub fn attachments_dir(&self) -> Option<&Path> {
        self.attachments.as_deref()
    }

    /// Removes the attached files of deleted conversations.
    fn remove_attachments(&self, ids: &[String]) {
        if let Some(root) = &self.attachments {
            for id in ids.iter().filter(|id| *id != crate::attach::WAITING) {
                crate::attach::remove_conversation(root, id);
            }
        }
    }

    /// Removes folders of attached files whose conversation is gone, such as
    /// after a restore of the history from a copy, which replaces the list.
    /// A folder changed within `fresh` stays: files attached to a message
    /// not yet sent may wait there. Returns how many folders went.
    pub fn prune_attachments(&self, fresh: std::time::Duration) -> Result<usize, DbError> {
        let Some(root) = &self.attachments else {
            return Ok(0);
        };
        let Ok(entries) = std::fs::read_dir(root) else {
            return Ok(0);
        };
        let mut kept: std::collections::HashSet<String> = self
            .list_conversations()?
            .iter()
            .map(|c| crate::attach::folder_name(&c.id))
            .collect();
        // Files under the field wait there, however long.
        kept.insert(crate::attach::folder_name(crate::attach::WAITING));
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(meta) = entry.metadata() else { continue };
            let recent = meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_none_or(|age| age < fresh);
            if !meta.is_dir() || kept.contains(&name) || recent {
                continue;
            }
            if std::fs::remove_dir_all(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn init(conn: Connection) -> Result<Db, DbError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let done: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(done.max(0) as usize) {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", (i + 1) as i64)?;
        }
        sync_conversations(&conn)?;
        Ok(Db {
            conn: Mutex::new(conn),
            attachments: None,
        })
    }

    /// Runs a closure on the connection. For modules that own their tables.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, DbError> {
        Ok(f(&self.conn.lock())?)
    }

    /// Like `with`, for callers with their own error type. The lock is not
    /// re-entrant: never call `with` or `with_conn` inside the closure.
    pub fn with_conn<T, E>(&self, f: impl FnOnce(&Connection) -> Result<T, E>) -> Result<T, E> {
        f(&self.conn.lock())
    }

    /// Stores a message of a conversation. The conversation gets a row when it
    /// has none, a newer date, and its title from the first message of the user.
    /// Pictures are never stored in the body: an attached one is a file
    /// (decision 0053), and the copy for the provider is made anew each time.
    pub fn append_message(&self, conversation: &str, msg: &ChatMessage) -> Result<(), DbError> {
        let body = if msg.images.is_empty() {
            serde_json::to_string(msg)?
        } else {
            let mut kept = msg.clone();
            kept.images.clear();
            serde_json::to_string(&kept)?
        };
        let at = now();
        let title = if msg.role == crate::llm::Role::User {
            title_of(&msg.content)
        } else {
            String::new()
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO messages (conversation, created_at, body) VALUES (?1, ?2, ?3)",
                params![conversation, at, body],
            )?;
            c.execute(
                "INSERT INTO conversations (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                     updated_at = excluded.updated_at,
                     title = CASE WHEN title = '' THEN excluded.title ELSE title END",
                params![conversation, title, at],
            )
            .map(|_| ())
        })
    }

    pub fn clear_messages(&self, conversation: &str) -> Result<(), DbError> {
        self.with(|c| {
            c.execute(
                "DELETE FROM message_vectors WHERE message_id IN
                     (SELECT id FROM messages WHERE conversation = ?1)",
                [conversation],
            )?;
            c.execute(
                "DELETE FROM messages WHERE conversation = ?1",
                [conversation],
            )
            .map(|_| ())
        })
    }

    /// Every conversation: the pinned ones first, then the newest first.
    pub fn list_conversations(&self) -> Result<Vec<Conversation>, DbError> {
        let mut list: Vec<Conversation> = self.with(|c| {
            sync_conversations(c)?;
            let mut st = c.prepare(
                "SELECT c.id, c.title, c.created_at, c.updated_at, c.pinned,
                        (SELECT COUNT(*) FROM messages m WHERE m.conversation = c.id)
                 FROM conversations c",
            )?;
            let rows = st.query_map([], |r| {
                Ok(Conversation {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    created_at: r.get(2)?,
                    updated_at: r.get(3)?,
                    pinned: r.get::<_, i64>(4)? != 0,
                    messages: r.get::<_, i64>(5)?.max(0) as usize,
                })
            })?;
            rows.collect()
        })?;
        list.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| parse_time(&b.updated_at).cmp(&parse_time(&a.updated_at)))
                .then_with(|| b.updated_at.cmp(&a.updated_at))
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(list)
    }

    /// One conversation of the list, `None` when there is no such.
    pub fn conversation(&self, id: &str) -> Result<Option<Conversation>, DbError> {
        Ok(self.list_conversations()?.into_iter().find(|c| c.id == id))
    }

    /// Makes an empty conversation and returns its id. It takes the place of
    /// the empty ones left behind without a title or a pin: «+» pressed in an
    /// empty chat and then elsewhere leaves one empty row, not many.
    pub fn create_conversation(&self) -> Result<String, DbError> {
        let at = now();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%3f").to_string();
        self.with(|c| {
            c.execute(
                "DELETE FROM conversations WHERE title = '' AND pinned = 0
                 AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.conversation = conversations.id)",
                [],
            )?;
            // A number of this run as well: an id of a row deleted above is
            // never given again, even within the same millisecond.
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            loop {
                let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let id = format!("c-{stamp}-{n}");
                let made = c.execute(
                    "INSERT OR IGNORE INTO conversations (id, created_at, updated_at) VALUES (?1, ?2, ?2)",
                    params![id, at],
                )?;
                if made == 1 {
                    return Ok(id);
                }
            }
        })
    }

    /// A new title; an empty one gives back the title from the first message.
    /// Returns false when there is no such conversation.
    pub fn rename_conversation(&self, id: &str, title: &str) -> Result<bool, DbError> {
        let title = title_of(title);
        self.with(|c| {
            let changed = c.execute(
                "UPDATE conversations SET title = ?2 WHERE id = ?1",
                params![id, title],
            )?;
            if title.is_empty() {
                sync_conversations(c)?;
            }
            Ok(changed > 0)
        })
    }

    /// Returns false when there is no such conversation.
    pub fn pin_conversation(&self, id: &str, pinned: bool) -> Result<bool, DbError> {
        self.with(|c| {
            c.execute(
                "UPDATE conversations SET pinned = ?2 WHERE id = ?1",
                params![id, pinned as i64],
            )
            .map(|n| n > 0)
        })
    }

    /// Deletes a conversation with its messages and their vectors: neither the
    /// history search nor the search by meaning finds them afterwards.
    /// Returns how many messages went.
    /// Its attached files go too (decision 0053).
    pub fn delete_conversation(&self, id: &str) -> Result<usize, DbError> {
        let n = self.with(|c| {
            let tx = c.unchecked_transaction()?;
            let n = delete_one(&tx, id)?;
            tx.commit()?;
            Ok(n)
        })?;
        self.remove_attachments(&[id.to_string()]);
        Ok(n)
    }

    /// Deletes the conversations that are not pinned and had no message and
    /// were not opened since `before`, except `keep` (the one open now).
    /// Each is checked again inside the deleting transaction: one opened or
    /// written to after the list was read stays. Returns how many went.
    pub fn purge_before(
        &self,
        before: chrono::DateTime<chrono::FixedOffset>,
        keep: &str,
    ) -> Result<usize, DbError> {
        let old: Vec<String> = self
            .list_conversations()?
            .into_iter()
            .filter(|c| !c.pinned && c.id != keep)
            .filter(|c| parse_time(&c.updated_at).is_some_and(|at| at < before))
            .map(|c| c.id)
            .collect();
        if old.is_empty() {
            return Ok(0);
        }
        let gone = self.with(|c| {
            let tx = c.unchecked_transaction()?;
            let mut gone = Vec::new();
            for id in &old {
                let row: Option<(i64, String, Option<String>)> = tx
                    .query_row(
                        "SELECT pinned, updated_at, opened_at FROM conversations WHERE id = ?1",
                        [id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map(Some)
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        e => Err(e),
                    })?;
                let Some((pinned, updated, opened)) = row else {
                    continue;
                };
                let lately =
                    |at: Option<&str>| at.and_then(parse_time).is_some_and(|at| at >= before);
                if pinned != 0 || lately(Some(&updated)) || lately(opened.as_deref()) {
                    continue;
                }
                delete_one(&tx, id)?;
                gone.push(id.clone());
            }
            tx.commit()?;
            Ok(gone)
        })?;
        self.remove_attachments(&gone);
        Ok(gone.len())
    }

    /// `purge_before` for conversations older than `days` days from now.
    pub fn purge_older_than(&self, days: u32, keep: &str) -> Result<usize, DbError> {
        let before = chrono::Local::now().fixed_offset() - chrono::Duration::days(days as i64);
        self.purge_before(before, keep)
    }

    /// Deletes every conversation that is not pinned, except `keep`. Returns how many went.
    pub fn delete_unpinned(&self, keep: &str) -> Result<usize, DbError> {
        let ids: Vec<String> = self
            .list_conversations()?
            .into_iter()
            .filter(|c| !c.pinned && c.id != keep)
            .map(|c| c.id)
            .collect();
        self.delete_many(&ids)
    }

    fn delete_many(&self, ids: &[String]) -> Result<usize, DbError> {
        if ids.is_empty() {
            return Ok(0);
        }
        self.with(|c| {
            let tx = c.unchecked_transaction()?;
            for id in ids {
                delete_one(&tx, id)?;
            }
            tx.commit()?;
            Ok(())
        })?;
        self.remove_attachments(ids);
        Ok(ids.len())
    }

    /// Notes that the conversation is the open one: it opens at the next start.
    pub fn mark_opened(&self, id: &str) -> Result<(), DbError> {
        self.with(|c| {
            c.execute(
                "UPDATE conversations SET opened_at = ?2 WHERE id = ?1",
                // UTC with milliseconds: the text sorts as the time does.
                params![
                    id,
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                ],
            )
            .map(|_| ())
        })
    }

    /// The conversation open last, or the newest one; `None` when there is none.
    pub fn last_opened(&self) -> Result<Option<String>, DbError> {
        let opened: Option<String> = self.with(|c| {
            c.query_row(
                "SELECT id FROM conversations WHERE opened_at IS NOT NULL
                 ORDER BY opened_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })
        })?;
        if opened.is_some() {
            return Ok(opened);
        }
        Ok(self
            .list_conversations()?
            .into_iter()
            .max_by_key(|c| parse_time(&c.updated_at))
            .map(|c| c.id))
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

    /// Sets the date of the last message of a conversation, for the cleanup.
    fn age(db: &Db, id: &str, updated_at: &str) {
        db.with(|c| {
            c.execute(
                "UPDATE conversations SET updated_at = ?2 WHERE id = ?1",
                params![id, updated_at],
            )
        })
        .unwrap();
    }

    #[test]
    fn main_and_archives_of_an_old_database_become_conversations_without_loss() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("mika.db");
        let before = MIGRATIONS.len() - 1;
        {
            // A database of 0.5: every step but the list of conversations.
            let c = Connection::open(&p).unwrap();
            for sql in &MIGRATIONS[..before] {
                c.execute_batch(sql).unwrap();
            }
            c.pragma_update(None, "user_version", before as i64)
                .unwrap();
            let rows = [
                (
                    "archive-20261101-090000",
                    "2026-11-01T09:00:00+03:00",
                    ChatMessage::assistant("Напоминание: чай"),
                ),
                (
                    "archive-20261101-090000",
                    "2026-11-01T09:01:00+03:00",
                    ChatMessage::user("  Как   зовут\nмоего кота?  "),
                ),
                (
                    "archive-20261101-090000",
                    "2026-11-01T09:02:00+03:00",
                    ChatMessage::assistant("Барсик"),
                ),
                (
                    "archive-20261105-100000",
                    "2026-11-05T10:00:00+03:00",
                    ChatMessage::assistant("Только заметка"),
                ),
                (
                    "main",
                    "2026-11-20T12:00:00+03:00",
                    ChatMessage::user("очень длинный вопрос ".repeat(5)),
                ),
                (
                    "main",
                    "2026-11-21T12:00:00+03:00",
                    ChatMessage::assistant("ответ"),
                ),
            ];
            for (conv, at, m) in rows {
                c.execute(
                    "INSERT INTO messages (conversation, created_at, body) VALUES (?1, ?2, ?3)",
                    params![conv, at, serde_json::to_string(&m).unwrap()],
                )
                .unwrap();
            }
        }
        let db = Db::open(&p).unwrap();
        let list = db.list_conversations().unwrap();
        let view: Vec<(&str, &str, &str, usize)> = list
            .iter()
            .map(|c| {
                (
                    c.id.as_str(),
                    c.title.as_str(),
                    c.updated_at.as_str(),
                    c.messages,
                )
            })
            .collect();
        let long = title_of(&"очень длинный вопрос ".repeat(5));
        assert_eq!(long.chars().count(), MAX_TITLE);
        assert!(long.ends_with('…'));
        assert_eq!(
            view,
            [
                ("main", long.as_str(), "2026-11-21T12:00:00+03:00", 2),
                (
                    "archive-20261105-100000",
                    "",
                    "2026-11-05T10:00:00+03:00",
                    1
                ),
                (
                    "archive-20261101-090000",
                    "Как зовут моего кота?",
                    "2026-11-01T09:02:00+03:00",
                    3
                ),
            ]
        );
        assert_eq!(list[2].created_at, "2026-11-01T09:00:00+03:00");
        assert!(list.iter().all(|c| !c.pinned));
        // Every message is still there, under its conversation.
        assert_eq!(
            db.load_messages("archive-20261101-090000").unwrap().len(),
            3
        );
        assert_eq!(db.load_messages("main").unwrap().len(), 2);
        // The one open before the update opens again.
        assert_eq!(db.last_opened().unwrap().as_deref(), Some("main"));
        drop(db);
        // A second start changes nothing.
        let again = Db::open(&p).unwrap().list_conversations().unwrap();
        assert_eq!(again, list);
    }

    #[test]
    fn a_new_conversation_gets_its_title_from_the_first_message_of_the_user() {
        let db = Db::open_in_memory().unwrap();
        let left = db.create_conversation().unwrap();
        assert!(left.starts_with("c-"));
        // An empty conversation without a title gives its place to the next one.
        let id = db.create_conversation().unwrap();
        assert_ne!(left, id);
        assert!(db.conversation(&left).unwrap().is_none());
        assert_eq!(db.conversation(&id).unwrap().unwrap().title, "");
        db.append_message(&id, &ChatMessage::assistant("Привет!"))
            .unwrap();
        db.append_message(&id, &ChatMessage::user("Какая погода в Казани?"))
            .unwrap();
        db.append_message(&id, &ChatMessage::user("А завтра?"))
            .unwrap();
        let c = db.conversation(&id).unwrap().unwrap();
        assert_eq!(c.title, "Какая погода в Казани?");
        assert_eq!(c.messages, 3);
        // A title of the user stays; an empty one brings the first message back.
        assert!(db.rename_conversation(&id, "  Погода  ").unwrap());
        db.append_message(&id, &ChatMessage::user("ещё")).unwrap();
        assert_eq!(db.conversation(&id).unwrap().unwrap().title, "Погода");
        assert!(db.rename_conversation(&id, "").unwrap());
        assert_eq!(
            db.conversation(&id).unwrap().unwrap().title,
            "Какая погода в Казани?"
        );
        assert!(!db.rename_conversation("nope", "x").unwrap());
        assert!(!db.pin_conversation("nope", true).unwrap());
        // One with messages, a pinned one and a named one stay.
        let pinned = db.create_conversation().unwrap();
        db.pin_conversation(&pinned, true).unwrap();
        let named = db.create_conversation().unwrap();
        db.rename_conversation(&named, "План").unwrap();
        db.create_conversation().unwrap();
        for kept in [&id, &pinned, &named] {
            assert!(db.conversation(kept).unwrap().is_some(), "{kept}");
        }
    }

    #[test]
    fn the_list_puts_pinned_first_then_the_newest() {
        let db = Db::open_in_memory().unwrap();
        for id in ["a", "b", "c"] {
            db.append_message(id, &ChatMessage::user(id)).unwrap();
        }
        age(&db, "a", "2026-11-01T10:00:00+03:00");
        age(&db, "b", "2026-11-03T10:00:00+03:00");
        // Later in time, though earlier as text: the offsets differ.
        age(&db, "c", "2026-11-03T09:00:00+00:00");
        let ids = |db: &Db| -> Vec<String> {
            db.list_conversations()
                .unwrap()
                .into_iter()
                .map(|c| c.id)
                .collect()
        };
        assert_eq!(ids(&db), ["c", "b", "a"]);
        assert!(db.pin_conversation("a", true).unwrap());
        assert_eq!(ids(&db), ["a", "c", "b"]);
        assert!(db.conversation("a").unwrap().unwrap().pinned);
    }

    #[test]
    fn the_cleanup_keeps_pinned_the_open_one_and_the_fresh_ones() {
        let db = Db::open_in_memory().unwrap();
        for id in ["old", "pinned", "open", "fresh", "empty"] {
            db.append_message(id, &ChatMessage::user(id)).unwrap();
        }
        db.clear_messages("empty").unwrap();
        for id in ["old", "pinned", "open", "empty"] {
            age(&db, id, "2026-10-01T10:00:00+03:00");
        }
        age(&db, "fresh", "2026-11-20T10:00:00+03:00");
        db.pin_conversation("pinned", true).unwrap();
        let before = chrono::DateTime::parse_from_rfc3339("2026-10-23T10:00:00+03:00").unwrap();
        assert_eq!(db.purge_before(before, "open").unwrap(), 2);
        let mut left: Vec<String> = db
            .list_conversations()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        left.sort();
        assert_eq!(left, ["fresh", "open", "pinned"]);
        assert!(db.load_messages("old").unwrap().is_empty());
        // Nothing older is left: a second run deletes nothing.
        assert_eq!(db.purge_before(before, "open").unwrap(), 0);
        // «Never»-like: a time far back keeps all.
        assert_eq!(db.purge_older_than(36500, "open").unwrap(), 0);
        assert_eq!(db.delete_unpinned("open").unwrap(), 1);
        let mut left: Vec<String> = db
            .list_conversations()
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        left.sort();
        assert_eq!(left, ["open", "pinned"]);
    }

    #[test]
    fn files_waiting_to_be_sent_outlive_the_cleanup() {
        let d = tempfile::tempdir().unwrap();
        let db = Db::open(&d.path().join("mika.db")).unwrap();
        let root = db.attachments_dir().unwrap().to_path_buf();
        let waiting = crate::attach::conversation_dir(&root, crate::attach::WAITING);
        std::fs::create_dir_all(&waiting).unwrap();
        std::fs::write(waiting.join("1.png"), b"png").unwrap();
        db.append_message("a", &ChatMessage::user("x")).unwrap();
        db.delete_conversation("a").unwrap();
        assert_eq!(db.delete_unpinned("").unwrap(), 0);
        assert_eq!(db.prune_attachments(std::time::Duration::ZERO).unwrap(), 0);
        assert!(waiting.join("1.png").exists());
    }

    #[test]
    fn a_conversation_opened_lately_is_not_old() {
        let db = Db::open_in_memory().unwrap();
        for id in ["opened", "old"] {
            db.append_message(id, &ChatMessage::user(id)).unwrap();
            age(&db, id, "2000-01-01T10:00:00+03:00");
        }
        // Opened just now, as the list does while the cleanup reads it.
        db.mark_opened("opened").unwrap();
        assert_eq!(db.purge_older_than(30, "elsewhere").unwrap(), 1);
        assert!(db.conversation("opened").unwrap().is_some());
        assert!(db.conversation("old").unwrap().is_none());
    }

    #[test]
    fn a_deleted_conversation_is_gone_from_the_search_and_its_vectors_too() {
        let db = Db::open_in_memory().unwrap();
        db.append_message("a", &ChatMessage::user("Мой кот Барсик"))
            .unwrap();
        db.append_message("a", &ChatMessage::assistant("Запомнила"))
            .unwrap();
        db.append_message("b", &ChatMessage::user("Про Барсика ещё"))
            .unwrap();
        db.with(|c| {
            c.execute_batch(
                "INSERT INTO message_vectors (message_id, model, embedding)
                 SELECT id, 'm', x'00' FROM messages",
            )
        })
        .unwrap();
        assert_eq!(db.search_messages(&["барсик".into()], 5).unwrap().len(), 2);
        assert_eq!(db.delete_conversation("a").unwrap(), 2);
        let found = db.search_messages(&["барсик".into()], 5).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].2, "Про Барсика ещё");
        assert!(db.conversation("a").unwrap().is_none());
        let vectors: i64 = db
            .with(|c| c.query_row("SELECT COUNT(*) FROM message_vectors", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(vectors, 1);
    }

    #[test]
    fn the_last_opened_conversation_opens_again() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.last_opened().unwrap(), None);
        db.append_message("a", &ChatMessage::user("1")).unwrap();
        db.append_message("b", &ChatMessage::user("2")).unwrap();
        age(&db, "a", "2026-11-01T10:00:00+03:00");
        age(&db, "b", "2026-11-02T10:00:00+03:00");
        // Never opened: the newest.
        assert_eq!(db.last_opened().unwrap().as_deref(), Some("b"));
        db.mark_opened("a").unwrap();
        assert_eq!(db.last_opened().unwrap().as_deref(), Some("a"));
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.mark_opened("b").unwrap();
        assert_eq!(db.last_opened().unwrap().as_deref(), Some("b"));
        db.delete_conversation("b").unwrap();
        assert_eq!(db.last_opened().unwrap().as_deref(), Some("a"));
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

    #[test]
    fn deleting_a_conversation_deletes_its_attachments() {
        let d = tempfile::tempdir().unwrap();
        let db = Db::open(&d.path().join("mika.db")).unwrap();
        let root = db.attachments_dir().unwrap().to_path_buf();
        assert_eq!(root, d.path().join("attachments"));
        for id in ["a", "b", "keep"] {
            db.append_message(id, &ChatMessage::user("x")).unwrap();
            let dir = crate::attach::conversation_dir(&root, id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("1.png"), b"png").unwrap();
        }
        db.delete_conversation("a").unwrap();
        assert!(!root.join("a").exists());
        assert!(root.join("b").join("1.png").exists());
        // The cleanup and «delete the unpinned» remove folders too.
        assert_eq!(db.delete_unpinned("keep").unwrap(), 1);
        assert!(!root.join("b").exists());
        assert!(root.join("keep").join("1.png").exists());
        // A database in memory has no folder and deletes nothing on disk.
        assert!(Db::open_in_memory().unwrap().attachments_dir().is_none());
    }

    #[test]
    fn folders_of_gone_conversations_are_pruned_but_fresh_ones_wait() {
        let d = tempfile::tempdir().unwrap();
        let db = Db::open(&d.path().join("mika.db")).unwrap();
        let root = db.attachments_dir().unwrap().to_path_buf();
        db.append_message("kept", &ChatMessage::user("x")).unwrap();
        for id in ["kept", "gone"] {
            std::fs::create_dir_all(root.join(id)).unwrap();
        }
        // Everything is fresh: a message not yet sent may wait there.
        let day = std::time::Duration::from_secs(86_400);
        assert_eq!(db.prune_attachments(day).unwrap(), 0);
        assert_eq!(db.prune_attachments(std::time::Duration::ZERO).unwrap(), 1);
        assert!(root.join("kept").exists());
        assert!(!root.join("gone").exists());
    }

    #[test]
    fn pictures_are_never_stored_in_the_body() {
        let db = Db::open_in_memory().unwrap();
        let mut m = ChatMessage::user_with_image("что тут?", "QUJD");
        m.attachments.push(crate::llm::Attachment {
            kind: crate::llm::AttachmentKind::Image,
            name: "a.png".into(),
            path: "c/1.png".into(),
            description: None,
        });
        db.append_message("c", &m).unwrap();
        let body: String = db
            .with(|c| c.query_row("SELECT body FROM messages", [], |r| r.get(0)))
            .unwrap();
        assert!(!body.contains("QUJD"), "{body}");
        let back = db.load_messages("c").unwrap();
        assert!(back[0].images.is_empty());
        assert_eq!(back[0].attachments, m.attachments);
    }

    #[test]
    fn cost_and_day_log_tables_exist() {
        let db = Db::open_in_memory().unwrap();
        for table in ["cost_usage", "cost_prices", "cost_alerts", "day_log"] {
            let n: i64 = db
                .with(|c| {
                    c.query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                        [table],
                        |r| r.get(0),
                    )
                })
                .unwrap();
            assert_eq!(n, 1, "{table}");
        }
    }
}
