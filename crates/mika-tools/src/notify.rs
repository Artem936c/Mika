//! Notifications of other programs, read from the Windows notification
//! database (`wpndatabase.db`, read only). Programs without a package cannot
//! use `UserNotificationListener`, the file is the way that works.
//! Nothing here runs until the user turns notifications on.

use std::path::{Path, PathBuf};

use mika_core::policy::blind::is_blind_title;

/// One toast as Windows keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub id: i64,
    /// Application id from Windows: `…TelegramDesktop…`, `C:\…\app.exe`.
    pub app_id: String,
    /// Short program name for people: `Telegram`.
    pub app: String,
    /// First text line: sender or subject.
    pub title: String,
    /// The other lines.
    pub text: String,
}

/// Programs people talk through: notifications of these are kept by default.
pub const MESSENGERS: &[(&str, &str)] = &[
    ("telegram", "Telegram"),
    ("ayugram", "Telegram"),
    ("kotatogram", "Telegram"),
    ("whatsapp", "WhatsApp"),
    ("viber", "Viber"),
    ("discord", "Discord"),
    ("skype", "Skype"),
    ("teams", "Teams"),
    ("slack", "Slack"),
    ("signal", "Signal"),
    ("vk", "VK"),
    ("outlook", "Outlook"),
    ("thunderbird", "Thunderbird"),
    ("windowscommunicationsapps", "Почта"),
];

/// Words of codes and passwords: such notifications are never read.
const SECRET_MARKERS: &[&str] = &[
    "код",
    "code",
    "парол",
    "password",
    "otp",
    "пин",
    "pin",
    "cvv",
    "списан",
    "зачислен",
    "баланс",
    "карт",
    "перевод",
    "оплат",
];

/// Short program name for an application id.
pub fn app_name(app_id: &str) -> String {
    let low = app_id.to_lowercase();
    if let Some((_, name)) = MESSENGERS.iter().find(|(k, _)| low.contains(k)) {
        return (*name).to_string();
    }
    // `Publisher.Product_hash!App` or a path to the program.
    let tail = app_id
        .rsplit(['!', '\\', '/'])
        .find(|s| !s.is_empty())
        .unwrap_or(app_id);
    let tail = tail.trim_end_matches(".exe").trim_end_matches(".EXE");
    tail.rsplit('.').next().unwrap_or(tail).to_string()
}

pub fn is_messenger(app_id: &str) -> bool {
    let low = app_id.to_lowercase();
    MESSENGERS.iter().any(|(k, _)| low.contains(k))
}

/// True when the notification may carry a code, a password or money: it is
/// left alone, as blind-zone windows are.
pub fn is_private(t: &Toast) -> bool {
    // A bank or a password manager as the sender or the program.
    if is_blind_title(&format!("{} {}", t.app_id, t.title)) {
        return true;
    }
    let all = format!("{} {}", t.title, t.text).to_lowercase();
    let words: Vec<&str> = all
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let marked = words
        .iter()
        .any(|w| SECRET_MARKERS.iter().any(|m| w.starts_with(m)));
    // A one-time code: 4 to 8 digits standing alone.
    let code = words
        .iter()
        .any(|w| (4..=8).contains(&w.len()) && w.chars().all(|c| c.is_ascii_digit()));
    marked && code || all.contains("парол") || all.contains("password")
}

