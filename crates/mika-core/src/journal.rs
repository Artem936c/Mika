//! The journal: what Mika did, what failed and why (decision 0036).
//!
//! The code writes through the `log` macros as before; `Journal` is the sink
//! behind them. A record is one line of `mika.log`: time, level, category and
//! message, separated by tabs. The category is the target of the record,
//! `log::info!(target: "tool", …)`; without one it comes from the module
//! path (`category::category_of`).
//!
//! What never gets into a record:
//! - secrets: every message passes the scrub, which cannot be switched off;
//! - texts of the user and of the model, window titles, tool arguments and
//!   the like, unless `log.content` is on: the code asks `content_allowed`
//!   or wraps the text in `content`;
//! - anything of a blind-zone window, in any mode: the code that sees such a
//!   window does not log it.
//!
//! The app has one journal, set up by `install`; tests open their own with
//! `Journal::open` over a temporary folder.

pub mod category;
pub mod files;
pub mod line;
/// A check of the source, for the tests of every crate.
#[cfg(any(test, feature = "testing"))]
pub mod lint;
pub mod read;
pub mod run;
pub mod scrub;
pub mod sink;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

pub use category::{category_of, CATEGORIES};
pub use line::{Level, Row};
pub use read::{Page, Query, DEFAULT_PAGE, MAX_PAGE};
pub use run::{panic_hook, PanicHook};
pub use scrub::{remember_secret, scrub, MAX_MESSAGE_CHARS};
pub use sink::{Journal, Options, Summary, Tail, MAX_FILE_BYTES, RING_RECORDS};

use crate::config::LogConfig;

/// How much of a text `content` lets through when texts are allowed.
pub const CONTENT_CHARS: usize = 500;
/// How much of an error text `error_text` keeps.
pub const ERROR_CHARS: usize = 300;

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("the journal is already installed")]
    AlreadyInstalled,
    #[error("another logger is already installed")]
    LoggerTaken,
    #[error("journal io error: {0}")]
    Io(#[from] std::io::Error),
}

static GLOBAL: OnceLock<Arc<Journal>> = OnceLock::new();
/// `log.content` of the installed journal, for `content_allowed`.
static CONTENT: AtomicBool = AtomicBool::new(false);

/// Counts the turns of this run.
static TURNS: AtomicU64 = AtomicU64::new(0);

tokio::task_local! {
    /// The turn the task works for.
    static TURN: u64;
}

/// A number for a turn that starts now. The records of one turn carry it as
/// `turn=N`: the start and the end, the requests to the model, the tool
/// calls, the questions. A request without it was made by something else: a
/// background job, a summary, the voice.
pub fn next_turn() -> u64 {
    TURNS.fetch_add(1, Ordering::Relaxed) + 1
}

/// Runs `work` as the turn with this number: `turn()` answers it inside, in
/// this task only.
pub async fn in_turn<F: std::future::Future>(turn: u64, work: F) -> F::Output {
    TURN.scope(turn, work).await
}

/// The number of the turn this task works for; `None` outside a turn.
pub fn turn() -> Option<u64> {
    TURN.try_with(|turn| *turn).ok()
}

/// `turn=N ` for a record, or nothing outside a turn.
pub fn turn_field() -> String {
    turn().map_or_else(String::new, |turn| format!("turn={turn} "))
}

/// What the `log` crate holds: it passes every record to the installed journal.
struct Installed;

static INSTALLED: Installed = Installed;

impl log::Log for Installed {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        GLOBAL
            .get()
            .is_some_and(|journal| journal.enabled(metadata))
    }

    fn log(&self, record: &log::Record<'_>) {
        if let Some(journal) = GLOBAL.get() {
            journal.log(record);
        }
    }

    fn flush(&self) {
        flush();
    }
}

/// Opens the journal of the app and makes it the sink of the `log` macros.
/// Once per process.
pub fn install(options: Options) -> Result<Arc<Journal>, JournalError> {
    if GLOBAL.get().is_some() {
        return Err(JournalError::AlreadyInstalled);
    }
    log::set_logger(&INSTALLED).map_err(|_| JournalError::LoggerTaken)?;
    let journal = Journal::open(options);
    let journal = GLOBAL.get_or_init(|| journal).clone();
    follow(&journal);
    Ok(journal)
}

/// The journal of the app; `None` before `install`.
pub fn global() -> Option<Arc<Journal>> {
    GLOBAL.get().cloned()
}

/// Applies changed settings to the journal of the app at once.
pub fn apply(config: &LogConfig) {
    if let Some(journal) = GLOBAL.get() {
        journal.apply(config);
        follow(journal);
    }
}

/// Writes what the journal of the app has buffered to its file. Call it
/// before the process exits.
pub fn flush() {
    if let Some(journal) = GLOBAL.get() {
        journal.flush();
    }
}

/// Makes the process-wide switches follow the journal of the app.
fn follow(journal: &Journal) {
    CONTENT.store(journal.content_allowed(), Ordering::Relaxed);
    log::set_max_level(journal.level().filter());
}

/// Makes a panic leave an `ERROR` record in the journal of the app before the
/// hook that was set before runs. May be called before `install`.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(run::hook(global, previous));
}

/// Whether texts and arguments may be written (`log.content` of the journal
/// of the app). False before `install`.
pub fn content_allowed() -> bool {
    CONTENT.load(Ordering::Relaxed)
}

