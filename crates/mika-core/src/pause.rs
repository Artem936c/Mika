//! Full stop and the pause after it (decision 0045).
//!
//! A stop word, the Stop button of the chat: she drops everything she is
//! doing and starts nothing on her own until the user addresses her. What
//! she would have started meanwhile is written down and told once, when
//! the pause ends.

use parking_lot::Mutex;

use crate::i18n::{lang, tr_args_in, tr_in, Lang};
use crate::state::AssistantState;

/// `Event::Remark` kind: the line saying what a full stop ended.
pub const REMARK_STOP: &str = "stop";
/// `Event::Remark` kind: the line saying what the pause skipped.
pub const REMARK_PAUSE: &str = "pause";
/// `Event::Remark` kind: the line she says when she falls asleep (decision 0050).
pub const REMARK_SLEEP: &str = "sleep";

/// Words that stop her (decisions 0045 and 0048). One list for the wake
/// phrase listener, the voice dialog and Telegram.
pub const STOP_WORDS: &[&str] = &[
    "стоп",
    "стой",
    "хватит",
    "замолчи",
    "помолчи",
    "тихо",
    "достаточно",
    "остановись",
    "останови",
    "перестань",
    "отмена",
    "отбой",
];

/// Words a stop phrase may hold besides the stop words: «перестань
/// слушать», «стоп, пожалуйста».
pub const STOP_TAIL: &[&str] = &[
    "слушать",
    "слушай",
    "говорить",
    "пожалуйста",
    "уже",
    "всё",
    "все",
    "не",
];

/// Words that put her to sleep (decision 0050): she goes quiet and starts
/// nothing by herself, but a running task and the background jobs go on.
/// They act only as a whole phrase, see `only_sleep_words`.
pub const SLEEP_WORDS: &[&str] = &["усни", "спи", "засыпай", "отдохни", "поспи", "спать"];

/// Words a sleep phrase may hold besides the sleep words: «иди спать»,
/// «усни, пожалуйста».
pub const SLEEP_TAIL: &[&str] = &["иди", "давай", "пожалуйста", "немного", "уже", "можешь"];

/// Words of a recognised phrase, lower case, without punctuation and
/// without the `[unk]` of Vosk.
pub fn phrase_words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && *w != "unk")
        .map(str::to_lowercase)
}

/// True when the phrase is a stop and nothing else: stop words, maybe with
/// a word of `STOP_TAIL`. «Не слушай» alone is not a stop.
pub fn only_stop_words(text: &str) -> bool {
    let mut any = false;
    for w in phrase_words(text) {
        if STOP_WORDS.contains(&w.as_str()) {
            any = true;
        } else if !STOP_TAIL.contains(&w.as_str()) {
            return false;
        }
    }
    any
}

/// True when the phrase asks her to sleep and nothing else: sleep words,
/// maybe with a word of `SLEEP_TAIL`. «Спи спокойно» and «список» are not.
pub fn only_sleep_words(text: &str) -> bool {
    let mut any = false;
    for w in phrase_words(text) {
        if SLEEP_WORDS.contains(&w.as_str()) {
            any = true;
        } else if !SLEEP_TAIL.contains(&w.as_str()) {
            return false;
        }
    }
    any
}

/// The first stop word of the phrase, as a whole word.
pub fn stop_word(text: &str) -> Option<&'static str> {
    phrase_words(text).find_map(|w| STOP_WORDS.iter().find(|s| **s == w).copied())
}

/// While a phrase is still being heard: it may yet turn out to be a stop
/// and nothing else. Its last word may be cut, «перестань слу».
pub fn may_be_stop_phrase(text: &str) -> bool {
    let words: Vec<String> = phrase_words(text).collect();
    let Some((last, before)) = words.split_last() else {
        return false;
    };
    let whole = |w: &String| STOP_WORDS.contains(&w.as_str()) || STOP_TAIL.contains(&w.as_str());
    let started = |w: &String| {
        STOP_WORDS
            .iter()
            .chain(STOP_TAIL)
            .any(|s| s.starts_with(w.as_str()))
    };
    before.iter().all(whole)
        && (whole(last) || started(last))
        && words.iter().any(|w| STOP_WORDS.contains(&w.as_str()))
}

