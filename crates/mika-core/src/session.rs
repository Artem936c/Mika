//! Entry point for user text: answers an open question or starts a reply.

use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::commands::{expand, CommandStore};
use crate::confirm::{
    parse_answer, voice_answer, ConfirmError, Confirmations, DecidedBy, VoiceAnswer,
};
use crate::events::{Event, EventBus};
use crate::pause::{skipped_line, Pause, SkipKind, StopReport, REMARK_PAUSE, REMARK_STOP};
use crate::skills::SkillStore;

/// Said in the chat when something else is typed while a question about an action is open.
pub fn answer_hint() -> String {
    crate::i18n::tr("core.answer_hint")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submitted {
    Empty,
    Answered,
    Hinted,
    Started,
    /// A question is open that was never shown where the message came from:
    /// nothing was answered and nothing was published. The caller shows the
    /// question there (`Confirmations::pending`) and then calls
    /// `Session::question_shown`; the next message from there answers it.
    Unseen,
}

/// Where the last user message came from. Replies go back the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Local,
    Telegram,
    /// Said into the microphone.
    Voice,
}

/// How the user addressed her, as the journal names it.
fn origin_name(origin: Origin) -> &'static str {
    match origin {
        Origin::Local => "window",
        Origin::Telegram => "telegram",
        Origin::Voice => "voice",
    }
}

/// What started a turn, as the journal names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// Typed in the window, or sent by an action of the user at this computer.
    Window,
    Telegram,
    Voice,
    /// A scenario by event.
    Scenario,
    /// A scheduled task, a reminder with a task, a summary of the day.
    Task,
}

impl Source {
    fn of(origin: Origin) -> Self {
        match origin {
            Origin::Local => Source::Window,
            Origin::Telegram => Source::Telegram,
            Origin::Voice => Source::Voice,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Source::Window => "window",
            Source::Telegram => "telegram",
            Source::Voice => "voice",
            Source::Scenario => "scenario",
            Source::Task => "task",
        }
    }
}

/// The first record of a turn. The text is the user's: its size is always
/// written, its beginning only while texts are allowed, and never when it
/// reads like the blind zone.
fn started_line(
    number: u64,
    source: &str,
    text: &str,
    expanded: bool,
    waited_ms: u128,
    content: bool,
) -> String {
    let mut line = format!(
        "turn started: turn={number} source={source} chars={} expanded={expanded} waited_ms={waited_ms}",
        text.chars().count()
    );
    if content {
        line.push_str(" text=");
        line.push_str(&crate::journal::guarded_if(true, text));
    }
    line
}

pub struct Session {
    pub agent: Arc<Agent>,
    pub confirms: Arc<Confirmations>,
    bus: EventBus,
    cancel: Mutex<CancellationToken>,
    commands: Mutex<Option<CommandStore>>,
    skills: Mutex<Option<SkillStore>>,
    origin: Mutex<Origin>,
    /// One turn at a time, in the order they were asked for. A turn takes
    /// the origin when it starts, not when it is queued (decision 0031).
    turns: tokio::sync::Mutex<()>,
    /// The pause after a full stop (decision 0045).
    pause: Pause,
}

impl Session {
    pub fn new(agent: Arc<Agent>, confirms: Arc<Confirmations>, bus: EventBus) -> Arc<Self> {
        Arc::new(Self {
            agent,
            confirms,
            bus,
            cancel: Mutex::new(CancellationToken::new()),
            commands: Mutex::new(None),
            skills: Mutex::new(None),
            origin: Mutex::new(Origin::Local),
            turns: tokio::sync::Mutex::new(()),
            pause: Pause::default(),
        })
    }

    /// The pause after a full stop: what she would start by herself asks it first.
    pub fn pause(&self) -> &Pause {
        &self.pause
    }

    /// Stops everything (decision 0045): the running reply and the queued
    /// ones, a task with its tool call, an open question (denied), every
    /// background job of `jobs` (a recording saves its file). `report`
    /// comes with what the caller stopped itself: her voice, a recording of
    /// the microphone. With `pause` the pause begins. One line in the chat
    /// says what was stopped; without the pause it comes only when something was.
    pub fn full_stop(
        &self,
        mut report: StopReport,
        jobs: Option<&crate::bgjobs::JobBoard>,
        pause: bool,
    ) -> StopReport {
        report.reply = self.agent.turn_running() || self.turns.try_lock().is_err();
        report.task = self.agent.task_active();
        report.confirm = self.confirms.pending().is_some();
        self.stop();
        for job in jobs.map(|j| j.stop_all()).unwrap_or_default() {
            if job.live {
                report.recording = true;
            } else {
                report.jobs += 1;
            }
        }
        let began = pause && self.pause.begin();
        log::info!(
            "full stop: stopped={} pause={}",
            report.names(),
            if began {
                "began"
            } else if pause {
                "on"
            } else {
                "off"
            }
        );
        if began {
            self.bus.publish(Event::PauseChanged { paused: true });
        }
        if pause || !report.is_empty() {
            self.bus.publish(Event::Remark {
                text: report.line(pause),
                kind: REMARK_STOP.into(),
            });
        }
        report
    }

    /// The user addressed her: the pause ends, and one line tells what it
    /// skipped. `by` names how, for the journal. Without a pause nothing happens.
    pub fn addressed(&self, by: &str) {
        let Some(skipped) = self.pause.end() else {
            return;
        };
        log::info!("pause ended: by={by} skipped={}", skipped.len());
        self.bus.publish(Event::PauseChanged { paused: false });
        if let Some(text) = skipped_line(&skipped) {
            self.bus.publish(Event::Remark {
                text,
                kind: REMARK_PAUSE.into(),
            });
        }
    }

    /// Turns on user commands: a message equal to a command name runs the command.
    pub fn set_commands(&self, store: CommandStore) {
        *self.commands.lock() = Some(store);
    }

