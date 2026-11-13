//! History of copied texts: the last ones, in memory only, gone when she exits.
//! Texts that look like passwords or keys, texts marked by password managers
//! and copies from blind-zone windows are never kept.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::{Clipboard, SavedClip};
use crate::util::{arg_u64, schema, truncate};

/// Texts kept.
pub const KEEP: usize = 50;
/// Longest text kept, characters.
const MAX_CHARS: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    /// Unix seconds.
    pub at: i64,
    pub text: String,
    /// Program it was copied in: `chrome.exe`.
    pub app: String,
}

/// Prefixes of well-known secret keys and tokens.
const KEY_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "pk_live",
    "rk_live",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "akia",
    "aiza",
    "ya29.",
    "eyj",
];

/// A link or a path by its plain signs: a scheme, a drive, a share, a leading
/// slash, a site name first or a file name with an extension last. A slash
/// alone proves nothing: passwords and base64 keys have them too.
fn path_like(t: &str) -> bool {
    let lower = t.to_lowercase();
    if lower.contains("://") || lower.starts_with("www.") || lower.starts_with("mailto:") {
        return true;
    }
    let b = t.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/') {
        return true;
    }
    if ["\\\\", "/", "./", "../", ".\\", "..\\", "~/", "~\\"]
        .iter()
        .any(|p| t.starts_with(p))
    {
        return true;
    }
    let parts: Vec<&str> = t.split(['/', '\\']).collect();
    if parts.len() < 2 {
        return false;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    // `site.ru/…`
    let site = first.rsplit_once('.').is_some_and(|(name, zone)| {
        !name.is_empty() && zone.len() >= 2 && zone.chars().all(|c| c.is_ascii_alphabetic())
    });
    // `…/name.ext`
    let file = last.rsplit_once('.').is_some_and(|(name, ext)| {
        !name.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && ext.chars().any(|c| c.is_ascii_alphabetic())
    });
    site || file
}

/// Whether a copied text looks like a password, a key or a one-time code.
pub fn looks_secret(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return false;
    }
    let lower = t.to_lowercase();
    if KEY_PREFIXES.iter().any(|p| lower.starts_with(p)) && t.len() >= 16 {
        return true;
    }
    let len = t.chars().count();
    // A one-time code: 4 to 8 digits alone.
    if (4..=8).contains(&len) && t.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if !(8..=128).contains(&len) {
        return false;
    }
    let lowers = t.chars().any(|c| c.is_lowercase());
    let uppers = t.chars().any(|c| c.is_uppercase());
    let digits = t.chars().any(|c| c.is_ascii_digit());
    let marks = t
        .chars()
        .any(|c| !c.is_alphanumeric() && !matches!(c, '.' | '/' | ':' | '-' | '_'));
    // A link or a path is not a password.
    if path_like(t) {
        return false;
    }
    let classes = [lowers, uppers, digits, marks]
        .iter()
        .filter(|x| **x)
        .count();
    classes >= 3 || (len >= 24 && classes >= 2 && digits)
}

#[derive(Clone, Default)]
pub struct ClipHistory {
    items: Arc<Mutex<Vec<Clip>>>,
    /// Unix milliseconds until which she uses the clipboard herself: her own
    /// copies and the text she puts back are not kept.
    quiet_until: Arc<AtomicI64>,
    /// Her uses of the clipboard going on now.
    holds: Arc<AtomicUsize>,
}

/// The history keeps quiet this long after she gives the clipboard back: the
/// app looks at the clipboard once a second, and what she put back is not a
/// new copy of the user.
const QUIET_AFTER_MS: i64 = 3000;

/// While it lives, and `QUIET_AFTER_MS` after, changes of the clipboard are hers.
pub struct ClipHold(ClipHistory);