/// What she is busy with when a stop word is heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    pub state: AssistantState,
    /// Her voice sounds, a reply still being said included.
    pub voice: bool,
    /// She records a phrase of the user: a question or her follow-up listen.
    pub listening: bool,
    /// A dictation into another program records or types.
    pub dictation: bool,
    /// A background job waits or works.
    pub jobs: bool,
}

/// Whether a heard stop word stops her. `only_stop_words`: the phrase held
/// nothing but stop words. `echo`: the word is in the reply she is saying.
///
/// - During a dictation it never acts: the words are the user's text.
/// - While she records a phrase of the user, only a phrase of stop words
///   acts: «сколько хватит бензина» is a question, not a stop.
/// - While she speaks, a word of her own reply is her voice in the speakers.
/// - Otherwise it acts whenever she is busy; idle with nothing running it
///   does nothing: the words are common in talk around the computer.
pub fn stop_word_acts(busy: Busy, only_stop_words: bool, echo: bool) -> bool {
    if busy.dictation {
        return false;
    }
    if busy.listening {
        return only_stop_words;
    }
    let speaking = busy.state == AssistantState::Speaking || busy.voice;
    if speaking && echo {
        return false;
    }
    busy.state != AssistantState::Idle || busy.voice || busy.jobs
}

/// What a full stop ended, for the one line in the chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StopReport {
    /// A reply was being made or waited in the queue.
    pub reply: bool,
    /// A multi-step task ran.
    pub task: bool,
    /// A question about an action was open: it is denied.
    pub confirm: bool,
    /// Her voice sounded.
    pub speech: bool,
    /// She recorded a phrase of the user.
    pub listening: bool,
    /// A screen recording ran: it is saved.
    pub recording: bool,
    /// Other background jobs that were stopped.
    pub jobs: usize,
}

impl StopReport {
    /// Nothing was running.
    pub fn is_empty(&self) -> bool {
        *self == StopReport::default()
    }

    /// The names of what was stopped, for the journal: `reply,speech`.
    pub fn names(&self) -> String {
        let mut names: Vec<String> = self.parts().iter().map(|p| p.0.to_string()).collect();
        if self.jobs > 0 {
            names.push(format!("jobs={}", self.jobs));
        }
        if names.is_empty() {
            return "nothing".into();
        }
        names.join(",")
    }

    /// What was stopped, but the jobs: the journal name and the locale key.
    /// A task is named in place of its reply.
    fn parts(&self) -> Vec<(&'static str, &'static str)> {
        let mut parts = Vec::new();
        if self.task {
            parts.push(("task", "core.stop.task"));
        } else if self.reply {
            parts.push(("reply", "core.stop.reply"));
        }
        for (on, name, key) in [
            (self.confirm, "confirm", "core.stop.confirm"),
            (self.speech, "speech", "core.stop.speech"),
            (self.listening, "listening", "core.stop.listening"),
            (self.recording, "recording", "core.stop.recording"),
        ] {
            if on {
                parts.push((name, key));
            }
        }
        parts
    }

    /// The line in the chat, in `lang`. `paused`: she waits to be called now.
    pub fn line_in(&self, lang: Lang, paused: bool) -> String {
        let mut what: Vec<String> = self.parts().iter().map(|p| tr_in(lang, p.1)).collect();
        match self.jobs {
            0 => {}
            1 => what.push(tr_in(lang, "core.stop.job")),
            n => what.push(tr_args_in(lang, "core.stop.jobs", &[("n", &n.to_string())])),
        }
        if what.is_empty() {
            let key = if paused {
                "core.stop_nothing_paused"
            } else {
                "core.stop_nothing"
            };
            return tr_in(lang, key);
        }
        let key = if paused {
            "core.stop_line_paused"
        } else {
            "core.stop_line"
        };
        tr_args_in(lang, key, &[("what", &what.join(", "))])
    }

    pub fn line(&self, paused: bool) -> String {
        self.line_in(lang(), paused)
    }
}