    /// Turns on skills by name: a message equal to a skill name asks her to
    /// run it (decision 0047).
    pub fn set_skills(&self, store: SkillStore) {
        *self.skills.lock() = Some(store);
    }

    /// What the model gets for a message: the instruction of the command or
    /// of the skill of that name, or the message itself. A command and a
    /// skill never share a name; one restored from a copy may, and then the
    /// command wins.
    fn expanded(&self, text: &str) -> String {
        if let Some(c) = self.commands.lock().as_ref().and_then(|c| c.find(text)) {
            return expand(&c);
        }
        if let Some(skill) = self.skills.lock().as_ref().and_then(|s| s.find(text)) {
            return crate::skills::expand(&skill);
        }
        text.to_string()
    }

    /// Starts a turn from this computer whose chat line is `shown` while the
    /// model gets `text`: a button of the chat, such as «Сохранить как
    /// навык». It never answers an open question: then it only reminds of it.
    /// Must run inside a tokio runtime.
    pub fn submit_shown(self: &Arc<Self>, shown: &str, text: &str) -> Submitted {
        let shown = shown.trim().to_string();
        let text = text.trim().to_string();
        if shown.is_empty() || text.is_empty() {
            return Submitted::Empty;
        }
        self.addressed("button");
        if self.confirms.pending().is_some() {
            self.bus.publish(Event::AssistantText {
                text: answer_hint(),
                spoken: None,
            });
            return Submitted::Hinted;
        }
        let token = self.cancel.lock().clone();
        tokio::spawn(
            self.clone()
                .turn(Origin::Local, Source::Window, shown, text, token),
        );
        Submitted::Started
    }

    pub fn origin(&self) -> Origin {
        *self.origin.lock()
    }

    /// True when a question is open that `origin` was never shown. The
    /// screen of this computer shows every question. Telegram gets the card
    /// only of a turn that goes on there: the question of a scenario, of a
    /// scheduled task or of a turn started at the computer is not sent to the
    /// phone. Nobody answers a question he was not shown: a "yes" typed in
    /// Telegram must not approve an action whose card is on another screen.
    pub fn question_unseen(&self, origin: Origin) -> bool {
        origin == Origin::Telegram
            && self.origin() != Origin::Telegram
            && self.confirms.pending().is_some()
    }

    /// The open question `id` has just been shown where `origin` reads. The
    /// turn goes on there from now on, as after an answer from there, so the
    /// next message from there answers the question. A question that has
    /// closed meanwhile changes nothing.
    pub fn question_shown(&self, id: u64, origin: Origin) {
        // The origin is locked first: a turn that starts right now either
        // sets its own origin after this, or the question is not its own.
        let mut current = self.origin.lock();
        if self.confirms.pending().is_some_and(|p| p.id == id) {
            *current = origin;
        }
    }

    pub fn submit(self: &Arc<Self>, text: &str) -> Submitted {
        self.submit_from(text, Origin::Local)
    }

    /// Runs one turn. It waits for the turn before it and only then takes
    /// the origin: a scenario that fires while a reply to Telegram is still
    /// running does not cut that reply off from the phone, and a message that
    /// waits behind a scenario does not send the scenario's questions there.
    async fn turn(
        self: Arc<Self>,
        origin: Origin,
        source: Source,
        shown: String,
        text: String,
        token: CancellationToken,
    ) {
        let asked = Instant::now();
        let _own = self.turns.lock().await;
        *self.origin.lock() = origin;
        // How the turn ended is written by the agent, with the same number.
        let number = crate::journal::next_turn();
        let line = started_line(
            number,
            source.as_str(),
            &text,
            shown != text,
            asked.elapsed().as_millis(),
            crate::journal::content_allowed(),
        );
        log::info!("{line}");
        let work = self.agent.handle_expanded(&shown, &text, token);
        let _ = crate::journal::in_turn(number, work).await;
    }