impl Drop for ClipHold {
    fn drop(&mut self) {
        // The pause first: no moment without either.
        self.0.pause_for(QUIET_AFTER_MS);
        self.0.holds.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The user's clipboard lent to her. What it held is saved when the loan
/// starts and put back when the loan ends, after an error or «Стоп» too, with
/// pictures, files and the marks of password managers. Meanwhile the history
/// keeps none of the changes.
pub struct ClipLoan {
    clipboard: Arc<dyn Clipboard>,
    saved: Option<SavedClip>,
    // Dropped after the clipboard is put back.
    _hold: ClipHold,
}

impl ClipLoan {
    pub fn start(clipboard: Arc<dyn Clipboard>, history: &ClipHistory) -> Self {
        let hold = history.hold();
        let saved = match clipboard.save() {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("clipboard not saved before using it: {e}");
                None
            }
        };
        Self {
            clipboard,
            saved,
            _hold: hold,
        }
    }

    /// Whether the old clipboard will come back.
    pub fn saved(&self) -> bool {
        self.saved.is_some()
    }
}

impl Drop for ClipLoan {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take() {
            if let Err(e) = self.clipboard.restore(&saved) {
                log::warn!("clipboard not put back: {e}");
            }
        }
    }
}

fn now_ms() -> i64 {
    chrono::Local::now().timestamp_millis()
}

impl ClipHistory {
    /// Keeps a copied text. Returns false when it was not kept.
    pub fn push(&self, text: &str, app: &str, at: i64) -> bool {
        let text = text.trim();
        if text.is_empty() || self.paused() || looks_secret(text) {
            return false;
        }
        let text: String = text.chars().take(MAX_CHARS).collect();
        let mut items = self.items.lock();
        // Copying the same text again moves it up.
        items.retain(|c| c.text != text);
        items.push(Clip {
            at,
            text,
            app: app.to_string(),
        });
        let extra = items.len().saturating_sub(KEEP);
        items.drain(..extra);
        true
    }

    /// Newest first; with a query, only texts that contain every word of it.
    pub fn find(&self, query: &str, limit: usize) -> Vec<Clip> {
        let words: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        self.items
            .lock()
            .iter()
            .rev()
            .filter(|c| {
                let t = c.text.to_lowercase();
                words.iter().all(|w| t.contains(w))
            })
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        self.items.lock().clear();
    }