/// What she would have started by herself during a pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipKind {
    /// A scenario by event; the detail is its name.
    Scenario,
    /// A scheduled task; the detail is its text.
    Task,
    MorningSummary,
    EveningSummary,
    /// A reply in a chat she keeps up; the detail is who wrote.
    ChatReply,
    /// A reminder to take a break.
    Break,
    /// A greeting or «уже поздно»: the «Живость» switch.
    Lively,
    /// A line of the companion in games (decision 0054).
    Companion,
}

impl SkipKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipKind::Scenario => "scenario",
            SkipKind::Task => "task",
            SkipKind::MorningSummary => "morning_summary",
            SkipKind::EveningSummary => "evening_summary",
            SkipKind::ChatReply => "chat_reply",
            SkipKind::Break => "break",
            SkipKind::Lively => "lively",
            SkipKind::Companion => "companion",
        }
    }

    /// The journal category of the work it stands for.
    fn category(self) -> &'static str {
        match self {
            SkipKind::Scenario | SkipKind::ChatReply => "events",
            SkipKind::Task | SkipKind::MorningSummary | SkipKind::EveningSummary => "jobs",
            SkipKind::Break | SkipKind::Lively => "app",
            SkipKind::Companion => "vision",
        }
    }

    fn key(self) -> &'static str {
        match self {
            SkipKind::Scenario => "core.skip.scenario",
            SkipKind::Task => "core.skip.task",
            SkipKind::MorningSummary => "core.skip.morning",
            SkipKind::EveningSummary => "core.skip.evening",
            SkipKind::ChatReply => "core.skip.chat_reply",
            SkipKind::Break => "core.skip.break",
            SkipKind::Lively => "core.skip.lively",
            SkipKind::Companion => "core.skip.companion",
        }
    }
}

/// One thing the pause held back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub kind: SkipKind,
    pub detail: String,
}

/// A line of her own of `kind` (`Event::Remark`) during a pause: `Some` —
/// it is not shown and goes into the list of what was skipped; `None` — it
/// is shown in the chat but not said aloud.
pub fn held_in_pause(kind: &str) -> Option<SkipKind> {
    match kind {
        "life" => Some(SkipKind::Lively),
        "break" => Some(SkipKind::Break),
        // The summary of the day comes as a line of hers.
        "day" => Some(SkipKind::EveningSummary),
        // A line about the game is of no use later (decision 0054).
        crate::companion::REMARK_COMPANION => Some(SkipKind::Companion),
        _ => None,
    }
}

/// Lines in the list of what was skipped; the rest is counted.
const SHOWN_SKIPPED: usize = 6;

/// The line in `lang` that tells what the pause skipped; `None` when nothing.
pub fn skipped_line_in(lang: Lang, items: &[Skipped]) -> Option<String> {
    skipped_line_of(lang, PauseKind::Stopped, items)
}

/// The line in `lang` that tells what the pause of `kind` skipped: «Пока
/// была пауза…» or «Пока я спала…»; `None` when nothing.
pub fn skipped_line_of(lang: Lang, kind: PauseKind, items: &[Skipped]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut what: Vec<String> = items
        .iter()
        .take(SHOWN_SKIPPED)
        .map(|i| tr_args_in(lang, i.kind.key(), &[("what", i.detail.trim())]))
        .collect();
    if items.len() > SHOWN_SKIPPED {
        let rest = (items.len() - SHOWN_SKIPPED).to_string();
        what.push(tr_args_in(lang, "core.skip.more", &[("n", &rest)]));
    }
    let key = match kind {
        PauseKind::Stopped => "core.pause_skipped",
        PauseKind::Sleeping => "core.sleep_skipped",
    };
    Some(tr_args_in(lang, key, &[("what", &what.join(", "))]))
}

pub fn skipped_line(items: &[Skipped]) -> Option<String> {
    skipped_line_in(lang(), items)
}

/// Why she waits to be called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseKind {
    /// After a full stop (decision 0045).
    Stopped,
    /// She was asked to sleep (decision 0050): a running task and the
    /// background jobs go on, nothing else starts and nothing sounds.
    Sleeping,
}

impl PauseKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PauseKind::Stopped => "stopped",
            PauseKind::Sleeping => "sleeping",
        }
    }
}