/// A text for a record: its beginning when texts are allowed, otherwise only
/// its length, as `<hidden N chars>`.
///
/// Wrap in it everything that is the user's or the model's: what was said,
/// typed or dictated, window titles, the clipboard, file contents, values of
/// tool arguments, search requests, facts of the memory. Nothing of a
/// blind-zone window is logged at all, with or without this.
pub fn content(text: &str) -> String {
    content_if(content_allowed(), text)
}

/// `content` with the switch given.
pub fn content_if(allowed: bool, text: &str) -> String {
    if !allowed {
        return hidden(text);
    }
    // Scrubbed before the cut: a secret that the cut would split in two
    // must not leave its first half behind.
    shortened(&scrub::scrub(text), CONTENT_CHARS)
}

/// What stands in a record in place of a text that is not written.
fn hidden(text: &str) -> String {
    format!("<hidden {} chars>", text.chars().count())
}

/// The first `max` characters of a text and how many more there were.
fn shortened(text: &str, max: usize) -> String {
    match scrub::head(text, max) {
        (all, 0) => all.to_string(),
        (start, more) => format!("{start} <+{more} chars>"),
    }
}

/// An address for a record: without its query and fragment unless texts are
/// allowed. What a query carries is the user's.
pub fn address(url: &str) -> String {
    address_if(content_allowed(), url)
}

/// `address` with the switch given.
pub fn address_if(allowed: bool, url: &str) -> String {
    if allowed {
        return content_if(true, url);
    }
    match url.find(['?', '#']) {
        Some(at) => format!("{}?<hidden>", &url[..at]),
        None => url.to_string(),
    }
}

/// The text of an error for a record, no longer than `ERROR_CHARS`. For the
/// app's own errors and those of the system and of a provider. An error that
/// may quote what a tool was given or what it saw goes through
/// `guarded_error`.
pub fn error_text(error: &dyn std::fmt::Display) -> String {
    // Scrubbed before the cut, as in `content_if`.
    shortened(&scrub::scrub(&error.to_string()), ERROR_CHARS)
}

/// What a record carries in place of everything about a call that the blind
/// zone refused: its arguments and its error.
pub const BLIND_MARK: &str = "<blind zone>";

/// Whether a text reads like the title or the address of a blind-zone window.
fn names_blind_zone(text: &str) -> bool {
    crate::policy::blind::is_blind_title(text) || crate::policy::blind::is_blind_url(text)
}

/// Whether an error is the refusal of the blind zone itself, not a text that
/// only has one of its words in it.
pub fn is_blind_refusal(error: &dyn std::fmt::Display) -> bool {
    error
        .to_string()
        .contains(crate::policy::blind::BLIND_REFUSAL)
}

/// What stands in place of a text that has a word of the blind zone in it.
/// It does not say that such a window was there: a file called
/// `bank_statement.mp4` reads the same.
fn like_blind(text: &str) -> String {
    format!(
        "<hidden: reads like the blind zone, {} chars>",
        text.chars().count()
    )
}

/// `content` for a text that may name a window or a web address: the
/// arguments of a tool call, a target. A text that reads like a blind-zone
/// title or address is never written, whatever the switch says.
pub fn guarded(text: &str) -> String {
    guarded_if(content_allowed(), text)
}

/// `guarded` with the switch given.
pub fn guarded_if(allowed: bool, text: &str) -> String {
    if allowed && names_blind_zone(text) {
        return like_blind(text);
    }
    content_if(allowed, text)
}

/// The text of an error that may quote what is not the app's own: the error
/// of a tool (it names windows, files, arguments, what a window shows), the
/// answer of an MCP server, the complaint of FFmpeg. Written only while
/// texts are allowed, and never when it reads like the blind zone. Say what
/// kind of error it was in a field beside it: that is always written.
pub fn guarded_error(error: &dyn std::fmt::Display) -> String {
    guarded_error_if(content_allowed(), error)
}

/// `guarded_error` with the switch given.
pub fn guarded_error_if(allowed: bool, error: &dyn std::fmt::Display) -> String {
    let text = error.to_string();
    if is_blind_refusal(&text) {
        return BLIND_MARK.to_string();
    }
    if !allowed {
        return hidden(&text);
    }
    if names_blind_zone(&text) {
        return like_blind(&text);
    }
    error_text(&text)
}

/// The complaint of a program for a record. With texts off only its last
/// part is kept, the reason after the last `: `, and only when that part
/// quotes nothing: `D:/videos/a.mp4: Invalid data found when processing
/// input` leaves the reason and the size of what stood before it.
pub fn complaint(text: &str) -> String {
    complaint_if(content_allowed(), text)
}

/// `complaint` with the switch given.
pub fn complaint_if(allowed: bool, text: &str) -> String {
    if allowed {
        return guarded_error_if(true, &text);
    }
    let text = text.trim();
    let (before, reason) = match text.rfind(": ") {
        Some(at) => (&text[..at], &text[at + 2..]),
        None => ("", text),
    };
    let plain = !reason.is_empty()
        && reason.chars().count() <= 120
        && !reason.contains(['\'', '"', '`', '\\', '/', '«', '»', '<', '>', '[', ']'])
        && !names_blind_zone(reason);
    match (plain, before.is_empty()) {
        (false, _) => hidden(text),
        (true, true) => scrub::scrub(reason),
        (true, false) => format!("{}: {}", hidden(before), scrub::scrub(reason)),
    }
}

impl Journal {
    /// `content` by the switch of this journal.
    pub fn content(&self, text: &str) -> String {
        content_if(self.content_allowed(), text)
    }
}

#[cfg(test)]
mod tests;