    /// Starts a turn that answers no open question and stays at this computer.
    fn start_task(self: &Arc<Self>, shown: &str, text: &str, source: Source) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let shown = shown.trim().to_string();
        // Nothing of her own starts during the pause (decision 0045).
        let kind = match source {
            Source::Scenario => SkipKind::Scenario,
            _ => SkipKind::Task,
        };
        if self.pause.skip(kind, &shown) {
            return;
        }
        // The token of now: a "stop" that comes before this turn drops it.
        let token = self.cancel.lock().clone();
        tokio::spawn(self.clone().turn(Origin::Local, source, shown, text, token));
    }

    /// Like `submit_task`, but the chat shows `shown` while the model gets
    /// `text`: a scenario by event shows one short line and sends its whole
    /// instruction (decision 0031).
    pub fn submit_task_as(self: &Arc<Self>, shown: &str, text: &str) {
        self.start_task(shown, text, Source::Scenario);
    }

    /// Starts a scheduled task. It never answers an open question:
    /// it waits for the running reply and then runs as its own turn.
    pub fn submit_task(self: &Arc<Self>, text: &str) {
        self.start_task(text, text, Source::Task);
    }

    /// Must run inside a tokio runtime: a reply starts as a task.
    pub fn submit_from(self: &Arc<Self>, text: &str, origin: Origin) -> Submitted {
        let text = text.trim();
        if text.is_empty() {
            return Submitted::Empty;
        }
        self.addressed(origin_name(origin));
        let by = match origin {
            Origin::Local => DecidedBy::Chat,
            Origin::Telegram => DecidedBy::Telegram,
            Origin::Voice => DecidedBy::Voice,
        };
        if let Some(p) = self.confirms.pending() {
            // Only someone who was shown the question answers it.
            if self.question_unseen(origin) {
                log::info!(
                    "message is not an answer: question {} was never shown in Telegram",
                    p.id
                );
                return Submitted::Unseen;
            }
            // An answer belongs to the running turn: it goes on where the
            // answer came from.
            *self.origin.lock() = origin;
            self.bus.publish(Event::UserText { text: text.into() });
            let (answer, need_name) = match origin {
                Origin::Voice => match voice_answer(text, &p.assessment) {
                    VoiceAnswer::Answer(a) => (Some(a), None),
                    VoiceAnswer::NeedName(name) => (None, Some(name)),
                    VoiceAnswer::Unclear => (None, None),
                },
                _ => (parse_answer(text), None),
            };
            if let Some(name) = need_name {
                self.bus.publish(Event::AssistantText {
                    text: format!(
                        "Это действие высокого риска. Чтобы подтвердить голосом, скажи его название: «{name}». Или скажи «отмена»."
                    ),
                    spoken: None,
                });
                return Submitted::Hinted;
            }
            let hint = match answer {
                Some(answer) => match self.confirms.answer(p.id, answer, by) {
                    Ok(()) => return Submitted::Answered,
                    Err(ConfirmError::ForeverNotAllowed(why)) => {
                        let mut chars = why.chars();
                        let first = chars.next().map(|c| c.to_uppercase().to_string());
                        format!(
                            "{}{}. Скажи «да» на этот раз или «нет».",
                            first.unwrap_or_default(),
                            chars.as_str()
                        )
                    }
                    Err(ConfirmError::NotPending) => answer_hint(),
                },
                None => answer_hint(),
            };
            self.bus.publish(Event::AssistantText {
                text: hint,
                spoken: None,
            });
            return Submitted::Hinted;
        }
        let token = self.cancel.lock().clone();
        let shown = text.to_string();
        let full = self.expanded(text);
        tokio::spawn(
            self.clone()
                .turn(origin, Source::of(origin), shown, full, token),
        );
        Submitted::Started
    }

    /// Starts a turn with `text` and never answers an open question: `None`
    /// when one is open, and then nothing is published and the origin stays.
    /// For text that was not typed: a recognised voice message (decision 0029).
    /// Must run inside a tokio runtime: a reply starts as a task.
    pub fn submit_new_from(self: &Arc<Self>, text: &str, origin: Origin) -> Option<Submitted> {
        let text = text.trim();
        if text.is_empty() {
            return Some(Submitted::Empty);
        }
        self.addressed(origin_name(origin));
        if self.confirms.pending().is_some() {
            return None;
        }
        let token = self.cancel.lock().clone();
        let shown = text.to_string();
        let full = self.expanded(text);
        // Through `turn`, like every other turn: it waits for the turn before
        // it and takes the origin when it starts (decision 0031).
        tokio::spawn(
            self.clone()
                .turn(origin, Source::of(origin), shown, full, token),
        );
        Some(Submitted::Started)
    }

    /// A token the next `stop` cancels, wherever that stop comes from: the
    /// computer or Telegram. Whoever took it before doing slow work for a
    /// turn that has not started learns from it that the work is not wanted
    /// any more (decision 0029).
    pub fn stop_token(&self) -> CancellationToken {
        self.cancel.lock().clone()
    }

    /// Stops the running reply, drops queued ones and denies an open question.
    pub fn stop(&self) {
        let mut c = self.cancel.lock();
        c.cancel();
        *c = CancellationToken::new();
        drop(c);
        log::debug!("stop: the running reply and the queued ones are dropped");
        self.confirms.deny_pending(DecidedBy::Policy);
    }

    /// Starts a direct turn: the chat shows `shown`, then what `answer` gives
    /// (see `Agent::handle_direct`). For an action the user started on this
    /// computer: when the turn begins the origin becomes `Local`, so the reply
    /// stays here unless a message from Telegram arrives while it is made (the
    /// origin is one value for the session). While a question about an
    /// action is open nothing starts and `answer` is dropped without being run.
    /// Must run inside a tokio runtime, like `submit_from`.
    pub fn submit_direct<F>(self: &Arc<Self>, shown: &str, answer: F) -> Submitted
    where
        F: std::future::Future<Output = Result<String, String>> + Send + 'static,
    {
        let shown = shown.trim().to_string();
        if shown.is_empty() {
            return Submitted::Empty;
        }
        self.addressed("direct");
        if self.confirms.pending().is_some() {
            self.bus.publish(Event::AssistantText {
                text: answer_hint(),
                spoken: None,
            });
            return Submitted::Hinted;
        }
        let token = self.cancel.lock().clone();
        let this = self.clone();
        tokio::spawn(async move {
            // One turn at a time, in the order they were asked for: a
            // scenario queued behind this turn must not take the origin
            // while it runs (decision 0031).
            let _own = this.turns.lock().await;
            // The answer is first asked for when the turn really begins, after
            // a running reply: only then the origin becomes this computer. Set
            // at once, it would take the questions of a turn that came from
            // Telegram away from the phone.
            let turn = this.clone();
            let answer = async move {
                *turn.origin.lock() = Origin::Local;
                answer.await
            };
            // What is shown is made by the app: the name of the program in
            // front and the question, of which only the size is written.
            let number = crate::journal::next_turn();
            log::info!(
                "turn started: turn={number} source=direct chars={}",
                shown.chars().count()
            );
            let work = this.agent.handle_direct(&shown, answer, token);
            let _ = crate::journal::in_turn(number, work).await;
        });
        Submitted::Started
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm::Answer;
    use crate::gate::Gate;
    use crate::llm::{LlmChunk, LlmError, LlmProvider, LlmRequest, LlmStream, ToolCall};
    use crate::policy::{AuditLog, PolicyGate, RuleStore};
    use crate::state::{AssistantState, StateHandle};
    use crate::storage::Db;
    use crate::tools::{TimeTool, ToolRegistry};
    use async_trait::async_trait;
    use futures::StreamExt;
    use std::time::Duration;

    #[test]
    fn the_first_record_of_a_turn_has_its_number_and_its_text_only_when_allowed() {
        assert_eq!(
            started_line(7, "window", "напомни про отчёт", false, 12, false),
            "turn started: turn=7 source=window chars=17 expanded=false waited_ms=12"
        );
        assert_eq!(
            started_line(8, "telegram", "напомни про отчёт", true, 0, true),
            "turn started: turn=8 source=telegram chars=17 expanded=true waited_ms=0 text=напомни про отчёт"
        );
        // A text that reads like the blind zone is not written in any mode.
        let line = started_line(9, "voice", "мой пароль 12345", false, 0, true);
        assert!(!line.contains("12345"), "{line}");
        assert!(line.contains("chars=16"), "{line}");
    }

    #[tokio::test]
    async fn the_number_of_a_turn_is_known_inside_it_and_nowhere_else() {
        use crate::journal;
        assert_eq!(journal::turn(), None);
        assert_eq!(journal::turn_field(), "");
        let number = journal::next_turn();
        assert!(journal::next_turn() > number);
        let inside = journal::in_turn(number, async {
            // A task started from the turn is not the turn: a background
            // job or the voice must not sign its requests with the number.
            let spawned = tokio::spawn(async { journal::turn() }).await.unwrap();
            (journal::turn(), journal::turn_field(), spawned)
        })
        .await;
        assert_eq!(inside, (Some(number), format!("turn={number} "), None));
        assert_eq!(journal::turn(), None);
    }

    /// Calls get_time first, then answers with the tool result.
    struct TimeThenAnswer;

    #[async_trait]
    impl LlmProvider for TimeThenAnswer {
        fn name(&self) -> &str {
            "fake"
        }
        async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError> {
            let last = req.messages.last().unwrap();
            let chunk = if last.role == crate::llm::Role::Tool {
                LlmChunk::Text(format!("итог: {}", last.content))
            } else {
                LlmChunk::ToolCall(ToolCall {
                    id: "c1".into(),
                    name: "get_time".into(),
                    arguments: serde_json::json!({}),
                })
            };
            Ok(futures::stream::iter(vec![Ok(chunk)]).boxed())
        }
    }

    fn session() -> (Arc<Session>, EventBus, StateHandle) {
        let bus = EventBus::new(256);
        let state = StateHandle::new(bus.clone());
        let db = Arc::new(Db::open_in_memory().unwrap());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let gate: Arc<dyn Gate> = Arc::new(PolicyGate::new(
            RuleStore::new(db.clone()),
            AuditLog::new(db),
            confirms.clone(),
        ));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TimeTool));
        let agent = Arc::new(Agent::new(
            Arc::new(TimeThenAnswer),
            Arc::new(reg),
            gate,
            bus.clone(),
            state.clone(),
            "s".into(),
        ));
        (Session::new(agent, confirms, bus.clone()), bus, state)
    }

    async fn wait_pending(s: &Session) {
        for _ in 0..1000 {
            if s.confirms.pending().is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("no question");
    }

    async fn wait_idle(state: &StateHandle) {
        for _ in 0..1000 {
            if state.get() == AssistantState::Idle {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("not idle");
    }

    #[tokio::test]
    async fn text_during_question_answers_it() {
        let (s, bus, state) = session();
        let mut rx = bus.subscribe();
        assert_eq!(s.submit("который час"), Submitted::Started);
        wait_pending(&s).await;
        assert_eq!(s.submit("да"), Submitted::Answered);
        tokio::time::sleep(Duration::from_millis(5)).await;
        wait_idle(&state).await;
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ConfirmResolved {
                answer: Answer::Once,
                by: DecidedBy::Chat,
                ..
            }
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::AssistantText { text, .. } if text.starts_with("итог: "))));
    }

    #[tokio::test]
    async fn unrelated_text_during_question_gets_hint() {
        let (s, bus, _state) = session();
        let mut rx = bus.subscribe();
        s.submit("который час");
        wait_pending(&s).await;
        assert_eq!(s.submit("а погода?"), Submitted::Hinted);
        assert!(s.confirms.pending().is_some());
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let hint = answer_hint();
        assert_ne!(hint, "core.answer_hint", "the text is missing in ru.json");
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::AssistantText { text, .. } if *text == hint)));
        s.stop();
    }

    /// Answers with the text of the last message it got.
    struct EchoLast;

    #[async_trait]
    impl LlmProvider for EchoLast {
        fn name(&self) -> &str {
            "fake"
        }
        async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError> {
            let last = req.messages.last().unwrap().content.clone();
            Ok(futures::stream::iter(vec![Ok(LlmChunk::Text(last))]).boxed())
        }
    }

    #[tokio::test]
    async fn command_name_expands_into_its_instruction() {
        let bus = EventBus::new(64);
        let state = StateHandle::new(bus.clone());
        let db = Arc::new(Db::open_in_memory().unwrap());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let gate: Arc<dyn Gate> = Arc::new(PolicyGate::new(
            RuleStore::new(db.clone()),
            AuditLog::new(db.clone()),
            confirms.clone(),
        ));
        let agent = Arc::new(Agent::new(
            Arc::new(EchoLast),
            Arc::new(ToolRegistry::new()),
            gate,
            bus.clone(),
            state.clone(),
            "s".into(),
        ));
        let s = Session::new(agent, confirms, bus.clone());
        let store = CommandStore::new(db);
        store.save("Отбой", "Выключи звук").unwrap();
        s.set_commands(store);
        let mut rx = bus.subscribe();
        assert_eq!(
            s.submit_from("отбой!", Origin::Telegram),
            Submitted::Started
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        wait_idle(&state).await;
        assert_eq!(s.origin(), Origin::Telegram);
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.contains(&Event::UserText {
            text: "отбой!".into()
        }));
        assert!(events.iter().any(
            |e| matches!(e, Event::AssistantText { text, .. } if text.ends_with("Выключи звук"))
        ));
    }

    fn echo_session(db: Arc<Db>) -> (Arc<Session>, EventBus, StateHandle) {
        let bus = EventBus::new(64);
        let state = StateHandle::new(bus.clone());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let gate: Arc<dyn Gate> = Arc::new(PolicyGate::new(
            RuleStore::new(db.clone()),
            AuditLog::new(db),
            confirms.clone(),
        ));
        let agent = Arc::new(Agent::new(
            Arc::new(EchoLast),
            Arc::new(ToolRegistry::new()),
            gate,
            bus.clone(),
            state.clone(),
            "s".into(),
        ));
        (Session::new(agent, confirms, bus.clone()), bus, state)
    }

    #[tokio::test]
    async fn a_skill_name_asks_her_to_run_the_skill() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let (s, bus, state) = echo_session(db.clone());
        let skills = crate::skills::SkillStore::new(db);
        skills
            .save(crate::skills::NewSkill {
                name: "Утро".into(),
                description: String::new(),
                params: vec![],
                steps: vec![crate::skills::SkillStep {
                    tool: "get_time".into(),
                    args: serde_json::json!({}),
                }],
            })
            .unwrap();
        s.set_skills(skills);
        let mut rx = bus.subscribe();
        assert_eq!(s.submit("утро"), Submitted::Started);
        tokio::time::sleep(Duration::from_millis(20)).await;
        wait_idle(&state).await;
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.contains(&Event::UserText {
            text: "утро".into()
        }));
        assert!(events.iter().any(
            |e| matches!(e, Event::AssistantText { text, .. } if text.contains("skill_run") && text.contains("«Утро»"))
        ));
    }

    #[tokio::test]
    async fn a_shown_request_sends_its_full_text_and_never_answers_a_question() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let (s, bus, state) = echo_session(db);
        let mut rx = bus.subscribe();
        assert_eq!(
            s.submit_shown("Сохрани это как навык", "Сохрани это как навык\n\nшаги"),
            Submitted::Started
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        wait_idle(&state).await;
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.contains(&Event::UserText {
            text: "Сохрани это как навык".into()
        }));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::AssistantText { text, .. } if text.ends_with("шаги"))));
    }

    #[tokio::test]
    async fn a_task_shows_one_line_and_sends_the_model_another() {
        let bus = EventBus::new(64);
        let state = StateHandle::new(bus.clone());
        let db = Arc::new(Db::open_in_memory().unwrap());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let gate: Arc<dyn Gate> = Arc::new(PolicyGate::new(
            RuleStore::new(db.clone()),
            AuditLog::new(db),
            confirms.clone(),
        ));
        let agent = Arc::new(Agent::new(
            Arc::new(EchoLast),
            Arc::new(ToolRegistry::new()),
            gate,
            bus.clone(),
            state.clone(),
            "s".into(),
        ));
        let s = Session::new(agent, confirms, bus.clone());
        *s.origin.lock() = Origin::Telegram;
        let mut rx = bus.subscribe();
        // Nothing to send: nothing starts.
        s.submit_task_as("⚡ Сценарий «Пусто»", "  ");
        s.submit_task_as(
            " ⚡ Сценарий «Наушники»: подключено звуковое устройство ",
            "Сработал мой сценарий. Выполни по шагам: Включи музыку",
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        wait_idle(&state).await;
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let shown: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::UserText { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            shown,
            vec!["⚡ Сценарий «Наушники»: подключено звуковое устройство"]
        );
        assert!(events.iter().any(|e| matches!(
            e,
            Event::AssistantText { text, .. } if text == "Сработал мой сценарий. Выполни по шагам: Включи музыку"
        )));
        // A task is a local turn, wherever the last message came from.
        assert_eq!(s.origin(), Origin::Local);

        // An ordinary task shows what it sends.
        let mut rx = bus.subscribe();
        s.submit_task("Проверь почту");
        tokio::time::sleep(Duration::from_millis(20)).await;
        wait_idle(&state).await;
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.contains(&Event::UserText {
            text: "Проверь почту".into()
        }));
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::AssistantText { text, .. } if text == "Проверь почту")));
    }

    #[tokio::test]
    async fn a_task_queued_behind_a_telegram_turn_does_not_take_its_origin() {
        let (s, _bus, state) = session();
        assert_eq!(
            s.submit_from("который час", Origin::Telegram),
            Submitted::Started
        );
        wait_pending(&s).await;
        let first = s.confirms.pending().unwrap().id;
        assert_eq!(s.origin(), Origin::Telegram);
        // A scenario fires while the Telegram turn waits for its answer.
        s.submit_task_as("⚡ Сценарий «Ушёл»", "Сработал мой сценарий");
        tokio::time::sleep(Duration::from_millis(20)).await;
        // The rest of the Telegram turn still goes to the phone.
        assert_eq!(s.origin(), Origin::Telegram);
        assert_eq!(s.confirms.pending().unwrap().id, first);
        s.confirms
            .answer(first, Answer::Once, DecidedBy::Telegram)
            .unwrap();
        // The scenario starts when that turn has ended: its question is local.
        for _ in 0..1000 {
            if s.confirms.pending().is_some_and(|p| p.id != first) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let second = s.confirms.pending().expect("the scenario asks").id;
        assert_ne!(second, first);
        assert_eq!(s.origin(), Origin::Local);
        s.confirms
            .answer(second, Answer::Once, DecidedBy::Chat)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        wait_idle(&state).await;
    }

    /// Thinks for a while, then answers with the text of the last message.
    struct SlowEcho;

    #[async_trait]
    impl LlmProvider for SlowEcho {
        fn name(&self) -> &str {
            "fake"
        }
        async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError> {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let last = req.messages.last().unwrap().content.clone();
            Ok(futures::stream::iter(vec![Ok(LlmChunk::Text(last))]).boxed())
        }
    }

    #[tokio::test]
    async fn a_telegram_message_behind_a_scenario_takes_the_origin_only_at_its_turn() {
        let bus = EventBus::new(64);
        let state = StateHandle::new(bus.clone());
        let db = Arc::new(Db::open_in_memory().unwrap());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let gate: Arc<dyn Gate> = Arc::new(PolicyGate::new(
            RuleStore::new(db.clone()),
            AuditLog::new(db),
            confirms.clone(),
        ));
        let agent = Arc::new(Agent::new(
            Arc::new(SlowEcho),
            Arc::new(ToolRegistry::new()),
            gate,
            bus.clone(),
            state.clone(),
            "s".into(),
        ));
        let s = Session::new(agent, confirms, bus.clone());
        let mut rx = bus.subscribe();
        s.submit_task_as("⚡ Сценарий «Наушники»", "Сработал мой сценарий");
        tokio::time::sleep(Duration::from_millis(20)).await;
        // The scenario is thinking; no question is open.
        assert_eq!(
            s.submit_from("привет", Origin::Telegram),
            Submitted::Started
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        // The message waits: a question of the scenario would stay on the computer.
        assert_eq!(s.origin(), Origin::Local);
        let mut replies = Vec::new();
        for _ in 0..2000 {
            while let Ok(e) = rx.try_recv() {
                if let Event::AssistantText { text, .. } = e {
                    replies.push(text);
                }
            }
            if replies.len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(replies, vec!["Сработал мой сценарий", "привет"]);
        // Its own turn goes back to the phone.
        assert_eq!(s.origin(), Origin::Telegram);
    }

    #[tokio::test]
    async fn stop_drops_a_task_that_waits_for_its_turn() {
        let (s, bus, state) = session();
        s.submit("который час");
        wait_pending(&s).await;
        let mut rx = bus.subscribe();
        s.submit_task_as("⚡ Сценарий «Ушёл»", "Сработал мой сценарий");
        s.stop();
        tokio::time::sleep(Duration::from_millis(30)).await;
        wait_idle(&state).await;
        // The queued scenario asked nothing and answered nothing.
        assert!(s.confirms.pending().is_none());
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(!events
            .iter()
            .any(|e| matches!(e, Event::AssistantText { .. })));
    }

    fn drain(rx: &mut tokio::sync::broadcast::Receiver<Event>) -> Vec<Event> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn remark_of(events: &[Event], kind: &str) -> Option<String> {
        events.iter().find_map(|e| match e {
            Event::Remark { text, kind: k } if k == kind => Some(text.clone()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn a_full_stop_ends_the_turn_denies_the_question_and_pauses() {
        let (s, bus, state) = session();
        s.submit("который час");
        wait_pending(&s).await;
        let mut rx = bus.subscribe();
        let report = s.full_stop(
            StopReport {
                speech: true,
                ..Default::default()
            },
            None,
            true,
        );
        assert!(
            report.reply && report.confirm && report.speech,
            "{report:?}"
        );
        assert!(s.confirms.pending().is_none());
        assert!(s.pause().is_on());
        wait_idle(&state).await;
        let events = drain(&mut rx);
        assert!(events.contains(&Event::PauseChanged { paused: true }));
        assert_eq!(remark_of(&events, REMARK_STOP), Some(report.line(true)));
        // A second stop during the pause changes nothing about it.
        let mut rx = bus.subscribe();
        s.full_stop(StopReport::default(), None, true);
        assert!(!drain(&mut rx).contains(&Event::PauseChanged { paused: true }));
    }

    #[tokio::test]
    async fn a_full_stop_stops_every_background_job() {
        let (s, bus, _state) = session();
        let jobs = crate::bgjobs::JobBoard::new(bus.clone());
        let live = jobs
            .submit(
                crate::bgjobs::JobSpec::live("screen_record", "rec", Duration::from_secs(5)),
                Box::new(|ctx: crate::bgjobs::JobCtx| {
                    Box::pin(async move {
                        ctx.cancel.cancelled().await;
                        Ok(crate::tools::ToolOutput::new("Запись сохранена."))
                    })
                }),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let report = s.full_stop(StopReport::default(), Some(&jobs), true);
        assert!(report.recording);
        assert_eq!(report.jobs, 0);
        let ended = jobs
            .wait(live.job.id, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(ended.state, crate::bgjobs::JobState::Done);
    }

    #[tokio::test]
    async fn a_stop_without_the_pause_says_nothing_when_nothing_ran() {
        let (s, bus, _state) = session();
        let mut rx = bus.subscribe();
        let report = s.full_stop(StopReport::default(), None, false);
        assert!(report.is_empty());
        assert!(!s.pause().is_on());
        assert!(drain(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn during_the_pause_tasks_start_nothing_and_a_message_ends_it() {
        let (s, bus, state) = session();
        s.full_stop(StopReport::default(), None, true);
        let mut rx = bus.subscribe();
        s.submit_task("полить цветы");
        s.submit_task_as("⚡ Сценарий «Наушники»", "Сработал мой сценарий");
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(drain(&mut rx).is_empty(), "nothing started");
        assert!(s.pause().is_on());

        assert_eq!(
            s.submit_from("привет", Origin::Telegram),
            Submitted::Started
        );
        assert!(!s.pause().is_on());
        let events = drain(&mut rx);
        assert_eq!(events.first(), Some(&Event::PauseChanged { paused: false }));
        let line = remark_of(&events, REMARK_PAUSE).expect("the line of what was skipped");
        assert!(line.contains("«полить цветы»"), "{line}");
        assert!(line.contains("«⚡ Сценарий «Наушники»»"), "{line}");
        s.stop();
        wait_idle(&state).await;
    }

    #[tokio::test]
    async fn a_pause_that_skipped_nothing_ends_without_a_line() {
        let (s, bus, _state) = session();
        let mut rx = bus.subscribe();
        // No pause: being addressed changes nothing.
        s.addressed("test");
        assert!(drain(&mut rx).is_empty());
        s.full_stop(StopReport::default(), None, true);
        let mut rx = bus.subscribe();
        s.addressed("test");
        assert_eq!(drain(&mut rx), vec![Event::PauseChanged { paused: false }]);
    }

    #[tokio::test]
    async fn a_direct_turn_and_a_voice_message_end_the_pause() {
        let (s, _bus, state) = session();
        s.full_stop(StopReport::default(), None, true);
        s.submit_direct("вопрос про экран", async {
            Ok("ответ".to_string())
        });
        assert!(!s.pause().is_on());
        tokio::time::sleep(Duration::from_millis(20)).await;
        wait_idle(&state).await;
        s.full_stop(StopReport::default(), None, true);
        assert!(s.submit_new_from("голосовое", Origin::Telegram).is_some());
        assert!(!s.pause().is_on());
        s.stop();
    }

    #[tokio::test]
    async fn stop_denies_open_question() {
        let (s, _bus, state) = session();
        s.submit("который час");
        wait_pending(&s).await;
        s.stop();
        wait_idle(&state).await;
        assert!(s.confirms.pending().is_none());
    }

    #[tokio::test]
    async fn a_question_of_this_computer_is_not_answered_from_telegram_before_it_is_shown_there() {
        let (s, bus, state) = session();
        // A turn of this computer: a scenario, a scheduled task or the chat.
        s.submit_task("check the time");
        wait_pending(&s).await;
        let id = s.confirms.pending().unwrap().id;
        assert_eq!(s.origin(), Origin::Local);
        assert!(s.question_unseen(Origin::Telegram));
        // The computer itself and the microphone at it see every question.
        assert!(!s.question_unseen(Origin::Local));
        assert!(!s.question_unseen(Origin::Voice));

        let mut rx = bus.subscribe();
        // Whatever is typed in Telegram: an answer in either language, a
        // permanent one, or something else.
        for typed in ["yes", "always allow", "no", "what is open?"] {
            assert_eq!(
                s.submit_from(typed, Origin::Telegram),
                Submitted::Unseen,
                "{typed}"
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(rx.try_recv().is_err(), "nothing may reach the bus");
        assert_eq!(s.confirms.pending().unwrap().id, id, "the question stays");
        assert_eq!(s.origin(), Origin::Local, "the turn stays here");

        // Another question was shown, not this one: nothing changes.
        s.question_shown(id + 1, Origin::Telegram);
        assert!(s.question_unseen(Origin::Telegram));
        // The card went to the phone: the turn goes on there and the next
        // message answers.
        s.question_shown(id, Origin::Telegram);
        assert_eq!(s.origin(), Origin::Telegram);
        assert!(!s.question_unseen(Origin::Telegram));
        assert_eq!(s.submit_from("yes", Origin::Telegram), Submitted::Answered);
        tokio::time::sleep(Duration::from_millis(5)).await;
        wait_idle(&state).await;
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ConfirmResolved {
                answer: Answer::Once,
                by: DecidedBy::Telegram,
                ..
            }
        )));
        // No question is open: nothing is unseen, and nothing can be marked.
        assert!(!s.question_unseen(Origin::Telegram));
        *s.origin.lock() = Origin::Local;
        s.question_shown(id, Origin::Telegram);
        assert_eq!(s.origin(), Origin::Local);
    }

    #[tokio::test]
    async fn a_question_of_a_telegram_turn_is_answered_from_telegram_at_once() {
        let (s, _bus, state) = session();
        assert_eq!(
            s.submit_from("check the time", Origin::Telegram),
            Submitted::Started
        );
        wait_pending(&s).await;
        assert!(!s.question_unseen(Origin::Telegram));
        assert_eq!(s.submit_from("yes", Origin::Telegram), Submitted::Answered);
        tokio::time::sleep(Duration::from_millis(5)).await;
        wait_idle(&state).await;
    }

    #[tokio::test]
    async fn text_that_was_not_typed_never_answers_a_question() {
        let (s, bus, state) = session();
        assert_eq!(s.submit("который час"), Submitted::Started);
        wait_pending(&s).await;
        let id = s.confirms.pending().unwrap().id;
        assert_eq!(s.origin(), Origin::Local);
        let mut rx = bus.subscribe();
        // A recognised «да» is not a yes, and neither is anything else.
        for heard in ["да", "Да.", "разреши всегда", "нет", "а погода?"]
        {
            assert_eq!(s.submit_new_from(heard, Origin::Telegram), None, "{heard}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(rx.try_recv().is_err(), "nothing may reach the bus");
        assert_eq!(s.confirms.pending().unwrap().id, id, "the question stays");
        assert_eq!(s.origin(), Origin::Local, "the origin stays");
        // Empty text is empty whether or not a question is open.
        assert_eq!(
            s.submit_new_from("  ", Origin::Telegram),
            Some(Submitted::Empty)
        );
        assert_eq!(s.origin(), Origin::Local);
        s.stop();
        wait_idle(&state).await;
    }

    #[tokio::test]
    async fn a_stop_token_tells_of_the_next_stop_only() {
        let (s, _bus, _state) = session();
        let before = s.stop_token();
        assert!(!before.is_cancelled());
        s.stop();
        assert!(before.is_cancelled(), "a stop that came after it was taken");
        let after = s.stop_token();
        assert!(!after.is_cancelled(), "a stop that came before is not told");
        s.stop();
        assert!(after.is_cancelled());
    }

    #[tokio::test]
    async fn text_that_was_not_typed_starts_a_turn_and_runs_a_command() {
        let bus = EventBus::new(64);
        let state = StateHandle::new(bus.clone());
        let db = Arc::new(Db::open_in_memory().unwrap());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let gate: Arc<dyn Gate> = Arc::new(PolicyGate::new(
            RuleStore::new(db.clone()),
            AuditLog::new(db.clone()),
            confirms.clone(),
        ));
        let agent = Arc::new(Agent::new(
            Arc::new(EchoLast),
            Arc::new(ToolRegistry::new()),
            gate,
            bus.clone(),
            state.clone(),
            "s".into(),
        ));
        let s = Session::new(agent, confirms, bus.clone());
        let store = CommandStore::new(db);
        store.save("Отбой", "Выключи звук").unwrap();
        s.set_commands(store);
        let mut rx = bus.subscribe();
        assert_eq!(
            s.submit_new_from(" Отбой. ", Origin::Telegram),
            Some(Submitted::Started)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        wait_idle(&state).await;
        // The origin is taken when the turn starts, not when it is queued.
        assert_eq!(s.origin(), Origin::Telegram);
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.contains(&Event::UserText {
            text: "Отбой.".into()
        }));
        assert!(events.iter().any(
            |e| matches!(e, Event::AssistantText { text, .. } if text.ends_with("Выключи звук"))
        ));
    }

    async fn wait_state(state: &StateHandle, want: AssistantState) {
        for _ in 0..1000 {
            if state.get() == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("state {want:?} never came");
    }

    #[tokio::test]
    async fn direct_turn_during_a_question_gets_the_hint_and_is_not_run() {
        let (s, bus, _state) = session();
        s.submit("который час");
        wait_pending(&s).await;
        let mut rx = bus.subscribe();
        // What unrelated text gets while the question is open: the same hint.
        assert_eq!(s.submit("а погода?"), Submitted::Hinted);
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = polled.clone();
        let got = s.submit_direct("вопрос про окно", async move {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("ответ".to_string())
        });
        assert_eq!(got, Submitted::Hinted);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!polled.load(std::sync::atomic::Ordering::SeqCst));
        assert!(s.confirms.pending().is_some());
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let said: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                Event::AssistantText { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(said.len(), 2, "{said:?}");
        assert_eq!(said[0], said[1]);
        // The direct turn itself never reached the chat.
        assert!(!events
            .iter()
            .any(|e| matches!(e, Event::UserText { text } if text.contains("окно"))));
        s.stop();
    }

    #[tokio::test]
    async fn direct_turn_answers_on_this_computer() {
        let (s, bus, state) = session();
        *s.origin.lock() = Origin::Telegram;
        let mut rx = bus.subscribe();
        assert_eq!(
            s.submit_direct("  ", async { Ok("ответ".to_string()) }),
            Submitted::Empty
        );
        assert_eq!(s.origin(), Origin::Telegram);
        assert_eq!(
            s.submit_direct(" вопрос про окно ", async {
                Ok("ответ".to_string())
            }),
            Submitted::Started
        );
        let mut events = Vec::new();
        let reply = Event::AssistantText {
            text: "ответ".into(),
            spoken: None,
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while !events.contains(&reply) {
                events.push(rx.recv().await.unwrap());
            }
        })
        .await
        .expect("no reply");
        assert_eq!(
            events[0],
            Event::UserText {
                text: "вопрос про окно".into()
            }
        );
        wait_idle(&state).await;
        // The reply must not go to Telegram.
        assert_eq!(s.origin(), Origin::Local);
        assert_eq!(s.agent.history().len(), 2);
    }

    #[tokio::test]
    async fn direct_turn_takes_the_origin_only_when_it_begins() {
        let (s, _bus, state) = session();
        // A turn is running: it holds the turn until it is let go.
        let gate = Arc::new(tokio::sync::Notify::new());
        let open = gate.clone();
        assert_eq!(
            s.submit_direct("первый вопрос", async move {
                open.notified().await;
                Ok("первый ответ".to_string())
            }),
            Submitted::Started
        );
        wait_state(&state, AssistantState::Thinking).await;
        // As if that turn had come from Telegram: its questions go to the phone.
        *s.origin.lock() = Origin::Telegram;
        assert_eq!(
            s.submit_direct("второй вопрос", async {
                Ok("второй ответ".to_string())
            }),
            Submitted::Started
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            s.origin(),
            Origin::Telegram,
            "a waiting direct turn must not take the running turn's origin"
        );
        gate.notify_one();
        for _ in 0..1000 {
            if s.agent.history().len() == 4 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(s.agent.history().len(), 4);
        assert_eq!(s.origin(), Origin::Local);
        wait_idle(&state).await;
    }

    #[tokio::test]
    async fn stop_cancels_a_direct_turn() {
        let (s, bus, state) = session();
        let mut rx = bus.subscribe();
        assert_eq!(
            s.submit_direct("вопрос", std::future::pending::<Result<String, String>>()),
            Submitted::Started
        );
        wait_state(&state, AssistantState::Thinking).await;
        s.stop();
        wait_idle(&state).await;
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(!events
            .iter()
            .any(|e| matches!(e, Event::AssistantText { .. } | Event::Error { .. })));
        assert!(s.agent.history().is_empty());

        // The next one runs: «Стоп» ended only the turn that was running.
        assert_eq!(
            s.submit_direct("ещё вопрос", async {
                Ok("ответ".to_string())
            }),
            Submitted::Started
        );
        for _ in 0..1000 {
            if s.agent.history().len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(s.agent.history().len(), 2);
        wait_idle(&state).await;
    }
}