/// What the pause holds while it lasts.
struct Held {
    kind: PauseKind,
    /// What was skipped so far, without repeats.
    skipped: Vec<Skipped>,
}

/// The pause after a full stop, or her sleep. It has no time limit: it lasts
/// until the user addresses her.
#[derive(Default)]
pub struct Pause {
    /// `Some` while paused.
    held: Mutex<Option<Held>>,
}

impl Pause {
    /// Starts the pause after a full stop. False when a pause or a sleep
    /// was on already: a sleep stays a sleep.
    pub fn begin(&self) -> bool {
        let mut held = self.held.lock();
        if held.is_some() {
            return false;
        }
        *held = Some(Held {
            kind: PauseKind::Stopped,
            skipped: Vec::new(),
        });
        log::info!(target: "turn", "pause began: she starts nothing until she is called");
        true
    }

    /// Puts her to sleep. A pause that was on becomes a sleep and keeps
    /// what it skipped. False when she slept already.
    pub fn begin_sleep(&self) -> bool {
        let mut held = self.held.lock();
        match held.as_mut() {
            Some(h) if h.kind == PauseKind::Sleeping => return false,
            Some(h) => h.kind = PauseKind::Sleeping,
            None => {
                *held = Some(Held {
                    kind: PauseKind::Sleeping,
                    skipped: Vec::new(),
                })
            }
        }
        log::info!(target: "turn", "sleep began: she starts nothing and says nothing until she is called");
        true
    }

    pub fn is_on(&self) -> bool {
        self.held.lock().is_some()
    }

    /// True while she sleeps.
    pub fn is_sleeping(&self) -> bool {
        self.kind() == Some(PauseKind::Sleeping)
    }

    /// Why she waits, `None` when she does not.
    pub fn kind(&self) -> Option<PauseKind> {
        self.held.lock().as_ref().map(|h| h.kind)
    }

    /// During a pause: writes down `kind` and returns true, and the caller
    /// starts nothing. Outside a pause: false, nothing is written.
    pub fn skip(&self, kind: SkipKind, detail: &str) -> bool {
        let mut guard = self.held.lock();
        let Some(held) = guard.as_mut() else {
            return false;
        };
        let item = Skipped {
            kind,
            detail: detail.trim().to_string(),
        };
        if !held.skipped.contains(&item) {
            held.skipped.push(item);
        }
        drop(guard);
        // The detail is the user's: a name, a task, who wrote.
        log::info!(
            target: kind.category(),
            "skipped during the pause: kind={} detail={}",
            kind.as_str(),
            crate::journal::content(detail)
        );
        true
    }

    /// Ends the pause: what it skipped, or `None` when there was no pause.
    pub fn end(&self) -> Option<Vec<Skipped>> {
        self.end_kind().map(|(_, skipped)| skipped)
    }

