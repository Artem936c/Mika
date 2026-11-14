//! Two ways to learn that a run ended badly: the record a panic leaves, and
//! the marker file that is still there when the next run starts.

use std::panic::PanicHookInfo;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use chrono::{Local, SecondsFormat};

use super::category::APP;
use super::files::MARKER;
use super::line::Level;
use super::sink::Journal;

/// A panic hook, as `std::panic::set_hook` takes it.
pub type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// How much of the marker is quoted in the record about it.
const MARKER_CHARS: usize = 120;

impl Journal {
    /// Marks the start of a run and tells whether the run before it ended
    /// without `end_run`: a crash, a kill, a power cut. When it did, a `WARN`
    /// record says so, and `previous_run_crashed` stays true until
    /// `acknowledge_crash`. A journal kept in memory has no marker. Called
    /// once per run; a second call only repeats what is known.
    pub fn begin_run(&self) -> bool {
        let Some(dir) = &self.dir else {
            return false;
        };
        if self.began.swap(true, Ordering::Relaxed) {
            return self.previous_run_crashed();
        }
        let marker = dir.join(MARKER);
        let left = std::fs::read_to_string(&marker).ok();
        let crashed = left.is_some() || marker.exists();
        if crashed {
            let left = left.unwrap_or_default();
            let about: String = left
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(MARKER_CHARS)
                .collect();
            self.crashed.store(true, Ordering::Relaxed);
            self.write(
                Level::Warn,
                APP,
                &format!("previous run did not exit cleanly ({about})"),
            );
        }
        let now = Local::now().to_rfc3339_opts(SecondsFormat::Secs, false);
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(
            &marker,
            format!("pid {} started {now}\n", std::process::id()),
        );
        crashed
    }

    /// Marks a clean exit: removes the marker and writes the buffer out.
    pub fn end_run(&self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_file(dir.join(MARKER));
        }
        self.flush();
    }

    /// The run before this one did not exit cleanly, and nobody has
    /// acknowledged it yet.
    pub fn previous_run_crashed(&self) -> bool {
        self.crashed.load(Ordering::Relaxed)
    }

    /// The user has seen the notice about the unclean exit.
    pub fn acknowledge_crash(&self) {
        self.crashed.store(false, Ordering::Relaxed);
    }
}

/// How much of a panic message is kept while texts are off.
const PANIC_HEAD_CHARS: usize = 200;

/// The message of a panic for a record. A panic may quote the text it choked
/// on: slicing a string in the middle of a letter prints the string, and
/// `unwrap` on an error prints the error. So while texts are off only the
/// opening words are kept, up to the first `: `, `; `, double quote or line
/// end, and the size of the rest. With texts on the message is written
/// unless it reads like the blind zone.
fn panic_message(message: &str, content: bool) -> String {
    if content {
        return super::guarded_if(true, message);
    }
    let cut = [": ", "; ", "\"", "\n"]
        .iter()
        .filter_map(|mark| message.find(mark))
        .min()
        .unwrap_or(message.len());
    let (head, more) = super::scrub::head(&message[..cut], PANIC_HEAD_CHARS);
    let hidden = message[cut..].chars().count() + more;
    if super::names_blind_zone(head) {
        return super::guarded_if(true, message);
    }
    match hidden {
        0 => head.to_string(),
        hidden => format!("{head} <hidden {hidden} chars>"),
    }
}

/// What a panic says: the thread, the place and the message. The place is
/// always written; the message by `panic_message`.
fn describe(info: &PanicHookInfo<'_>, content: bool) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");
    let message = panic_message(message, content);
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("unnamed");
    match info.location() {
        Some(at) => format!(
            "panic in thread '{thread}' at {}:{}:{}: {message}",
            at.file(),
            at.line(),
            at.column()
        ),
        None => format!("panic in thread '{thread}': {message}"),
    }
}

/// A hook that writes the panic to `journal` as an `ERROR` record of `app`,
/// forces the file to the disk and then calls `previous`.
pub fn panic_hook(journal: Arc<Journal>, previous: PanicHook) -> PanicHook {
    hook(move || Some(journal.clone()), previous)
}

/// The same for a journal that is looked up when the panic happens.
pub(super) fn hook(
    journal: impl Fn() -> Option<Arc<Journal>> + Sync + Send + 'static,
    previous: PanicHook,
) -> PanicHook {
    Box::new(move |info| {
        if let Some(journal) = journal() {
            journal.record_panic(&describe(info, journal.content_allowed()));
        }
        previous(info);
    })
}

#[cfg(test)]
mod tests {
    use super::panic_message;

    #[test]
    fn a_panic_message_keeps_its_opening_words_while_texts_are_off() {
        // What the standard library says when a string is cut inside a letter.
        let sliced =
            "byte index 1 is not a char boundary; it is inside 'п' (bytes 0..2) of `письмо Ане`";
        assert_eq!(
            panic_message(sliced, false),
            "byte index 1 is not a char boundary <hidden 47 chars>"
        );
        assert_eq!(
            panic_message(
                "called `Result::unwrap()` on an `Err` value: Custom(\"C:/Users/x/notes.txt\")",
                false
            ),
            "called `Result::unwrap()` on an `Err` value <hidden 32 chars>"
        );
        assert_eq!(
            panic_message("called `Option::unwrap()` on a `None` value", false),
            "called `Option::unwrap()` on a `None` value"
        );
        assert_eq!(
            panic_message(
                "index out of bounds: the len is 3 but the index is 5",
                false
            ),
            "index out of bounds <hidden 33 chars>"
        );
        assert_eq!(
            panic_message("said \"письмо Ане\" and stopped", false),
            "said  <hidden 24 chars>"
        );
        let long = "w".repeat(250);
        assert_eq!(
            panic_message(&long, false),
            format!("{} <hidden 50 chars>", "w".repeat(200))
        );
        // With texts on the message is whole, unless it reads like the
        // blind zone: then nothing of it is written in either mode.
        assert_eq!(panic_message(sliced, true), sliced);
        for content in [false, true] {
            let text = panic_message("no window Вход в банк", content);
            assert!(
                text.starts_with("<hidden: reads like the blind zone"),
                "{text}"
            );
        }
    }
}