/// Text lines of a toast payload: the `<text>` elements, unescaped.
pub fn toast_lines(payload: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = payload;
    while let Some(start) = rest.find("<text") {
        let after = &rest[start + 5..];
        // `<textBox>` and the like are other elements.
        if !after.starts_with('>') && !after.starts_with(' ') && !after.starts_with('/') {
            rest = after;
            continue;
        }
        let Some(open_end) = after.find('>') else {
            break;
        };
        if after[..open_end].ends_with('/') {
            rest = &after[open_end + 1..];
            continue;
        }
        let body = &after[open_end + 1..];
        let Some(close) = body.find("</text>") else {
            break;
        };
        let line = unescape(body[..close].trim());
        if !line.is_empty() {
            out.push(line);
        }
        rest = &body[close + 7..];
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#10;", "\n")
        .replace("&#13;", "")
        .replace("&amp;", "&")
}

/// Builds a toast from a database row; `None` when it has no text.
pub fn toast(id: i64, app_id: &str, payload: &str) -> Option<Toast> {
    let mut lines = toast_lines(payload).into_iter();
    let title = lines.next()?;
    let text: Vec<String> = lines.collect();
    Some(Toast {
        id,
        app_id: app_id.to_string(),
        app: app_name(app_id),
        title,
        text: text.join("\n"),
    })
}

/// The Windows notification database of this user.
pub fn default_database() -> PathBuf {
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    Path::new(&local).join(r"Microsoft\Windows\Notifications\wpndatabase.db")
}

/// Reads toasts from the notification database, read only.
pub struct ToastReader {
    path: PathBuf,
}

impl ToastReader {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn open(&self) -> Result<rusqlite::Connection, String> {
        use rusqlite::OpenFlags;
        rusqlite::Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("база уведомлений Windows не открылась: {e}"))
    }

    /// The newest notification id: toasts up to it are old news.
    pub fn last_id(&self) -> Result<i64, String> {
        let db = self.open()?;
        db.query_row("SELECT IFNULL(MAX(Id), 0) FROM Notification", [], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())
    }

    /// Toasts newer than `after`, oldest first.
    pub fn after(&self, after: i64) -> Result<Vec<Toast>, String> {
        let db = self.open()?;
        let mut st = db
            .prepare(
                "SELECT n.Id, h.PrimaryId, CAST(n.Payload AS TEXT) FROM Notification n \
                 JOIN NotificationHandler h ON h.RecordId = n.HandlerId \
                 WHERE n.Id > ?1 AND n.Type = 'toast' ORDER BY n.Id LIMIT 50",
            )
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([after], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows.flatten() {
            if let Some(t) = toast(row.0, &row.1, &row.2) {
                out.push(t);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TELEGRAM: &str =
        "TelegramMessengerLLP.TelegramDesktop_t4vj0pshhgkwm!Telegram.TelegramDesktop.Store";

    #[test]
    fn lines_come_from_text_elements() {
        let p = r#"<toast><visual><binding template="ToastGeneric"><text id="1">Алексей Новиков</text><text/><text id="2">Привет &amp; пока &lt;3</text><textBox id="x">no</textBox></binding></visual></toast>"#;
        assert_eq!(
            toast_lines(p),
            vec![
                "Алексей Новиков".to_string(),
                "Привет & пока <3".to_string()
            ]
        );
        let t = toast(7, TELEGRAM, p).unwrap();
        assert_eq!(
            (t.app.as_str(), t.title.as_str(), t.text.as_str()),
            ("Telegram", "Алексей Новиков", "Привет & пока <3")
        );
        assert!(toast(8, TELEGRAM, "<toast/>").is_none());
    }

    #[test]
    fn program_names_are_short() {
        assert_eq!(app_name(TELEGRAM), "Telegram");
        assert_eq!(app_name(r"C:\Program Files\Foo\foo.exe"), "foo");
        assert_eq!(app_name("Microsoft.Windows.Explorer"), "Explorer");
        assert!(is_messenger(TELEGRAM) && !is_messenger("Symantec.ToastUI"));
    }

    #[test]
    fn codes_passwords_and_banks_stay_private() {
        let t = |title: &str, text: &str| Toast {
            id: 1,
            app_id: TELEGRAM.into(),
            app: "Telegram".into(),
            title: title.into(),
            text: text.into(),
        };
        assert!(is_private(&t("Telegram", "Код для входа: 48213")));
        assert!(is_private(&t("Сбербанк", "Списание 500 р")));
        assert!(is_private(&t("Сервис", "Ваш новый пароль готов")));
        assert!(!is_private(&t("Алексей", "Встречаемся в 1830 у входа")));
        assert!(!is_private(&t("Алексей", "Привет, как дела?")));
    }

    #[test]
    #[ignore = "reads the notification database of this computer"]
    fn the_real_database_is_readable() {
        let r = ToastReader::new(default_database());
        let last = r.last_id().unwrap();
        let all = r.after(0).unwrap();
        println!("last id {last}, toasts {}", all.len());
        for t in all.iter().rev().take(3) {
            println!("{} | {} | private {}", t.app, t.title, is_private(t));
        }
    }

    #[test]
    fn reader_takes_only_new_toasts_from_a_copy_of_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wpn.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE NotificationHandler (RecordId INTEGER PRIMARY KEY, PrimaryId TEXT);
             CREATE TABLE Notification ([Order] INTEGER PRIMARY KEY, Id INTEGER, HandlerId INTEGER, Type TEXT, Payload BLOB);
             INSERT INTO NotificationHandler VALUES (1, 'TelegramDesktop');
             INSERT INTO Notification VALUES (1, 10, 1, 'toast', CAST('<toast><text>Аня</text><text>старое</text></toast>' AS BLOB));
             INSERT INTO Notification VALUES (2, 11, 1, 'tile', CAST('<tile><text>плитка</text></tile>' AS BLOB));
             INSERT INTO Notification VALUES (3, 12, 1, 'toast', CAST('<toast><text>Аня</text><text>новое</text></toast>' AS BLOB));",
        )
        .unwrap();
        drop(db);
        let r = ToastReader::new(path);
        assert_eq!(r.last_id().unwrap(), 12);
        let new = r.after(10).unwrap();
        assert_eq!(new.len(), 1);
        assert_eq!((new[0].id, new[0].text.as_str()), (12, "новое"));
    }
}