    /// Ends the pause: why she waited and what it skipped, or `None` when
    /// there was no pause.
    pub fn end_kind(&self) -> Option<(PauseKind, Vec<Skipped>)> {
        self.held.lock().take().map(|h| (h.kind, h.skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AssistantState::*;

    #[test]
    fn a_stop_phrase_is_stop_words_and_maybe_a_tail() {
        for stop in [
            "стоп",
            "Стой.",
            "Хватит, хватит!",
            "[unk] остановись",
            "Останови",
            "Перестань слушать",
            "Стоп, пожалуйста",
            "Всё, хватит",
            "отбой",
        ] {
            assert!(only_stop_words(stop), "{stop}");
        }
        for other in [
            "",
            "[unk]",
            "стопка книг",
            "сколько хватит бензина",
            "останови машину у магазина",
            "не слушай",
            "тихий вечер",
        ] {
            assert!(!only_stop_words(other), "{other}");
        }
        assert_eq!(stop_word("Стоп."), Some("стоп"));
        assert_eq!(stop_word("а теперь тихо"), Some("тихо"));
        assert_eq!(stop_word("стопка"), None);
    }

    #[test]
    fn a_phrase_being_heard_may_still_become_a_stop() {
        assert!(may_be_stop_phrase("перестань слу"));
        assert!(may_be_stop_phrase("стоп"));
        assert!(may_be_stop_phrase("стоп пожа"));
        assert!(!may_be_stop_phrase("останови машину"));
        assert!(!may_be_stop_phrase("слу"));
        assert!(!may_be_stop_phrase(""));
    }

    fn busy(state: AssistantState) -> Busy {
        Busy {
            state,
            voice: false,
            listening: false,
            dictation: false,
            jobs: false,
        }
    }

    #[test]
    fn a_stop_word_acts_in_every_busy_state() {
        for state in [Thinking, Speaking, AwaitingConfirm, Listening] {
            assert!(stop_word_acts(busy(state), false, false), "{state:?}");
        }
        // Idle, but her voice still sounds or a job runs.
        let voice = Busy {
            voice: true,
            ..busy(Idle)
        };
        assert!(stop_word_acts(voice, false, false));
        let jobs = Busy {
            jobs: true,
            ..busy(Idle)
        };
        assert!(stop_word_acts(jobs, false, false));
    }

    #[test]
    fn idle_with_nothing_running_a_stop_word_does_nothing() {
        assert!(!stop_word_acts(busy(Idle), true, false));
    }

    #[test]
    fn her_own_word_in_the_speakers_is_an_echo_only_while_she_speaks() {
        assert!(!stop_word_acts(busy(Speaking), true, true));
        let voice = Busy {
            voice: true,
            ..busy(Idle)
        };
        assert!(!stop_word_acts(voice, true, true));
        // Not speaking: her last reply is not in the speakers any more.
        assert!(stop_word_acts(busy(Thinking), false, true));
    }

    #[test]
    fn while_she_listens_only_a_phrase_of_stop_words_acts() {
        let listening = Busy {
            listening: true,
            ..busy(Listening)
        };
        assert!(stop_word_acts(listening, true, false));
        assert!(!stop_word_acts(listening, false, false));
    }

    #[test]
    fn a_dictation_is_never_stopped_by_its_own_words() {
        let dictation = Busy {
            dictation: true,
            listening: true,
            ..busy(Listening)
        };
        assert!(!stop_word_acts(dictation, true, false));
    }

    #[test]
    fn the_stop_line_names_what_was_stopped() {
        let report = StopReport {
            reply: true,
            speech: true,
            recording: true,
            ..Default::default()
        };
        assert_eq!(
            report.line_in(Lang::Ru, false),
            "Остановилась: ответ, речь, запись экрана."
        );
        assert_eq!(
            report.line_in(Lang::En, false),
            "Stopped: the reply, speech, the screen recording."
        );
        assert!(report
            .line_in(Lang::Ru, true)
            .starts_with("Остановилась: ответ, речь, запись экрана. "));
        assert_eq!(report.names(), "reply,speech,recording");
    }

    #[test]
    fn a_task_is_named_instead_of_its_reply_and_jobs_are_counted() {
        let report = StopReport {
            reply: true,
            task: true,
            confirm: true,
            listening: true,
            jobs: 2,
            ..Default::default()
        };
        assert_eq!(
            report.line_in(Lang::Ru, false),
            "Остановилась: задача, вопрос о действии, запись с микрофона, фоновые задания (2)."
        );
        let one = StopReport {
            jobs: 1,
            ..Default::default()
        };
        assert_eq!(
            one.line_in(Lang::Ru, false),
            "Остановилась: фоновое задание."
        );
        assert_eq!(report.names(), "task,confirm,listening,jobs=2");
    }

    #[test]
    fn nothing_stopped_still_says_that_she_stopped() {
        let none = StopReport::default();
        assert!(none.is_empty());
        assert_eq!(none.line_in(Lang::Ru, false), "Остановилась.");
        assert!(none.line_in(Lang::En, true).starts_with("Stopped. "));
        assert_eq!(none.names(), "nothing");
    }

    #[test]
    fn the_pause_collects_what_it_skipped_without_repeats() {
        let pause = Pause::default();
        assert!(!pause.is_on());
        assert!(!pause.skip(SkipKind::Task, "полить цветы"));
        assert_eq!(pause.end(), None);

        assert!(pause.begin());
        assert!(!pause.begin());
        assert!(pause.is_on());
        assert!(pause.skip(SkipKind::Task, "полить цветы"));
        assert!(pause.skip(SkipKind::Task, "полить цветы"));
        assert!(pause.skip(SkipKind::Break, ""));
        assert_eq!(
            pause.end(),
            Some(vec![
                Skipped {
                    kind: SkipKind::Task,
                    detail: "полить цветы".into()
                },
                Skipped {
                    kind: SkipKind::Break,
                    detail: String::new()
                },
            ])
        );
        assert!(!pause.is_on());
        assert_eq!(pause.end(), None);
    }

    #[test]
    fn the_skipped_line_lists_each_item_and_counts_the_rest() {
        assert_eq!(skipped_line_in(Lang::Ru, &[]), None);
        let items = [
            Skipped {
                kind: SkipKind::Scenario,
                detail: "Наушники".into(),
            },
            Skipped {
                kind: SkipKind::MorningSummary,
                detail: String::new(),
            },
        ];
        assert_eq!(
            skipped_line_in(Lang::Ru, &items).unwrap(),
            "Пока была пауза, я пропустила: сценарий «Наушники», утреннюю сводку."
        );
        assert_eq!(
            skipped_line_in(Lang::En, &items).unwrap(),
            "While I was paused I skipped: the scenario “Наушники”, the morning summary."
        );
        let many: Vec<Skipped> = (0..9)
            .map(|i| Skipped {
                kind: SkipKind::Task,
                detail: format!("t{i}"),
            })
            .collect();
        let line = skipped_line_in(Lang::Ru, &many).unwrap();
        assert!(line.contains("«t5»"), "{line}");
        assert!(!line.contains("«t6»"), "{line}");
        assert!(line.ends_with("и ещё 3."), "{line}");
    }

    #[test]
    fn a_sleep_phrase_is_sleep_words_and_maybe_a_tail() {
        for sleep in [
            "усни",
            "Спи.",
            "Засыпай!",
            "отдохни",
            "иди спать",
            "Усни, пожалуйста",
            "[unk] поспи",
        ] {
            assert!(only_sleep_words(sleep), "{sleep}");
        }
        for other in [
            "",
            "[unk]",
            "список",
            "спи спокойно",
            "хочу спать",
            "сколько спать коту",
            "иди",
            "стоп",
        ] {
            assert!(!only_sleep_words(other), "{other}");
        }
        // A sleep phrase is no stop, and a stop is no sleep.
        assert!(!only_stop_words("усни"));
    }

    #[test]
    fn a_sleep_keeps_what_the_pause_skipped_and_is_told_apart() {
        let pause = Pause::default();
        assert_eq!(pause.kind(), None);
        assert!(pause.begin());
        assert!(pause.skip(SkipKind::Break, ""));
        assert!(!pause.is_sleeping());
        // A pause becomes a sleep; a second sleep changes nothing.
        assert!(pause.begin_sleep());
        assert!(!pause.begin_sleep());
        // A full stop while she sleeps leaves her asleep.
        assert!(!pause.begin());
        assert!(pause.is_sleeping());
        assert_eq!(pause.kind(), Some(PauseKind::Sleeping));
        let (kind, skipped) = pause.end_kind().unwrap();
        assert_eq!(kind, PauseKind::Sleeping);
        assert_eq!(skipped.len(), 1);
        assert!(!pause.is_on());
        let line = skipped_line_of(Lang::Ru, kind, &skipped).unwrap();
        assert!(line.starts_with("Пока я спала"), "{line}");
        let en = skipped_line_of(Lang::En, kind, &skipped).unwrap();
        assert!(en.starts_with("While I was asleep"), "{en}");
    }

    #[test]
    fn lively_lines_are_skipped_and_the_rest_only_go_quiet() {
        assert_eq!(held_in_pause("life"), Some(SkipKind::Lively));
        assert_eq!(held_in_pause("break"), Some(SkipKind::Break));
        assert_eq!(held_in_pause("day"), Some(SkipKind::EveningSummary));
        assert_eq!(held_in_pause("companion"), Some(SkipKind::Companion));
        for shown in ["calendar", "focus", "watch", "job", "cost", "trigger"] {
            assert_eq!(held_in_pause(shown), None, "{shown}");
        }
    }
}
