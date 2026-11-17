//! What she did in the last turn, kept in memory for a skill (decision 0047).
//!
//! The agent writes every successful call of a turn here. When the turn ends,
//! its calls become the last recording, and a turn of at least three actions
//! without an error offers once to save them as a skill. Nothing of it is
//! written to disk: the recording lives until the next turn that does
//! something, or until the app closes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;

use crate::tools::{RiskLevel, ToolRegistry};

/// Actions (calls that change something) a turn needs for the offer.
pub const OFFER_MIN_ACTIONS: usize = 3;
/// Calls one turn records at most; a skill holds fewer anyway.
const MAX_RECORDED: usize = 40;

/// Words of the names of tools that only read, look or find.
const READ_WORDS: &[&str] = &[
    "get",
    "list",
    "read",
    "search",
    "info",
    "status",
    "now_playing",
    "look",
    "screen_text",
    "health",
    "time",
    "events",
    "summary",
    "report",
    "fetch",
    "usage",
    "find",
    "jobs",
    "history",
];

/// Whether a call only reads: a low risk and a name that says so. A read is
/// kept in the recording, but it is no action for the offer.
pub fn is_read(tool: &str, level: RiskLevel) -> bool {
    level == RiskLevel::Low && READ_WORDS.iter().any(|w| tool.contains(w))
}

/// One successful call of a turn.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recorded {
    pub tool: String,
    pub args: Value,
    /// It only read something.
    pub read: bool,
}

/// The calls of the last turn that did something.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    /// Grows with every recording: an offer names the one it is about.
    pub id: u64,
    pub steps: Vec<Recorded>,
}

impl Recording {
    pub fn actions(&self) -> usize {
        self.steps.iter().filter(|s| !s.read).count()
    }
}

/// The offer to save the last recording as a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub id: u64,
    pub actions: usize,
}

#[derive(Default)]
struct Inner {
    registry: Weak<ToolRegistry>,
    turn: Vec<Recorded>,
    failed: bool,
    last: Option<Recording>,
    next_id: u64,
}

/// Shared by the agent, which writes, and the skill tools, which read.
#[derive(Clone)]
pub struct SkillBook {
    inner: Arc<Mutex<Inner>>,
    offers: Arc<AtomicBool>,
}

impl Default for SkillBook {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            offers: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl SkillBook {
    /// Whether a turn may end with the offer: `skills.suggest`.
    pub fn set_offers(&self, on: bool) {
        self.offers.store(on, Ordering::Relaxed);
    }

    pub fn offers(&self) -> bool {
        self.offers.load(Ordering::Relaxed)
    }

    /// The tools the agent calls now: `skill_save` checks steps against them.
    pub fn set_registry(&self, registry: &Arc<ToolRegistry>) {
        self.inner.lock().registry = Arc::downgrade(registry);
    }

    pub fn registry(&self) -> Option<Arc<ToolRegistry>> {
        self.inner.lock().registry.upgrade()
    }

    /// A turn begins: what the turn before recorded stays the last recording.
    pub fn begin_turn(&self) {
        let mut inner = self.inner.lock();
        inner.turn.clear();
        inner.failed = false;
    }

    /// A call of this turn succeeded. The skill and plan tools are not kept.
    pub fn record(&self, tool: &str, args: &Value, read: bool) {
        if super::is_service(tool) {
            return;
        }
        let mut inner = self.inner.lock();
        if inner.turn.len() < MAX_RECORDED {
            inner.turn.push(Recorded {
                tool: tool.to_string(),
                args: args.clone(),
                read,
            });
        }
    }

    /// A call of this turn was refused or failed: no offer for this turn.
    pub fn note_failure(&self) {
        self.inner.lock().failed = true;
    }