    pub fn len(&self) -> usize {
        self.items.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// She is about to use the clipboard herself for up to `ms`: the selection
    /// shortcut, pasting a reply. Changes meanwhile are not the user's copies.
    pub fn pause_for(&self, ms: i64) {
        self.quiet_until.store(now_ms() + ms, Ordering::Relaxed);
    }

    /// She uses the clipboard herself until the hold is dropped, and a little after.
    pub fn hold(&self) -> ClipHold {
        self.holds.fetch_add(1, Ordering::SeqCst);
        ClipHold(self.clone())
    }

    pub fn paused(&self) -> bool {
        self.holds.load(Ordering::SeqCst) > 0 || self.quiet_until.load(Ordering::Relaxed) > now_ms()
    }
}

/// `5 мин назад`, `2 ч назад`.
fn ago(seconds: i64) -> String {
    match seconds.max(0) {
        s if s < 60 => "только что".into(),
        s if s < 3600 => format!("{} мин назад", s / 60),
        s => format!("{} ч назад", s / 3600),
    }
}

pub struct ClipboardHistoryTool {
    pub history: ClipHistory,
    /// `tracking.clipboard` from the config.
    pub enabled: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for ClipboardHistoryTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "clipboard_history",
            "Что пользователь копировал раньше: последние тексты из буфера обмена, новые первыми. С query — только тексты с этими словами.",
            schema(
                &[
                    ("query", "string", "Слова, которые есть в тексте"),
                    ("limit", "integer", "Сколько текстов, по умолчанию 5"),
                ],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let q = arg_str_opt(args, "query").unwrap_or("").trim();
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть историю буфера обмена",
            if q.is_empty() {
                "последние тексты"
            } else {
                q
            },
            "только чтение; пароли и ключи в историю не попадают",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Ok(ToolOutput::new(
                "История буфера обмена выключена: включить можно в настройках, раздел «Инструменты».",
            ));
        }
        let query = arg_str_opt(&args, "query").unwrap_or("");
        let limit = arg_u64(&args, "limit").unwrap_or(5).clamp(1, 20) as usize;
        let found = self.history.find(query, limit);
        if found.is_empty() {
            return Ok(ToolOutput::new(if query.trim().is_empty() {
                "В истории буфера пока пусто.".to_string()
            } else {
                format!("В истории буфера нет текста со словами «{}».", query.trim())
            }));
        }
        let now = chrono::Local::now().timestamp();
        let lines: Vec<String> = found
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let from = if c.app.is_empty() {
                    String::new()
                } else {
                    format!(", {}", crate::usage::app_label(&c.app))
                };
                format!(
                    "{}. ({}{from}) {}",
                    i + 1,
                    ago(now - c.at),
                    truncate(&c.text, 1500)
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
    use serde_json::json;

    #[test]
    fn passwords_keys_and_codes_are_recognised() {
        for secret in [
            "Qw3rty!pass",
            "sk-proj-abcdefghijklmnop1234",
            "ghp_1234567890abcdefghij",
            "482915",
            "aB3dE6gH9jK2mN5pQ8rS1tU4",
            // A slash or a backslash inside does not make it a path.
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "Qw3rty/2024!",
            r"Pa\ss9word",
        ] {
            assert!(looks_secret(secret), "{secret}");
        }
        for plain in [
            "Привет, как дела?",
            "https://example.com/Path?a=1&B=2",
            "github.com/Mika-AI/Repo2",
            r"C:\Users\Me\Документы\Отчёт-2026.docx",
            r"\\Server\Share\Plan2026",
            "/usr/Local/Bin2",
            "src/Main2.rs",
            "договор",
            "12,5",
            "Иванов",
            "user@example.com",
        ] {
            assert!(!looks_secret(plain), "{plain}");
        }
    }

    #[test]
    fn history_keeps_the_newest_without_repeats_and_secrets() {
        let h = ClipHistory::default();
        assert!(h.push("первый", "a.exe", 1));
        assert!(h.push("второй текст", "b.exe", 2));
        assert!(h.push("первый", "a.exe", 3));
        assert!(!h.push("Qw3rty!pass", "a.exe", 4));
        assert!(!h.push("   ", "a.exe", 5));
        h.pause_for(60_000);
        assert!(!h.push("её собственное", "mika.exe", 6));
        h.pause_for(0);
        let all = h.find("", 10);
        assert_eq!(
            all.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            ["первый", "второй текст"]
        );
        assert_eq!(h.find("ВТОРОЙ", 10).len(), 1);
        for i in 0..KEEP + 5 {
            h.push(&format!("текст {i}"), "", i as i64);
        }
        assert_eq!(h.len(), KEEP);
        assert_eq!(h.find("", 1)[0].text, format!("текст {}", KEEP + 4));
    }

    #[test]
    fn a_loan_puts_the_marked_clipboard_back_and_keeps_the_history_quiet() {
        let (e, f) = crate::testing::env();
        let history = ClipHistory::default();
        *f.clip_watch.lock() = Some(history.clone());
        *f.clipboard.lock() = "unrated-bloating-shrimp".into();
        *f.clip_private.lock() = true;
        {
            let loan = ClipLoan::start(e.clipboard.clone(), &history);
            assert!(loan.saved());
            assert!(history.paused());
            e.clipboard.set("её текст 🙂").unwrap();
            assert!(!*f.clip_private.lock());
        }
        assert_eq!(*f.clipboard.lock(), "unrated-bloating-shrimp");
        assert!(*f.clip_private.lock(), "the marks come back");
        assert!(history.paused(), "quiet a little after");
        assert!(history.is_empty());

        // Paused while any hold lives.
        let outer = history.hold();
        drop(history.hold());
        history.pause_for(0);
        assert!(history.paused());
        drop(outer);
        history.pause_for(0);
        assert!(!history.paused());
    }

    #[tokio::test]
    async fn the_tool_answers_only_when_on() {
        let history = ClipHistory::default();
        history.push(
            "номер заказа 12-345",
            "chrome.exe",
            chrono::Local::now().timestamp(),
        );
        let enabled = Arc::new(AtomicBool::new(false));
        let tool = ClipboardHistoryTool {
            history,
            enabled: enabled.clone(),
        };
        let off = run(&tool, json!({})).await.unwrap().text;
        assert!(off.contains("выключена"), "{off}");
        enabled.store(true, Ordering::Relaxed);
        let on = run(&tool, json!({"query": "заказа"})).await.unwrap().text;
        assert!(
            on.starts_with("1. (только что, Chrome) номер заказа"),
            "{on}"
        );
    }
}