    /// The turn ended, `ok` when its reply came. A turn that recorded
    /// something becomes the last recording; one that recorded nothing
    /// leaves the one before it, so «сохрани это как навык» finds it.
    /// The offer comes once, for a recording of enough actions made by a
    /// turn without an error, and only while offers are on.
    pub fn end_turn(&self, ok: bool) -> Option<Offer> {
        let mut inner = self.inner.lock();
        if inner.turn.is_empty() {
            return None;
        }
        inner.next_id += 1;
        let recording = Recording {
            id: inner.next_id,
            steps: std::mem::take(&mut inner.turn),
        };
        let actions = recording.actions();
        let offer = (ok
            && !inner.failed
            && actions >= OFFER_MIN_ACTIONS
            && recording.steps.len() <= super::MAX_SKILL_STEPS
            && self.offers())
        .then_some(Offer {
            id: recording.id,
            actions,
        });
        inner.last = Some(recording);
        offer
    }

    pub fn last(&self) -> Option<Recording> {
        self.inner.lock().last.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn(book: &SkillBook, calls: &[(&str, bool)]) {
        book.begin_turn();
        for (tool, read) in calls {
            book.record(tool, &json!({"x": tool}), *read);
        }
    }

    #[test]
    fn reads_are_low_risk_calls_whose_names_read() {
        assert!(is_read("get_weather", RiskLevel::Low));
        assert!(is_read("list_windows", RiskLevel::Low));
        assert!(is_read("ui_read", RiskLevel::Low));
        assert!(!is_read("open_app", RiskLevel::Low));
        assert!(!is_read("volume", RiskLevel::Low));
        // A call that changes something has a higher risk, whatever its name.
        assert!(!is_read("read_file", RiskLevel::Medium));
    }

    #[test]
    fn three_actions_without_an_error_are_offered_once() {
        let book = SkillBook::default();
        turn(
            &book,
            &[
                ("open_app", false),
                ("get_weather", true),
                ("volume", false),
                ("open_url", false),
            ],
        );
        let offer = book.end_turn(true).unwrap();
        assert_eq!(offer.actions, 3);
        let last = book.last().unwrap();
        assert_eq!(last.id, offer.id);
        assert_eq!(last.steps.len(), 4);
        assert!(last.steps[1].read && !last.steps[0].read);
        // The next turn that records nothing keeps the recording and offers nothing.
        book.begin_turn();
        assert_eq!(book.end_turn(true), None);
        assert_eq!(book.last().unwrap().id, offer.id);
    }

    #[test]
    fn fewer_actions_an_error_or_offers_off_mean_no_offer() {
        let book = SkillBook::default();
        turn(
            &book,
            &[
                ("open_app", false),
                ("volume", false),
                ("get_time", true),
                ("get_news", true),
            ],
        );
        assert_eq!(book.end_turn(true), None);
        assert_eq!(book.last().unwrap().steps.len(), 4);

        turn(
            &book,
            &[("open_app", false), ("volume", false), ("open_url", false)],
        );
        book.note_failure();
        assert_eq!(book.end_turn(true), None);

        turn(
            &book,
            &[("open_app", false), ("volume", false), ("open_url", false)],
        );
        assert_eq!(book.end_turn(false), None);

        book.set_offers(false);
        turn(
            &book,
            &[("open_app", false), ("volume", false), ("open_url", false)],
        );
        assert_eq!(book.end_turn(true), None);
        // The recording is kept all the same: she can be asked by words.
        assert_eq!(book.last().unwrap().steps.len(), 3);
    }

    #[test]
    fn the_skill_tools_and_the_plan_are_never_recorded() {
        let book = SkillBook::default();
        turn(
            &book,
            &[
                ("task_plan", false),
                ("skill_run", false),
                ("skill_save", false),
                ("command_save", false),
            ],
        );
        assert_eq!(book.end_turn(true), None);
        assert_eq!(book.last(), None);
    }

    #[test]
    fn a_new_turn_that_did_something_replaces_the_recording() {
        let book = SkillBook::default();
        turn(&book, &[("open_app", false)]);
        book.end_turn(true);
        turn(&book, &[("volume", false)]);
        book.end_turn(true);
        let last = book.last().unwrap();
        assert_eq!(last.id, 2);
        assert_eq!(last.steps[0].tool, "volume");
    }
}
