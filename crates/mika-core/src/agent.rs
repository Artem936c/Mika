//! Agent loop: history, LLM stream, tool calls through the gate.

use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::AssistantConfig;
use crate::emotion::Emotion;
use crate::events::{Event, EventBus};
use crate::gate::{Gate, GateDecision};
use crate::llm::{
    ChatMessage, LlmChunk, LlmError, LlmProvider, LlmRequest, ThoughtFilter, ToolCall,
};
use crate::marks::MarkFilter;
use crate::memory::Recall;
use crate::skills::{is_read, SkillBook};
use crate::state::{AssistantState, StateHandle};
use crate::storage::Db;
use crate::task::{TaskBoard, TASK_MAX_STEPS};
use crate::tools::{with_progress, Replay, RiskLevel, ToolError, ToolRegistry};

/// LLM calls per user message. A task with a plan gets `task::TASK_MAX_STEPS`.
pub const MAX_STEPS: usize = 14;
/// Reminders per message that a plan still has open steps.
const MAX_NUDGES: usize = 2;
/// Reminder after a reply that stopped without doing what it said.
const STALL_NOTE: &str = "(служебная заметка) Ты остановилась, не доделав просьбу. \
Не обещай и не жди разрешения на уже одобренное: продолжай вызовами инструментов сейчас же. \
Если всё сделано или что-то мешает, коротко скажи пользователю, что вышло.";
/// Joins the system prompt while the voice performs marks (decision 0026).
pub const MARKS_HINT: &str = "Твои ответы озвучивает голос, который исполняет разметку. \
Размечай ответ для голоса умеренно, одна-две пометки на ответ, там, где они к месту.\n\
- Ремарка: короткое указание в квадратных скобках прямо перед словами, лучше по-английски, \
например [softly, with a smile], [barely holding back tears], [sigh], [laughs], [pause]. \
Она действует до конца строки или до следующей ремарки. Внутри ремарки только слова \
и знаки препинания, не больше 80 знаков: без цифр, длительностей, смайликов и знаков \
вроде / ; ( ) < >. Паузу пиши [pause] или [long pause], а не [pause 2s]. \
Перед [ ставь пробел, не приклеивай ремарку к слову и не ставь ( сразу после ].\n\
- Парные теги вокруг слов: <whisper>…</whisper> шёпот, <emphasis>…</emphasis> выделить, \
<soft>…</soft> мягко, <fast>…</fast> быстрее, <slow>…</slow> медленнее, \
<stress>…</stress> с нажимом, <forceful>…</forceful> напористо. Теги пиши только так, \
в угловых скобках: не пиши [/whisper], <laugh>, <sigh>, <break> и другую разметку SSML, \
смех и вздох — ремарками [laughs] и [sigh]. Других тегов нет.\n\
Пользователь разметку не видит, не упоминай её. Квадратные скобки используй только для ремарок: \
названия кнопок, пунктов меню и цитаты пиши в «ёлочках». Не ставь разметку в код, \
в аргументы инструментов, в сообщения, которые отправляешь людям, и в тексты, которые сохраняешь. \
Не пиши ремарки в круглых скобках: их голос читает вслух.";
/// Tool output sent back to the LLM, characters.
const MAX_TOOL_RESULT: usize = 16_000;
/// Tool output shown in the chat row, characters.
const MAX_TOOL_PREVIEW: usize = 400;
/// Past conversation sent with each request, characters. Small local models have small contexts.
pub const HISTORY_BUDGET: usize = 40_000;
/// Error of a direct turn whose answer came back empty.
const EMPTY_DIRECT_REPLY: &str = "модель ничего не ответила";

fn message_size(m: &ChatMessage) -> usize {
    m.content.chars().count()
        + m.tool_calls
            .iter()
            .map(|c| c.name.len() + c.arguments.to_string().len())
            .sum::<usize>()
}

/// The newest part of `history` that fits `budget`. Cuts only before a user message,
/// so a tool result never loses the assistant message that asked for it.
pub fn trim_history(history: &[ChatMessage], budget: usize) -> &[ChatMessage] {
    let mut size = 0;
    let mut start = history.len();
    for (i, m) in history.iter().enumerate().rev() {
        size += message_size(m);
        if size > budget {
            break;
        }
        if m.role == crate::llm::Role::User {
            start = i;
        }
    }
    &history[start..]
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("остановлено")]
    Cancelled,
    #[error("слишком много шагов подряд, остановилась")]
    TooManySteps,
    #[error("модель зациклилась и повторяла одно и то же: ответ остановлен. Попробуй сказать иначе или выбери другую модель")]
    Looping,
}

impl AgentError {
    /// The kind of the error as the journal names it.
    fn kind(&self) -> &'static str {
        match self {
            AgentError::Llm(e) => e.kind(),
            AgentError::Cancelled => "stopped",
            AgentError::TooManySteps => "too_many_steps",
            AgentError::Looping => "looping",
        }
    }
}

/// What a turn did, for its last record in the journal.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TurnStats {
    /// Requests to the model.
    steps: usize,
    /// Tool calls the model asked for.
    tools: usize,
}

/// `turn=N ` for a record, or nothing for a turn without a number.
fn turn_field(turn: Option<u64>) -> String {
    turn.map_or_else(String::new, |turn| format!("turn={turn} "))
}

/// The last record of a turn: how it ended, how long it took, what it did.
/// The reply itself is the model's text: its size is always written, its
/// beginning only while texts are (`content`).
fn turn_line(
    result: &Result<(String, Option<String>), AgentError>,
    stats: TurnStats,
    ms: u128,
    turn: Option<u64>,
    content: bool,
) -> (log::Level, String) {
    let did = format!("ms={ms} steps={} tools={}", stats.steps, stats.tools);
    let turn = turn_field(turn);
    match result {
        Ok((reply, _)) => {
            let mut line = format!(
                "turn finished: {turn}outcome=ok {did} reply_chars={}",
                reply.chars().count()
            );
            if content {
                line.push_str(" reply=");
                line.push_str(&crate::journal::guarded_if(true, reply));
            }
            (log::Level::Info, line)
        }
        Err(AgentError::Cancelled) => (
            log::Level::Info,
            format!("turn finished: {turn}outcome=stopped {did}"),
        ),
        Err(e) => {
            let text = match e {
                AgentError::Llm(e) => {
                    let status = match e {
                        LlmError::Http { status, .. } => format!("http={status} "),
                        _ => String::new(),
                    };
                    format!("{status}error={}", e.journal_text())
                }
                other => format!("error={}", crate::journal::error_text(other)),
            };
            (
                log::Level::Error,
                format!(
                    "turn finished: {turn}outcome=error kind={} {did} {text}",
                    e.kind()
                ),
            )
        }
    }
}

/// What the journal says about one tool call besides its name and time.
#[derive(Debug, Clone, PartialEq)]
struct CallNote {
    /// Not known for a call that never got to its assessment.
    risk: Option<RiskLevel>,
    /// `rule`, `asked_yes`, `asked_always`, `denied`, `stopped`; `-` when the
    /// gate was never asked.
    decision: &'static str,
    /// `ok`, `error`, `denied`, `stopped`, `unknown_tool`, `bad_arguments`,
    /// `not_ready`, `blind_zone`. It is also the kind of the error: the text
    /// of the error is written only while texts are.
    outcome: &'static str,
    /// The text of the error, already made fit for a record.
    error: Option<String>,
    /// The blind zone refused the call: nothing of it is written, neither
    /// the error nor the arguments, which may hold what was to be typed
    /// into that window.
    blind: bool,
}

impl CallNote {
    fn new(outcome: &'static str) -> Self {
        Self {
            risk: None,
            decision: "-",
            outcome,
            error: None,
            blind: false,
        }
    }

    fn risk(mut self, risk: RiskLevel) -> Self {
        self.risk = Some(risk);
        self
    }

    fn decision(mut self, decision: &'static str) -> Self {
        self.decision = decision;
        self
    }

    fn outcome(mut self, outcome: &'static str) -> Self {
        self.outcome = outcome;
        self
    }

    /// The error of the call. What a tool says when it fails quotes what
    /// it was given and what it saw (titles of the open windows, what a
    /// window shows, paths), so the text is written only while texts are
    /// (`content`). The refusal of the blind zone is told by what it is and
    /// leaves no text at all.
    fn error(mut self, error: &dyn std::fmt::Display, content: bool) -> Self {
        if crate::journal::is_blind_refusal(error) {
            self.outcome = "blind_zone";
            self.blind = true;
            self.error = None;
        } else {
            self.error = Some(crate::journal::guarded_error_if(content, error));
        }
        self
    }

    /// A call that ended the way it was meant to: done, refused by the user
    /// or stopped by him.
    fn expected(&self) -> bool {
        matches!(self.outcome, "ok" | "denied" | "stopped")
    }
}

/// Longest name of a tool in a record: a name the model made up may be long.
const MAX_LOGGED_NAME: usize = 64;

/// The record that a tool starts to run, after the gate let it through. A
/// hang or a crash in the middle of a call leaves this one behind.
fn call_started_line(name: &str, note: &CallNote, turn: Option<u64>) -> String {
    let name: String = name.chars().take(MAX_LOGGED_NAME).collect();
    format!(
        "tool call started: {}tool={name} risk={} decision={}",
        turn_field(turn),
        note.risk.map_or("-", RiskLevel::as_str),
        note.decision
    )
}

/// The record of one tool call. `args` are the arguments as they go into the
/// record: given only while texts are allowed.
fn call_line(
    name: &str,
    note: &CallNote,
    ms: u128,
    args: Option<&str>,
    turn: Option<u64>,
) -> String {
    let name: String = name.chars().take(MAX_LOGGED_NAME).collect();
    let mut line = format!(
        "tool call finished: {}tool={name} risk={} decision={} ms={ms} outcome={}",
        turn_field(turn),
        note.risk.map_or("-", RiskLevel::as_str),
        note.decision,
        note.outcome
    );
    if let Some(args) = args {
        line.push_str(" args=");
        line.push_str(if note.blind {
            crate::journal::BLIND_MARK
        } else {
            args
        });
    }
    if let Some(error) = &note.error {
        line.push_str(" error=");
        line.push_str(error);
    }
    line
}

/// Longest reply text of one step, characters: past it the model is rambling.
const MAX_REPLY: usize = 12_000;

/// Longest raw text of one step, characters, with reasoning and tool calls
/// written as text. They never reach the reply, so `MAX_REPLY` does not see them,
/// and some providers set no token limit.
const MAX_RAW: usize = 60_000;

/// How many last lines are compared to notice a loop.
const LOOP_LINES: usize = 30;

/// Whether a text goes round in circles: among its last `LOOP_LINES` finished
/// lines only one or two different ones. Code blocks, table rows and lines
/// without letters or digits do not count: an empty table, a checklist or a
/// picture of symbols repeats its lines on purpose.
fn repeating(text: &str) -> bool {
    let finished = text.rfind('\n').map_or("", |at| &text[..at]);
    let mut code = false;
    let mut lines: Vec<&str> = Vec::new();
    for line in finished.lines().map(str::trim) {
        if line.starts_with("```") {
            code = !code;
            continue;
        }
        if code || line.starts_with('|') || !line.chars().any(char::is_alphanumeric) {
            continue;
        }
        lines.push(line);
    }
    if lines.len() < LOOP_LINES {
        return false;
    }
    let mut distinct: Vec<&str> = Vec::new();
    for l in &lines[lines.len() - LOOP_LINES..] {
        if !distinct.contains(l) {
            distinct.push(l);
        }
    }
    distinct.len() <= 2
}

/// Whether a reply went round in circles or grew past any sensible length.
pub fn looping(text: &str) -> bool {
    text.chars().count() > MAX_REPLY || repeating(text)
}

pub fn system_prompt(assistant: &AssistantConfig) -> String {
    if crate::i18n::lang() == crate::i18n::Lang::En {
        return crate::i18n::prompt::system_prompt_en(assistant);
    }
    format!(
        "Ты {name}, живой голосовой и текстовый помощник на компьютере пользователя с Windows. \
Говоришь по-русски, о себе в женском роде, к собеседнику обращаешься на «ты» напрямую, \
тепло и коротко: одна-три фразы, если не просят подробнее. \
Результаты инструментов пересказывай своими словами, без служебных строк вроде «код выхода». \
Для действий на компьютере и для свежих данных вызывай инструменты. \
Не говори, что сделала действие, пока инструмент не вернул успех. \
Если пользователь отклонил действие, прими это и предложи другой путь. \
Не придумывай факты о системе: проверь инструментом или скажи, что не знаешь. \
Ты умеешь шутить, рассказывать анекдоты, переводить и играть в словесные игры без инструментов. \
Когда пользователь сообщает о себе что-то важное на будущее (имя, близкие, предпочтения, даты), \
сохрани это инструментом memory_save. \
Чтобы действовать в окне программы, сначала прочитай его через ui_read, потом нажимай и печатай \
по подписям элементов (надёжнее номеров); несколько кнопок подряд нажимай одним ui_click с names. \
Ответ на действие сам сообщает, что изменилось в окне, появился ли диалог или окно закрылось: \
сверяй это с тем, что ожидала, и исправляй, если вышло не так.\n\
{persona}\n\
В самом начале ответа можешь поставить одну метку эмоции в квадратных скобках: \
[joy] радость, [shy] смущение, [surprise] удивление, [sad] грусть, [angry] сердишься, \
[think] задумалась, [wink] шутишь, [sleepy] сонная, [worry] тревога. \
Ставь метку, только когда эмоция уместна. Пользователь метку не видит, не упоминай её. \
Ты сама решаешь, что прозвучит голосом. Длинный ответ голос не зачитывает, а говорит его короткий пересказ. \
Если ответ должен прозвучать целиком, слово в слово (анекдот, шутка, стихотворение, сказка, история, \
тост, поздравление, или тебя попросили прочитать что-то вслух), поставь в самом начале метку [aloud], \
рядом с меткой эмоции. Такой текст рассказывай полностью, а не описывай его. Эту метку пользователь тоже не видит.",
        name = assistant.name,
        persona = assistant.persona_prompt(),
    )
}

/// Longest text between `[` and `]` that can still be an emotion tag.
const MAX_TAG: usize = 16;

/// Words of a reply that promise an action without doing it: «подожди
/// секунду», «сейчас попробую». A small model says them and ends its turn.
const PROMISES: [&str; 9] = [
    "подожди",
    "подождёшь",
    "подождешь",
    "секунду",
    "минутку",
    "сейчас попробую",
    "сейчас я ",
    "я попробую",
    "начала искать",
];

/// True when the reply promises an action that should have been a tool call.
pub fn promises_action(reply: &str) -> bool {
    let t = reply.to_lowercase();
    PROMISES.iter().any(|p| t.contains(p))
}

/// A one-word Latin tag such as `[smile]` at the very start of a reply: small
/// models invent their own emotion names, and none of them is meant to be read.
fn is_invented_tag(inner: &str) -> bool {
    (2..=MAX_TAG).contains(&inner.len())
        && inner
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
}

/// `🤔` or `😊😊`: a few emoji (symbols from U+2600 up, with their joiners), nothing else.
fn is_emoji_tag(inner: &str) -> bool {
    let n = inner.chars().count();
    (1..=4).contains(&n)
        && inner
            .chars()
            .all(|c| (c as u32 >= 0x2600 && !c.is_alphanumeric()) || c == '\u{200d}')
}

/// The tag at the very start of a reply that asks the voice to say it whole,
/// however long: a joke, a poem, a story (decision 0037).
fn is_whole_tag(inner: &str) -> bool {
    matches!(inner.trim().to_lowercase().as_str(), "aloud" | "вслух")
}

/// Cuts emotion tags like `[joy]` out of streamed reply text.
///
/// A tag may arrive split across chunks, so text after `[` is held back until `]`
/// comes or the text is too long to be a tag. Brackets that are not tags pass through.
#[derive(Debug, Default)]
pub struct TagFilter {
    held: String,
    started: bool,
    skip_space: bool,
    /// Brackets that make a voice direction pass unchanged and give no cue:
    /// the voice mark filter after this one cuts them (decision 0026).
    keep_directions: bool,
    /// The last character given out.
    last: Option<char>,
    /// The character given out before the held `[`.
    before: Option<char>,
    /// `[aloud]` stood at the start and nobody has taken it yet.
    whole: bool,
    /// Words were given out, not only voice directions: the start is over.
    worded: bool,
}

impl TagFilter {
    /// A filter in front of the voice mark filter: what it cuts leaves both
    /// the shown and the spoken text, and directions are left to the other.
    pub fn keeping_directions() -> Self {
        Self {
            keep_directions: true,
            ..Self::default()
        }
    }

    fn emit(&mut self, out: &mut String, text: &str) {
        for ch in text.chars() {
            // No whitespace before the first visible character, one space less after a tag.
            if !self.started && ch.is_whitespace() {
                continue;
            }
            if std::mem::take(&mut self.skip_space) && ch == ' ' {
                continue;
            }
            self.started = true;
            self.worded |= !ch.is_whitespace();
            self.last = Some(ch);
            out.push(ch);
        }
    }

    /// Whether `[` + `inner` + `]` is a voice direction for the mark filter.
    fn direction(&self, inner: &str) -> bool {
        self.keep_directions
            && !self.before.is_some_and(char::is_alphanumeric)
            && crate::marks::is_direction(inner)
    }

    /// Returns the text to show and the emotions of the tags that ended in this chunk.
    pub fn push(&mut self, chunk: &str) -> (String, Vec<Emotion>) {
        let mut out = String::new();
        let mut cues = Vec::new();
        for ch in chunk.chars() {
            if self.held.is_empty() {
                if ch == '[' {
                    self.before = self.last;
                    self.held.push(ch);
                } else {
                    self.emit(&mut out, ch.encode_utf8(&mut [0; 4]));
                }
                continue;
            }
            match ch {
                // Before the directions: `[aloud]` is words in brackets too,
                // and the voice must not get it as one.
                ']' if !self.worded && is_whole_tag(&self.held[1..]) => {
                    self.held.clear();
                    self.whole = true;
                    self.skip_space = true;
                }
                ']' if self.direction(&self.held[1..]) => {
                    let held = std::mem::take(&mut self.held);
                    // A direction in front of `[aloud]` is not the words yet.
                    let worded = self.worded;
                    self.emit(&mut out, &held);
                    self.emit(&mut out, "]");
                    self.worded = worded;
                }
                ']' => {
                    let held = std::mem::take(&mut self.held);
                    match Emotion::from_tag(&held[1..]) {
                        Some(e) => {
                            cues.push(e);
                            self.skip_space = true;
                        }
                        // Small models invent tags like `[🤔]`: an emoji in brackets is dropped.
                        None if is_emoji_tag(&held[1..]) => self.skip_space = true,
                        None if !self.started && is_invented_tag(&held[1..]) => {
                            self.skip_space = true
                        }
                        None => {
                            self.emit(&mut out, &held);
                            self.emit(&mut out, "]");
                        }
                    }
                }
                '[' => {
                    let held = std::mem::replace(&mut self.held, "[".into());
                    self.emit(&mut out, &held);
                    self.before = self.last;
                }
                _ => {
                    self.held.push(ch);
                    if self.held.chars().count() > MAX_TAG + 1 {
                        let held = std::mem::take(&mut self.held);
                        self.emit(&mut out, &held);
                    }
                }
            }
        }
        (out, cues)
    }

    /// Whether the reply asked to be said whole since the last call.
    pub fn take_whole(&mut self) -> bool {
        std::mem::take(&mut self.whole)
    }

    /// Text still held back when the stream ends.
    pub fn finish(&mut self) -> String {
        let held = std::mem::take(&mut self.held);
        let mut out = String::new();
        self.emit(&mut out, &held);
        out
    }
}

/// Adds a paragraph to the system prompt of each request: the time, remembered facts.
pub type PromptExtra = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Whether replies are marked up for the voice now. Asked once per LLM step.
pub type MarksSwitch = Arc<dyn Fn() -> bool + Send + Sync>;

/// The spoken copy of one step's reply while the voice marks are on.
#[derive(Debug, Default)]
struct StepSpeech {
    /// Spoken text of the deltas already published.
    said: String,
    /// Spoken text that waits for a delta with something to show.
    pending: String,
}

pub struct Agent {
    provider: RwLock<Arc<dyn LlmProvider>>,
    registry: RwLock<Arc<ToolRegistry>>,
    extras: RwLock<Vec<PromptExtra>>,
    /// Voice marks in replies; off while unset.
    marks: RwLock<Option<MarksSwitch>>,
    recall: RwLock<Option<Arc<dyn Recall>>>,
    /// What memory returned for the message being answered.
    recalled: Mutex<Option<String>>,
    gate: Arc<dyn Gate>,
    bus: EventBus,
    state: StateHandle,
    system_prompt: RwLock<String>,
    history: Mutex<Vec<ChatMessage>>,
    store: Option<(Arc<Db>, String)>,
    turn: tokio::sync::Mutex<()>,
    /// Plan of the message being answered; with a plan the step limit is higher.
    tasks: RwLock<Option<TaskBoard>>,
    /// What the turn did, for a skill (decision 0047); nothing is recorded while unset.
    skills: RwLock<Option<SkillBook>>,
}

/// How one call ended.
struct Done {
    ok: bool,
    /// For the model: the output or what went wrong.
    text: String,
    /// The steps of a skill the call gave, to run next.
    replay: Option<Replay>,
}

impl Done {
    fn failed(text: String) -> Self {
        Self {
            ok: false,
            text,
            replay: None,
        }
    }
}

/// Output of one step of a skill in the answer to the model, characters.
const MAX_STEP_RESULT: usize = 1_500;

impl Agent {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        registry: Arc<ToolRegistry>,
        gate: Arc<dyn Gate>,
        bus: EventBus,
        state: StateHandle,
        system_prompt: String,
    ) -> Self {
        Self {
            provider: RwLock::new(provider),
            registry: RwLock::new(registry),
            extras: RwLock::new(Vec::new()),
            marks: RwLock::new(None),
            recall: RwLock::new(None),
            recalled: Mutex::new(None),
            gate,
            bus,
            state,
            system_prompt: RwLock::new(system_prompt),
            history: Mutex::new(Vec::new()),
            store: None,
            turn: tokio::sync::Mutex::new(()),
            tasks: RwLock::new(None),
            skills: RwLock::new(None),
        }
    }

    /// The book the turns write their calls to, for skills (decision 0047).
    pub fn set_skill_book(&self, book: SkillBook) {
        book.set_registry(&self.registry());
        *self.skills.write() = Some(book);
    }

    /// The board that `task_plan` and `task_step` write to.
    pub fn set_task_board(&self, board: TaskBoard) {
        *self.tasks.write() = Some(board);
    }

    fn step_limit(&self) -> usize {
        match self.tasks.read().as_ref() {
            Some(b) if b.active() => TASK_MAX_STEPS,
            _ => MAX_STEPS,
        }
    }

    /// Persists every finished exchange to `db` under `conversation`.
    pub fn with_store(mut self, db: Arc<Db>, conversation: impl Into<String>) -> Self {
        self.store = Some((db, conversation.into()));
        self
    }

    pub fn with_history(self, messages: Vec<ChatMessage>) -> Self {
        *self.history.lock() = messages;
        self
    }

    pub fn set_provider(&self, provider: Arc<dyn LlmProvider>) {
        *self.provider.write() = provider;
    }

    /// The model the agent talks to now.
    pub fn provider(&self) -> Arc<dyn LlmProvider> {
        self.provider.read().clone()
    }

    /// Replaces the tool set, for example when an MCP server connects.
    pub fn set_registry(&self, registry: Arc<ToolRegistry>) {
        if let Some(book) = self.skills.read().as_ref() {
            book.set_registry(&registry);
        }
        *self.registry.write() = registry;
    }

    pub fn registry(&self) -> Arc<ToolRegistry> {
        self.registry.read().clone()
    }

    /// Memory asked once per user message; its answer joins the system prompt.
    pub fn set_recall(&self, recall: Arc<dyn Recall>) {
        *self.recall.write() = Some(recall);
    }

    pub fn add_prompt_extra(&self, extra: PromptExtra) {
        self.extras.write().push(extra);
    }

    /// Decides at every step whether replies carry voice marks: the model is
    /// asked for them, the chat gets the text without them, the speaker with them.
    pub fn set_marks_switch(&self, switch: MarksSwitch) {
        *self.marks.write() = Some(switch);
    }

    fn marks_on(&self) -> bool {
        let switch = self.marks.read().clone();
        switch.is_some_and(|on| on())
    }

    fn full_system_prompt(&self, marks: bool) -> String {
        let mut prompt = self.system_prompt.read().clone();
        for extra in self.extras.read().iter() {
            if let Some(text) = extra().filter(|t| !t.trim().is_empty()) {
                prompt.push_str("\n\n");
                prompt.push_str(text.trim());
            }
        }
        if let Some(note) = crate::i18n::prompt::reply_note(crate::i18n::lang()) {
            prompt.push_str("\n\n");
            prompt.push_str(note);
        }
        if marks {
            prompt.push_str("\n\n");
            prompt.push_str(MARKS_HINT);
        }
        if let Some(text) = self.recalled.lock().as_deref() {
            prompt.push_str("\n\n");
            prompt.push_str(text.trim());
        }
        prompt
    }

    pub fn set_system_prompt(&self, prompt: String) {
        *self.system_prompt.write() = prompt;
    }

    /// Adds an assistant message that no reply produced, such as a fired reminder,
    /// so the next reply knows about it.
    pub fn append_note(&self, text: &str) {
        self.commit(vec![ChatMessage::assistant(text)]);
    }

    pub fn history(&self) -> Vec<ChatMessage> {
        self.history.lock().clone()
    }

    pub fn clear_history(&self) {
        self.history.lock().clear();
    }

    /// Handles one user message. Returns the final reply text.
    pub async fn handle(
        &self,
        text: &str,
        cancel: CancellationToken,
    ) -> Result<String, AgentError> {
        self.handle_expanded(text, text, cancel).await
    }

    /// Like `handle`, but the chat shows `shown` while the model gets `text`:
    /// a user command expands into its instruction.
    pub async fn handle_expanded(
        &self,
        shown: &str,
        text: &str,
        cancel: CancellationToken,
    ) -> Result<String, AgentError> {
        let _turn = self.turn.lock().await;
        let started = Instant::now();
        let text = text.trim();
        self.bus.publish(Event::UserText {
            text: shown.trim().into(),
        });
        self.state.set(AssistantState::Thinking);

        let recall = self.recall.read().clone();
        let recalled = match recall {
            Some(r) => tokio::select! {
                _ = cancel.cancelled() => None,
                found = r.recall(text) => found,
            },
            None => None,
        };
        *self.recalled.lock() = recalled;

        let board = self.tasks.read().clone();
        if let Some(b) = &board {
            b.clear();
        }
        let book = self.skills.read().clone();
        if let Some(b) = &book {
            b.begin_turn();
        }
        let mut new_msgs = vec![ChatMessage::user(text)];
        let mut stats = TurnStats::default();
        let result = self.run_steps(&mut new_msgs, &cancel, &mut stats).await;
        if let Some(b) = &board {
            b.finish(result.is_err());
        }
        let offer = book.and_then(|b| b.end_turn(result.is_ok()));
        let (level, line) = turn_line(
            &result,
            stats,
            started.elapsed().as_millis(),
            crate::journal::turn(),
            crate::journal::content_allowed(),
        );
        log::log!(level, "{line}");

        match &result {
            Ok((reply, spoken)) => {
                self.bus.publish(Event::AssistantText {
                    text: reply.clone(),
                    spoken: spoken.clone(),
                });
                // Under the reply, never before it.
                if let Some(o) = offer {
                    log::info!(
                        target: "tool",
                        "skill offered: {}recording={} actions={}",
                        crate::journal::turn_field(),
                        o.id,
                        o.actions
                    );
                    self.bus.publish(Event::SkillOffer {
                        id: o.id,
                        actions: o.actions,
                    });
                }
            }
            Err(AgentError::Cancelled) => {}
            Err(e) => {
                self.bus.publish(Event::Error {
                    message: e.to_string(),
                });
            }
        }
        if matches!(result, Err(AgentError::TooManySteps)) {
            new_msgs.push(ChatMessage::assistant(AgentError::TooManySteps.to_string()));
        }
        self.commit(new_msgs);
        self.state.set(AssistantState::Idle);
        result.map(|(reply, _)| reply)
    }

    /// A direct turn: the user started an action whose answer is made outside
    /// the agent loop, such as a question about a picture of a window. The chat
    /// shows `shown`, then what `answer` gives; the pair joins the history and
    /// «Стоп» ends the wait, as with any reply. The model of the agent, the
    /// tools, the gate, the task board and the recall are not touched.
    /// Returns the reply, `None` after an error or a stop.
    pub async fn handle_direct<F>(
        &self,
        shown: &str,
        answer: F,
        cancel: CancellationToken,
    ) -> Option<String>
    where
        F: std::future::Future<Output = Result<String, String>> + Send,
    {
        let _turn = self.turn.lock().await;
        let started = Instant::now();
        let shown = shown.trim();
        self.bus.publish(Event::UserText { text: shown.into() });
        self.state.set(AssistantState::Thinking);

        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            got = answer => Some(got.and_then(|text| {
                let text = text.trim().to_string();
                if text.is_empty() {
                    Err(EMPTY_DIRECT_REPLY.to_string())
                } else {
                    Ok(text)
                }
            })),
        };
        let reply = match outcome {
            Some(Ok(reply)) => {
                log::info!(
                    "turn finished: {}outcome=ok ms={} direct=true reply_chars={}",
                    crate::journal::turn_field(),
                    started.elapsed().as_millis(),
                    reply.chars().count()
                );
                self.bus.publish(Event::AssistantText {
                    text: reply.clone(),
                    spoken: None,
                });
                self.commit(vec![
                    ChatMessage::user(shown),
                    ChatMessage::assistant(reply.clone()),
                ]);
                Some(reply)
            }
            Some(Err(message)) => {
                // The reason may quote what the model sent or what was on
                // the screen: written only while texts are. Its kind is
                // always written; what made the answer wrote the cause in a
                // record of its own before this one.
                let kind = if message == EMPTY_DIRECT_REPLY {
                    "empty_answer"
                } else {
                    "failed"
                };
                log::error!(
                    "turn finished: {}outcome=error error_kind={kind} ms={} direct=true error={}",
                    crate::journal::turn_field(),
                    started.elapsed().as_millis(),
                    crate::journal::guarded(&message)
                );
                self.bus.publish(Event::Error { message });
                None
            }
            // Stopped by the user: nothing more is said and nothing is kept.
            None => {
                log::info!(
                    "turn finished: {}outcome=stopped ms={} direct=true",
                    crate::journal::turn_field(),
                    started.elapsed().as_millis()
                );
                None
            }
        };
        self.state.set(AssistantState::Idle);
        reply
    }

    /// Whether a turn is running now, whatever the shared state shows. An
    /// action of the user that must not run beside a reply asks this: the
    /// state alone cannot tell its own `Thinking` from a turn's.
    pub fn turn_running(&self) -> bool {
        self.turn.try_lock().is_err()
    }

    /// Whether a multi-step task with a plan is unfinished.
    pub fn task_active(&self) -> bool {
        self.tasks.read().as_ref().is_some_and(TaskBoard::active)
    }

    fn commit(&self, msgs: Vec<ChatMessage>) {
        if let Some((db, conv)) = &self.store {
            for m in &msgs {
                if let Err(e) = db.append_message(conv, m) {
                    log::warn!("cannot save a message of the conversation: {e}");
                }
            }
        }
        self.history.lock().extend(msgs);
    }

    /// Runs LLM steps. On error `msgs` keeps only finished steps, so history stays valid.
    /// Returns the reply and, with voice marks on, its spoken copy.
    async fn run_steps(
        &self,
        msgs: &mut Vec<ChatMessage>,
        cancel: &CancellationToken,
        stats: &mut TurnStats,
    ) -> Result<(String, Option<String>), AgentError> {
        let mut step = 0;
        let mut nudges = 0;
        // The step before had voice marks on.
        let mut marked = false;
        // A tool of this message sent its work to the background: «скажу,
        // когда закончится» is then the right answer, not a stalled turn.
        let mut backgrounded = false;
        while step < self.step_limit() {
            if marked {
                // The next step finds its marks anew: so must the speaker.
                self.bus.publish(Event::StepStarted);
            }
            stats.steps += 1;
            let (text, spoken, calls) = self.one_step(msgs, cancel, step == 0).await?;
            marked = spoken.is_some();
            step += 1;
            stats.tools += calls.len();
            if calls.is_empty() {
                // Small models end a turn in silence after tools, or promise
                // «подожди секунду» and stop: remind them to act or to report.
                // An empty reply is never an answer, with or without tools before it.
                let stalled = text.trim().is_empty() || (promises_action(&text) && !backgrounded);
                if stalled && nudges < MAX_NUDGES {
                    nudges += 1;
                    log::info!(
                        "the model stopped without an answer, reminded: step={step} empty={}",
                        text.trim().is_empty()
                    );
                    if !text.is_empty() {
                        msgs.push(ChatMessage::assistant(text));
                    }
                    msgs.push(ChatMessage::user(STALL_NOTE));
                    continue;
                }
                // Small models stop in the middle of a plan: remind them once or twice.
                let open = self
                    .tasks
                    .read()
                    .as_ref()
                    .map(TaskBoard::open_steps)
                    .unwrap_or_default();
                if !open.is_empty() && nudges < MAX_NUDGES {
                    nudges += 1;
                    log::info!(
                        "the model left plan steps open, reminded: step={step} open={}",
                        open.len()
                    );
                    if !text.is_empty() {
                        msgs.push(ChatMessage::assistant(text));
                    }
                    let list: Vec<String> = open.iter().map(usize::to_string).collect();
                    msgs.push(ChatMessage::user(format!(
                        "(служебная заметка) В плане не закрыты шаги {}. Продолжай их выполнять \
и отмечай task_step, а если шаг не выходит — отметь failed с причиной и скажи пользователю.",
                        list.join(", ")
                    )));
                    continue;
                }
                msgs.push(ChatMessage::assistant(text.clone()));
                return Ok((text, spoken));
            }
            msgs.push(ChatMessage::assistant_with_calls(text, calls.clone()));
            for call in calls {
                let result = self.execute(&call, cancel, marked).await;
                backgrounded |= result.starts_with(crate::bgjobs::STARTED_PREFIX);
                msgs.push(ChatMessage::tool_result(call.id, result));
                if cancel.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
            }
        }
        Err(AgentError::TooManySteps)
    }

    /// One LLM request: the reply text, its spoken copy with voice marks (only
    /// while they are on) and the tool calls.
    async fn one_step(
        &self,
        msgs: &[ChatMessage],
        cancel: &CancellationToken,
        // The first request of a turn reports how long the provider took to start answering.
        timed: bool,
    ) -> Result<(String, Option<String>, Vec<ToolCall>), AgentError> {
        let asked = Instant::now();
        let mut waiting_first = timed;
        // One look at the switch decides both the hint and the filter of this step.
        let marks_on = self.marks_on();
        let mut messages = vec![ChatMessage::system(self.full_system_prompt(marks_on))];
        messages.extend(
            trim_history(&self.history.lock(), HISTORY_BUDGET)
                .iter()
                .cloned(),
        );
        messages.extend(msgs.iter().cloned());
        let req = LlmRequest {
            messages,
            tools: self.registry().specs(),
        };
        let provider = self.provider.read().clone();

        let mut stream = tokio::select! {
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            s = provider.stream(req) => s?,
        };
        let mut text = String::new();
        // Everything the model streamed in this step, with the reasoning and the
        // calls written as text that `text` never gets: a loop can hide there.
        let mut raw = String::new();
        let mut raw_chars = 0;
        let mut calls = Vec::new();
        // With voice marks on, emotion tags that are no direction leave both
        // copies of the reply; the directions are left to the mark filter.
        let mut tags = if marks_on {
            TagFilter::keeping_directions()
        } else {
            TagFilter::default()
        };
        let mut thoughts = ThoughtFilter::default();
        let mut marks = marks_on.then(MarkFilter::default);
        let mut speech = marks_on.then(StepSpeech::default);
        // A reply that starts like tool calls written as text is held back, see `calls_from_text`.
        let mut written_calls = String::new();
        // The same calls as they came: with voice marks on, the shown copy has
        // lost what looked like a direction inside their arguments.
        let mut written_raw = String::new();
        // Spoken text that waited before the calls: said if they are no calls.
        let mut written_before = String::new();
        let mut route = |agent: &Self,
                         text: &mut String,
                         speech: Option<&mut StepSpeech>,
                         piece: String,
                         spoken: String| {
            if !written_calls.is_empty() || (text.is_empty() && piece.starts_with("[{")) {
                let raw = if marks_on { spoken } else { piece.clone() };
                if written_calls.is_empty() {
                    // Calls are never said, and neither is anything before them,
                    // unless they turn out to be no calls.
                    if let Some(s) = speech {
                        written_before = std::mem::take(&mut s.pending);
                    }
                    let at = raw.find("[{").unwrap_or(0);
                    written_before.push_str(&raw[..at]);
                    written_raw.push_str(&raw[at..]);
                } else {
                    written_raw.push_str(&raw);
                }
                written_calls.push_str(&piece);
            } else {
                agent.say(text, speech, piece, spoken);
            }
        };
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(AgentError::Cancelled),
                c = stream.next() => c,
            };
            if waiting_first && matches!(chunk, Some(Ok(_))) {
                waiting_first = false;
                let ms = asked.elapsed().as_millis() as u64;
                log::info!("first piece of the answer: ms={ms}");
                self.bus.publish(Event::Timing {
                    stage: "llm".into(),
                    ms,
                });
            }
            match chunk {
                None => {
                    let (shown, spoken) =
                        self.filter_reply(&mut tags, marks.as_mut(), thoughts.finish(), true);
                    route(self, &mut text, speech.as_mut(), shown, spoken);
                    break;
                }
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(LlmChunk::Text(t))) => {
                    raw_chars += t.chars().count();
                    raw.push_str(&t);
                    let (clean, spoken) =
                        self.filter_reply(&mut tags, marks.as_mut(), thoughts.push(&t), false);
                    route(self, &mut text, speech.as_mut(), clean, spoken);
                    let raw_loop = raw_chars > MAX_RAW || (t.contains('\n') && repeating(&raw));
                    if raw_loop || looping(&text) {
                        log::warn!(
                            "the model is looping, the reply is stopped: raw_chars={raw_chars} shown_chars={}",
                            text.chars().count()
                        );
                        return Err(AgentError::Looping);
                    }
                }
                Some(Ok(LlmChunk::ToolCall(c))) => calls.push(c),
            }
        }
        if !written_calls.is_empty() {
            // A direction in front of the calls spoils their raw copy; the
            // shown one has lost only what looked like marks.
            let found = self.calls_from_text(&written_raw).or_else(|| {
                marks_on
                    .then(|| self.calls_from_text(&written_calls))
                    .flatten()
            });
            match found {
                Some(found) if calls.is_empty() => calls = found,
                _ => self.say(
                    &mut text,
                    speech.as_mut(),
                    written_calls,
                    written_before + &written_raw,
                ),
            }
        } else if let Some(s) = speech.as_mut().filter(|s| !s.pending.trim().is_empty()) {
            // Marks after the last words, `Удачи! [laughs]`: no delta that
            // shows something carries them, so an empty one does.
            if !text.is_empty() {
                let pending = std::mem::take(&mut s.pending);
                s.said.push_str(&pending);
                self.bus.publish(Event::AssistantDelta {
                    text: String::new(),
                    spoken: Some(pending),
                });
            }
        }
        let spoken = speech.map(|s| (s.said + &s.pending).trim_end().to_string());
        Ok((text.trim_end().to_string(), spoken, calls))
    }

    /// Reply text after the reasoning filter, as shown and as spoken. Emotion
    /// tags go first, from both copies; with voice marks on, the directions
    /// they leave are then cut from the shown copy only. `end` closes both
    /// filters.
    fn filter_reply(
        &self,
        tags: &mut TagFilter,
        marks: Option<&mut MarkFilter>,
        text: String,
        end: bool,
    ) -> (String, String) {
        let (mut kept, cues) = tags.push(&text);
        // Before the text it stands in front of: the speaker must know in time.
        if tags.take_whole() {
            self.bus.publish(Event::SayWhole);
        }
        for emotion in cues {
            self.bus.publish(Event::EmotionCue { emotion });
        }
        if end {
            kept.push_str(&tags.finish());
        }
        self.unmark(marks, kept, end)
    }

    /// With voice marks on, splits reply text into what is shown and what is
    /// spoken, and turns the emotion words among the cut directions into cues.
    /// Without them the text is all shown. `end` closes the filter.
    fn unmark(&self, marks: Option<&mut MarkFilter>, text: String, end: bool) -> (String, String) {
        let Some(filter) = marks else {
            return (text, String::new());
        };
        let mut piece = filter.push(&text);
        if end {
            let last = filter.finish();
            piece.shown.push_str(&last.shown);
            piece.spoken.push_str(&last.spoken);
            piece.directions.extend(last.directions);
        }
        for emotion in piece.directions.iter().filter_map(|d| Emotion::from_tag(d)) {
            self.bus.publish(Event::EmotionCue { emotion });
        }
        (piece.shown, piece.spoken)
    }

    /// Tool calls that the model wrote into the reply text instead of the tool-call channel:
    /// `[{"name": "get_time", "arguments": {}}]`. Small models do this now and then.
    /// Only a whole reply of this shape with registered tool names counts; the calls
    /// still go through the gate like any other.
    fn calls_from_text(&self, text: &str) -> Option<Vec<ToolCall>> {
        let items: Vec<Value> = serde_json::from_str(text.trim()).ok()?;
        if items.is_empty() {
            return None;
        }
        items
            .into_iter()
            .enumerate()
            .map(|(i, item)| {
                let name = item.get("name")?.as_str()?;
                self.registry().get(name)?;
                let arguments = match item.get("arguments") {
                    None | Some(Value::Null) => Value::Object(Default::default()),
                    Some(v @ Value::Object(_)) => v.clone(),
                    Some(_) => return None,
                };
                Some(ToolCall {
                    id: format!("call_text_{i}"),
                    name: name.to_string(),
                    arguments,
                })
            })
            .collect()
    }

    /// Adds reply text that is already free of emotion tags and voice marks.
    /// With marks on, `speech` gets the spoken copy: it rides on the next
    /// delta that shows something.
    fn say(
        &self,
        text: &mut String,
        mut speech: Option<&mut StepSpeech>,
        piece: String,
        spoken: String,
    ) {
        if let Some(s) = speech.as_deref_mut() {
            s.pending.push_str(&spoken);
        }
        if piece.is_empty() {
            return;
        }
        if text.is_empty() {
            self.state.set(AssistantState::Speaking);
        }
        text.push_str(&piece);
        let spoken = speech.map(|s| {
            let now = std::mem::take(&mut s.pending);
            s.said.push_str(&now);
            now
        });
        self.bus.publish(Event::AssistantDelta {
            text: piece,
            spoken,
        });
    }

    /// Runs one tool call. Always returns text for the LLM, errors included.
    /// With voice marks on (`marked`), marks that leaked into its text for
    /// people are cut first. A call that gives the steps of a skill has
    /// them run next, each one through the gate (decision 0047).
    async fn execute(&self, call: &ToolCall, cancel: &CancellationToken, marked: bool) -> String {
        let done = self.run_call(call, cancel, marked, false).await;
        match done.replay {
            Some(replay) if done.ok => {
                let report = self.replay(call, replay, cancel).await;
                truncate(&format!("{}\n{report}", done.text), MAX_TOOL_RESULT)
            }
            _ => done.text,
        }
    }

    /// Runs the steps of a skill one by one, each as a call of its own:
    /// assessed, through the gate, shown in the chat, written to the
    /// journal. Stops at the first step that is refused or fails and tells
    /// the model which one and why, so it can go on by hand.
    async fn replay(&self, call: &ToolCall, replay: Replay, cancel: &CancellationToken) -> String {
        let total = replay.steps.len();
        let mut lines = Vec::with_capacity(total);
        let mut done = 0;
        let mut stopped_at = None;
        for (i, step) in replay.steps.iter().enumerate() {
            let n = i + 1;
            if cancel.is_cancelled() {
                stopped_at = Some(n);
                lines.push(format!("{n}. {} — остановлено пользователем", step.tool));
                break;
            }
            let step_call = ToolCall {
                id: format!("{}-step{n}", call.id),
                name: step.tool.clone(),
                arguments: step.args.clone(),
            };
            let result = self.run_call(&step_call, cancel, false, true).await;
            let output = truncate(&result.text, MAX_STEP_RESULT);
            if result.ok {
                done += 1;
                lines.push(format!("{n}. {} — готово: {output}", step.tool));
            } else {
                stopped_at = Some(n);
                lines.push(format!("{n}. {} — не выполнено: {output}", step.tool));
                break;
            }
        }
        let rest: Vec<String> = replay
            .steps
            .iter()
            .enumerate()
            .skip(stopped_at.unwrap_or(total))
            .map(|(i, s)| format!("{}. {}", i + 1, s.tool))
            .collect();
        let level = if stopped_at.is_some() {
            log::Level::Warn
        } else {
            log::Level::Info
        };
        log::log!(
            target: "tool",
            level,
            "skill run finished: {}skill={} steps={total} done={done} stopped_at={}",
            crate::journal::turn_field(),
            crate::journal::content(&replay.skill),
            stopped_at.map_or("-".to_string(), |n| n.to_string())
        );
        let mut report = format!(
            "Навык «{}»: выполнено шагов {done} из {total}.\n{}",
            replay.skill,
            lines.join("\n")
        );
        if !rest.is_empty() {
            report.push_str(&format!(
                "\nНе запускались: {}. Если нужно, доделай их сама по одному или скажи пользователю, что помешало.",
                rest.join(", ")
            ));
        }
        report
    }

    /// One call: assessment, gate, run, the chat rows and the journal
    /// record. `replayed` marks a step of a skill: it is not recorded for
    /// a skill again, and steps it would give are not run.
    async fn run_call(
        &self,
        call: &ToolCall,
        cancel: &CancellationToken,
        marked: bool,
        replayed: bool,
    ) -> Done {
        let started = Instant::now();
        let book = if replayed {
            None
        } else {
            self.skills.read().clone()
        };
        let fail = |text: String| {
            if let Some(b) = &book {
                b.note_failure();
            }
            Done::failed(text)
        };
        let Some(tool) = self.registry().get(&call.name) else {
            let msg = format!("Ошибка: инструмента `{}` нет.", call.name);
            let note = CallNote::new("unknown_tool");
            self.finish(call, false, &msg, None, started, &note, None);
            return fail(msg);
        };
        let args = match &call.arguments {
            Value::Null => Value::Object(Default::default()),
            v => v.clone(),
        };
        let args = if marked {
            unmark_args(&call.name, args)
        } else {
            args
        };
        // The arguments are the user's and the model's: written only while
        // texts are allowed, and never when they read like the blind zone or
        // when the blind zone refused the call.
        let content = crate::journal::content_allowed();
        let logged_args = content.then(|| crate::journal::guarded_if(true, &args.to_string()));
        let logged_args = logged_args.as_deref();
        let assessment = match tool.assess(&args) {
            Ok(a) => a,
            Err(ToolError::NotReady { message, data }) => {
                let msg = format!("Ошибка: {message}");
                let note = CallNote::new("not_ready").error(&message, content);
                self.finish(call, false, &msg, Some(data), started, &note, logged_args);
                return fail(msg);
            }
            Err(e) => {
                let msg = format!("Ошибка: {e}");
                let outcome = match e {
                    ToolError::BadArguments(_) => "bad_arguments",
                    _ => "error",
                };
                let note = CallNote::new(outcome).error(&e, content);
                self.finish(call, false, &msg, None, started, &note, logged_args);
                return fail(msg);
            }
        };
        let risk = assessment.level;
        self.bus.publish(Event::ToolStarted {
            id: call.id.clone(),
            name: call.name.clone(),
            summary: assessment.summary.clone(),
        });

        let mut stopped = false;
        let decision = tokio::select! {
            _ = cancel.cancelled() => {
                stopped = true;
                GateDecision::Denied("остановлено пользователем".into())
            }
            d = self.gate.check(&call.name, &args, &assessment) => d,
        };
        let approval = match decision {
            GateDecision::Approved(a) => a,
            GateDecision::Denied(reason) => {
                let msg = format!("Действие не выполнено: {reason}.");
                // Who refused is in the record of the gate, just before this one.
                let said = if stopped { "stopped" } else { "denied" };
                let note = CallNote::new(said).risk(risk).decision(said);
                self.finish(call, false, &msg, None, started, &note, logged_args);
                return fail(msg);
            }
        };
        let decided = CallNote::new("ok").risk(risk).decision(approval.how());
        log::info!(
            target: "tool",
            "{}",
            call_started_line(&call.name, &decided, crate::journal::turn())
        );
        self.state.set(AssistantState::Thinking);
        // "Stop" ends the wait for any tool, even one that does not watch its token.
        // Kept for the recording: `run` takes the arguments.
        let recorded_args = book.as_ref().map(|_| args.clone());
        let run = with_progress(
            call.id.clone(),
            self.bus.clone(),
            tool.run(args, approval, cancel.child_token()),
        );
        let mut replay = None;
        let (ok, text, data, note) = tokio::select! {
            _ = cancel.cancelled() => (
                false,
                "Остановлено пользователем.".to_string(),
                None,
                decided.outcome("stopped"),
            ),
            r = run => match r {
                Ok(out) => {
                    // A step never starts more steps: no skill runs a skill.
                    if !replayed {
                        replay = out.replay;
                    }
                    (true, out.text, out.data, decided)
                }
                Err(ToolError::NotReady { message, data }) => {
                    let note = decided.outcome("not_ready").error(&message, content);
                    (false, format!("Ошибка: {message}"), Some(data), note)
                }
                Err(ToolError::Cancelled) => (
                    false,
                    format!("Ошибка: {}", ToolError::Cancelled),
                    None,
                    decided.outcome("stopped"),
                ),
                Err(e) => {
                    let note = decided.outcome("error").error(&e, content);
                    (false, format!("Ошибка: {e}"), None, note)
                }
            },
        };
        self.finish(call, ok, &text, data, started, &note, logged_args);
        if let Some(b) = &book {
            match (ok, recorded_args) {
                (true, Some(args)) => b.record(&call.name, &args, is_read(&call.name, risk)),
                _ => b.note_failure(),
            }
        }
        Done {
            ok,
            text: truncate(&text, MAX_TOOL_RESULT),
            replay,
        }
    }

    /// Tells the chat that a call has ended and writes its record.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        call: &ToolCall,
        ok: bool,
        output: &str,
        data: Option<Value>,
        started: Instant,
        note: &CallNote,
        logged_args: Option<&str>,
    ) {
        let line = call_line(
            &call.name,
            note,
            started.elapsed().as_millis(),
            logged_args,
            crate::journal::turn(),
        );
        if note.expected() {
            log::info!(target: "tool", "{line}");
        } else {
            log::warn!(target: "tool", "{line}");
        }
        self.bus.publish(Event::ToolFinished {
            id: call.id.clone(),
            name: call.name.clone(),
            ok,
            output: truncate(output, MAX_TOOL_PREVIEW),
            elapsed_ms: started.elapsed().as_millis() as u64,
            data,
        });
    }
}

/// Text arguments meant for people, by tool. While voice marks are on, a mark
/// that leaked into one is cut before the call is shown, confirmed, stored or
/// sent (decision 0026). Paths, patterns, commands, code, file contents and
/// typed text are never touched: brackets there are data.
const PROSE_ARGS: [(&str, &[&str]); 7] = [
    ("messenger_send", &["text"]),
    ("reminder_set", &["text"]),
    ("memory_save", &["text"]),
    ("note_create", &["text", "title"]),
    ("todo_add", &["text"]),
    ("task_plan", &["title", "steps"]),
    ("task_step", &["note"]),
];

/// `args` of the tool `name` with voice marks cut from its text for people.
fn unmark_args(name: &str, mut args: Value) -> Value {
    let Some((_, fields)) = PROSE_ARGS.iter().find(|(tool, _)| *tool == name) else {
        return args;
    };
    let Value::Object(map) = &mut args else {
        return args;
    };
    let mut cut = false;
    for field in *fields {
        match map.get_mut(*field) {
            Some(Value::String(s)) => cut |= unmark_in_place(s),
            Some(Value::Array(items)) => {
                for item in items {
                    if let Value::String(s) = item {
                        cut |= unmark_in_place(s);
                    }
                }
            }
            _ => {}
        }
    }
    if cut {
        log::debug!("voice marks cut from the arguments: tool={name}");
    }
    args
}

/// Cuts voice marks out of `s`; true if it had any. Text without them is
/// left as it is, spacing included.
fn unmark_in_place(s: &mut String) -> bool {
    let marked = crate::marks::tokens(s)
        .iter()
        .any(|t| !matches!(t, crate::marks::Token::Text(_)));
    if marked {
        *s = crate::marks::strip_marks(s);
    }
    marked
}

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_of_a_turn_has_its_size_and_never_its_text() {
        let stats = TurnStats { steps: 3, tools: 2 };
        let done = Ok(("тайный ответ".to_string(), None));
        assert_eq!(
            turn_line(&done, stats, 1500, None, false),
            (
                log::Level::Info,
                "turn finished: outcome=ok ms=1500 steps=3 tools=2 reply_chars=12".to_string()
            )
        );
        // The number of the turn ties the record to those of its requests
        // and tool calls; the reply joins it only while texts are written.
        assert_eq!(
            turn_line(&done, stats, 1500, Some(7), true).1,
            "turn finished: turn=7 outcome=ok ms=1500 steps=3 tools=2 reply_chars=12 reply=тайный ответ"
        );
        let blind = Ok(("Открой окно «Вход в банк»".to_string(), None));
        let (_, line) = turn_line(&blind, stats, 1, Some(7), true);
        assert!(!line.contains("банк"), "{line}");
        assert_eq!(
            turn_line(&Err(AgentError::Cancelled), stats, 7, Some(8), true),
            (
                log::Level::Info,
                "turn finished: turn=8 outcome=stopped ms=7 steps=3 tools=2".to_string()
            )
        );
        let refused = Err(AgentError::Llm(LlmError::Http {
            status: 401,
            body: "invalid api key".into(),
        }));
        assert_eq!(
            turn_line(&refused, stats, 90, None, false),
            (
                log::Level::Error,
                "turn finished: outcome=error kind=http ms=90 steps=3 tools=2 http=401 error=invalid api key"
                    .to_string()
            )
        );
        // What the model sent stays out while texts are off; what the
        // parser said about it is written.
        let broken = Err(AgentError::Llm(LlmError::BadJson {
            why: "expected value at line 1 column 1".into(),
            data: "привет".into(),
        }));
        let (level, line) = turn_line(&broken, stats, 1, None, false);
        assert_eq!(level, log::Level::Error);
        assert!(line.contains("kind=bad_json"), "{line}");
        assert!(
            line.ends_with("error=expected value at line 1 column 1 data=<hidden 6 chars>"),
            "{line}"
        );
        // What the provider says about its own refusal is written, in the
        // answer as in the status.
        let overloaded = Err(AgentError::Llm(LlmError::Provider(
            "{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}".into(),
        )));
        let (_, line) = turn_line(&overloaded, stats, 1, Some(3), false);
        assert!(
            line.starts_with("turn finished: turn=3 outcome=error kind=provider_error "),
            "{line}"
        );
        assert!(line.ends_with("\"message\":\"Overloaded\"}"), "{line}");
        let empty = Err(AgentError::Llm(LlmError::Protocol("пустой ответ".into())));
        let (_, line) = turn_line(&empty, stats, 1, None, false);
        assert!(
            line.ends_with("kind=protocol ms=1 steps=3 tools=2 error=пустой ответ"),
            "{line}"
        );
        let (_, line) = turn_line(&Err(AgentError::TooManySteps), stats, 1, None, false);
        assert!(line.contains("kind=too_many_steps"), "{line}");
        let (_, line) = turn_line(&Err(AgentError::Looping), stats, 1, None, false);
        assert!(line.contains("kind=looping"), "{line}");
    }

    #[test]
    fn the_record_of_a_tool_call_names_risk_decision_time_and_outcome() {
        let ok = CallNote::new("ok")
            .risk(RiskLevel::Medium)
            .decision("asked_yes");
        assert!(ok.expected());
        assert_eq!(
            call_line("av_convert", &ok, 5321, None, None),
            "tool call finished: tool=av_convert risk=medium decision=asked_yes ms=5321 outcome=ok"
        );
        assert_eq!(
            call_started_line("av_convert", &ok, Some(4)),
            "tool call started: turn=4 tool=av_convert risk=medium decision=asked_yes"
        );
        // Arguments join the record only when the caller gives them: while
        // texts are written.
        assert_eq!(
            call_line("echo", &ok, 1, Some("{\"text\":\"x\"}"), Some(4)),
            "tool call finished: turn=4 tool=echo risk=medium decision=asked_yes ms=1 outcome=ok args={\"text\":\"x\"}"
        );
        let failed = CallNote::new("error")
            .risk(RiskLevel::Low)
            .decision("rule")
            .error(&ToolError::Failed("диск полон".into()), true);
        assert!(!failed.expected());
        assert_eq!(
            call_line("note_create", &failed, 12, None, None),
            "tool call finished: tool=note_create risk=low decision=rule ms=12 outcome=error error=диск полон"
        );
        // A call that never reached its assessment has no risk and no decision.
        let unknown = CallNote::new("unknown_tool");
        let name = "x".repeat(200);
        let line = call_line(&name, &unknown, 0, None, None);
        assert_eq!(
            line,
            format!(
                "tool call finished: tool={} risk=- decision=- ms=0 outcome=unknown_tool",
                "x".repeat(64)
            )
        );
        for outcome in ["denied", "stopped"] {
            assert!(CallNote::new(outcome).expected(), "{outcome}");
        }
        for outcome in ["error", "bad_arguments", "not_ready", "unknown_tool"] {
            assert!(!CallNote::new(outcome).expected(), "{outcome}");
        }
    }

    #[test]
    fn the_error_of_a_tool_is_not_written_while_texts_are_off() {
        // What `ui_*` tools say when the window is not found: the titles
        // of every open window.
        let windows = ToolError::Failed(
            "нет окна «отчёт». Открытые окна: «Письмо Ане — Почта» (mail), «Зарплата.xlsx» (excel)"
                .into(),
        );
        let off = CallNote::new("error")
            .risk(RiskLevel::Medium)
            .decision("rule")
            .error(&windows, false);
        let line = call_line("ui_click", &off, 3, None, Some(2));
        assert_eq!(
            line,
            "tool call finished: turn=2 tool=ui_click risk=medium decision=rule ms=3 outcome=error error=<hidden 85 chars>"
        );
        for part in ["Письмо", "Зарплата", "отчёт", "mail"] {
            assert!(!line.contains(part), "{line}");
        }
        // The kind of the error is the outcome, and that is always written.
        let bad = CallNote::new("bad_arguments")
            .error(&ToolError::BadArguments("нет поля `path`".into()), false);
        assert_eq!(
            call_line("file_read", &bad, 0, None, None),
            "tool call finished: tool=file_read risk=- decision=- ms=0 outcome=bad_arguments error=<hidden 35 chars>"
        );
        // With texts on the text is there.
        let on = CallNote::new("error").error(&windows, true);
        assert!(on.error.as_deref().is_some_and(|e| e.contains("Зарплата")));
    }

    #[test]
    fn a_call_the_blind_zone_refused_leaves_neither_its_error_nor_its_arguments() {
        for content in [false, true] {
            let refused = CallNote::new("error")
                .risk(RiskLevel::Medium)
                .decision("rule")
                .error(
                    &ToolError::Failed(crate::policy::blind::BLIND_REFUSAL.to_string()),
                    content,
                );
            assert_eq!(refused.outcome, "blind_zone");
            assert_eq!(refused.error, None);
            assert!(!refused.expected());
            // What was to be typed into that window is in the arguments.
            let args = content.then_some("{\"text\":\"zq-typed-into-the-window\"}");
            let line = call_line("ui_type", &refused, 5, args, None);
            assert!(!line.contains("zq-typed"), "{line}");
            assert!(!line.contains("error="), "{line}");
            assert!(line.contains("outcome=blind_zone"), "{line}");
            assert_eq!(line.contains("args=<blind zone>"), content, "{line}");
        }
        // A text that only has a word of the blind zone in it is an error
        // like any other, and its text is never written.
        for content in [false, true] {
            let titled = CallNote::new("error").error(
                &ToolError::Failed("окно «Вход в банк — Chrome» не найдено".into()),
                content,
            );
            assert_eq!(titled.outcome, "error");
            let error = titled.error.unwrap();
            assert!(error.starts_with("<hidden"), "{error}");
            assert!(!error.contains("банк"), "{error}");
        }
    }

    #[test]
    fn a_reply_going_round_in_circles_is_caught() {
        let circle = "Wait, I'll call task_plan.

Actually, I'll call generate_image.

"
        .repeat(LOOP_LINES / 2 + 1);
        assert!(looping(&format!(
            "Начинаю.
{circle}"
        )));
        let list: String = (1..=12)
            .map(|i| {
                format!(
                    "{i}. пункт номер {i}
"
                )
            })
            .collect();
        assert!(!looping(&list));
        assert!(!looping(
            "Да.
Да.
Да."
        ));
        assert!(looping(&"а".repeat(MAX_REPLY + 1)));
    }

    #[test]
    fn lines_repeated_on_purpose_are_not_a_loop() {
        let replies = [
            format!("| День | Дело |\n|---|---|\n{}", "|  |  |\n".repeat(40)),
            "- [ ]\n".repeat(40),
            format!("```\n{}```\n", "# # # #\n # # # #\n".repeat(20)),
            format!("```\n{}```\n", "x = 1\n".repeat(40)),
            "# # # #\n # # # #\n".repeat(20),
            "Я молодец!\n".repeat(20),
            format!("Вот таблица:\n{}Заполняй.\n", "|  |  |\n".repeat(12)),
        ];
        for reply in replies {
            assert!(!looping(&reply), "{reply}");
        }
    }

    #[test]
    fn an_unfinished_last_line_does_not_count() {
        let lines = "Wait, I'll call task_plan.\n".repeat(LOOP_LINES - 1);
        assert!(!repeating(&format!("{lines}Wait, I'll call task_plan.")));
        assert!(repeating(&format!("{lines}Wait, I'll call task_plan.\n")));
    }
    use crate::gate::LowRiskOnlyGate;
    use crate::llm::{LlmStream, Role};
    use crate::tools::{EchoTool, TimeTool};
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::VecDeque;
    use tokio::sync::broadcast::Receiver;

    type Script = Vec<Result<LlmChunk, LlmError>>;

    /// Plays scripted answers; `None` in the queue means a stream that never ends.
    struct FakeProvider {
        scripts: Mutex<VecDeque<Option<Script>>>,
        requests: Mutex<Vec<LlmRequest>>,
    }

    impl FakeProvider {
        fn new(scripts: Vec<Option<Script>>) -> Arc<Self> {
            Arc::new(Self {
                scripts: Mutex::new(scripts.into()),
                requests: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        async fn stream(&self, req: LlmRequest) -> Result<LlmStream, LlmError> {
            self.requests.lock().push(req);
            match self.scripts.lock().pop_front() {
                Some(Some(script)) => Ok(futures::stream::iter(script).boxed()),
                Some(None) => Ok(futures::stream::iter(vec![Ok(LlmChunk::Text("…".into()))])
                    .chain(futures::stream::pending())
                    .boxed()),
                None => Err(LlmError::Network("script empty".into())),
            }
        }
    }

    fn text(t: &str) -> Result<LlmChunk, LlmError> {
        Ok(LlmChunk::Text(t.into()))
    }

    fn call(id: &str, name: &str, args: Value) -> Result<LlmChunk, LlmError> {
        Ok(LlmChunk::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: args,
        }))
    }

    fn agent(p: Arc<FakeProvider>) -> (Agent, Receiver<Event>, StateHandle) {
        let bus = EventBus::new(256);
        let rx = bus.subscribe();
        let state = StateHandle::new(bus.clone());
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TimeTool));
        reg.register(Arc::new(EchoTool));
        let a = Agent::new(
            p,
            Arc::new(reg),
            Arc::new(LowRiskOnlyGate),
            bus,
            state.clone(),
            "sys".into(),
        );
        (a, rx, state)
    }

    fn drain(rx: &mut Receiver<Event>) -> Vec<Event> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn tool_results(req: &LlmRequest) -> Vec<String> {
        req.messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| m.content.clone())
            .collect()
    }

    #[tokio::test]
    async fn plain_answer_streams_deltas_and_final_text() {
        let p = FakeProvider::new(vec![Some(vec![text("При"), text("вет")])]);
        let (a, mut rx, state) = agent(p.clone());
        assert_eq!(
            a.handle("здравствуй", CancellationToken::new()).await,
            Ok("Привет".into())
        );
        let ev = drain(&mut rx);
        assert_eq!(
            ev[0],
            Event::UserText {
                text: "здравствуй".into()
            }
        );
        assert!(ev.contains(&Event::AssistantDelta {
            text: "При".into(),
            spoken: None,
        }));
        assert!(ev.contains(&Event::AssistantText {
            text: "Привет".into(),
            spoken: None,
        }));
        assert_eq!(state.get(), AssistantState::Idle);
        assert_eq!(a.history().len(), 2);
        let req = &p.requests.lock()[0];
        assert_eq!(req.messages[0], ChatMessage::system("sys"));
        assert_eq!(req.tools.len(), 2);
    }

    #[tokio::test]
    async fn tool_call_runs_tool_and_feeds_result_back() {
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "echo", json!({"text": "пинг"}))]),
            Some(vec![text("Готово")]),
        ]);
        let (a, mut rx, _) = agent(p.clone());
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Готово".into())
        );
        assert_eq!(tool_results(&p.requests.lock()[1]), vec!["пинг"]);
        let ev = drain(&mut rx);
        assert!(ev
            .iter()
            .any(|e| matches!(e, Event::ToolStarted { name, .. } if name == "echo")));
        assert!(ev.iter().any(
            |e| matches!(e, Event::ToolFinished { ok: true, output, .. } if output == "пинг")
        ));
        // user, assistant with call, tool result, final answer
        assert_eq!(a.history().len(), 4);
    }

    #[tokio::test]
    async fn unknown_tool_returns_error_to_llm() {
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "rm_rf", json!({}))]),
            Some(vec![text("Не вышло")]),
        ]);
        let (a, _rx, _) = agent(p.clone());
        a.handle("x", CancellationToken::new()).await.unwrap();
        assert!(tool_results(&p.requests.lock()[1])[0].contains("инструмента `rm_rf` нет"));
    }

    #[tokio::test]
    async fn bad_arguments_return_error_to_llm() {
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "echo", Value::String("{broken".into()))]),
            Some(vec![text("ок")]),
        ]);
        let (a, _rx, _) = agent(p.clone());
        a.handle("x", CancellationToken::new()).await.unwrap();
        assert!(tool_results(&p.requests.lock()[1])[0].contains("неверные аргументы"));
    }

    #[tokio::test]
    async fn a_loop_inside_an_unclosed_thought_block_is_stopped() {
        let mut script = vec![text("<|channel>thought\n")];
        for _ in 0..LOOP_LINES {
            script.push(text("Wait, I'll call task_plan.\n"));
            script.push(text("Actually, I'll call generate_image.\n"));
        }
        let (a, _rx, _) = agent(FakeProvider::new(vec![Some(script)]));
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Err(AgentError::Looping)
        );
    }

    #[tokio::test]
    async fn an_endless_answer_out_of_sight_is_stopped() {
        // Calls written as text are held back from the reply until the stream ends.
        let calls: Script =
            std::iter::once(text("[{\"name\": \"echo\", \"arguments\": {\"text\": \""))
                .chain((0..MAX_RAW / 10).map(|i| text(&format!("слово {i:>4} "))))
                .collect();
        // So is an unclosed thought block, line after different line.
        let thinking: Script = std::iter::once(text("<think>"))
            .chain((0..MAX_RAW / 100).map(|i| text(&format!("{i:>4}: {}\n", "думаю ".repeat(16)))))
            .collect();
        for script in [calls, thinking] {
            let (a, _rx, _) = agent(FakeProvider::new(vec![Some(script)]));
            assert_eq!(
                a.handle("x", CancellationToken::new()).await,
                Err(AgentError::Looping)
            );
        }
    }

    #[tokio::test]
    async fn long_reasoning_before_an_answer_is_not_a_loop() {
        let script: Script = std::iter::once(text("<think>"))
            .chain((0..200).map(|i| text(&format!("Шаг {i}: проверяю вариант {i}.\n"))))
            .chain([text("</think>Ответ: 42.")])
            .collect();
        let (a, _rx, _) = agent(FakeProvider::new(vec![Some(script)]));
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Ответ: 42.".into())
        );
    }

    struct DenyAll;
    #[async_trait]
    impl Gate for DenyAll {
        async fn check(&self, _: &str, _: &Value, _: &crate::tools::Assessment) -> GateDecision {
            GateDecision::Denied("пользователь отклонил".into())
        }
    }

    #[tokio::test]
    async fn denied_tool_is_reported_to_llm() {
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "get_time", json!({}))]),
            Some(vec![text("Поняла")]),
        ]);
        let bus = EventBus::new(64);
        let state = StateHandle::new(bus.clone());
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TimeTool));
        let a = Agent::new(
            p.clone(),
            Arc::new(reg),
            Arc::new(DenyAll),
            bus,
            state,
            "s".into(),
        );
        a.handle("x", CancellationToken::new()).await.unwrap();
        assert!(tool_results(&p.requests.lock()[1])[0].contains("пользователь отклонил"));
    }

    #[tokio::test]
    async fn stops_after_max_steps() {
        let scripts = (0..MAX_STEPS + 2)
            .map(|i| Some(vec![call(&format!("c{i}"), "get_time", json!({}))]))
            .collect();
        let p = FakeProvider::new(scripts);
        let (a, mut rx, state) = agent(p.clone());
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Err(AgentError::TooManySteps)
        );
        assert_eq!(p.requests.lock().len(), MAX_STEPS);
        assert!(drain(&mut rx)
            .iter()
            .any(|e| matches!(e, Event::Error { .. })));
        assert_eq!(state.get(), AssistantState::Idle);
        assert_eq!(a.history().last().unwrap().role, Role::Assistant);
    }

    #[tokio::test]
    async fn planned_task_runs_past_the_plain_limit_and_closes() {
        use crate::task::{StepStatus, TaskPlanTool, TaskStepTool};
        let steps: Vec<String> = (1..=6).map(|i| format!("шаг {i}")).collect();
        let mut scripts = vec![Some(vec![call(
            "p",
            "task_plan",
            json!({"title": "длинная", "steps": steps}),
        )])];
        for i in 1..=6 {
            scripts.push(Some(vec![call(
                &format!("a{i}"),
                "task_step",
                json!({"step": i, "status": "active"}),
            )]));
            scripts.push(Some(vec![call(
                &format!("d{i}"),
                "task_step",
                json!({"step": i, "status": "done"}),
            )]));
        }
        scripts.push(Some(vec![text("Всё сделала")]));
        let p = FakeProvider::new(scripts);
        let bus = EventBus::new(256);
        let mut rx = bus.subscribe();
        let state = StateHandle::new(bus.clone());
        let board = TaskBoard::new(bus.clone());
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TaskPlanTool(board.clone())));
        reg.register(Arc::new(TaskStepTool(board.clone())));
        let a = Agent::new(
            p.clone(),
            Arc::new(reg),
            Arc::new(LowRiskOnlyGate),
            bus,
            state,
            "s".into(),
        );
        a.set_task_board(board.clone());
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Всё сделала".into())
        );
        assert_eq!(p.requests.lock().len(), 14);
        let t = board.get().unwrap();
        assert!(t.finished && t.ok);
        assert!(t.steps.iter().all(|s| s.status == StepStatus::Done));
        assert!(drain(&mut rx)
            .iter()
            .any(|e| matches!(e, Event::TaskUpdated { task } if task.finished)));
    }

    #[tokio::test]
    async fn open_steps_get_a_reminder_then_the_reply_ends() {
        use crate::task::{TaskPlanTool, TaskStepTool};
        let p = FakeProvider::new(vec![
            Some(vec![call(
                "p",
                "task_plan",
                json!({"title": "т", "steps": ["a", "b"]}),
            )]),
            Some(vec![text("Начала.")]),
            Some(vec![call(
                "d1",
                "task_step",
                json!({"step": 1, "status": "done"}),
            )]),
            Some(vec![text("Ещё чуть.")]),
            Some(vec![text("Всё, дальше не могу.")]),
        ]);
        let bus = EventBus::new(256);
        let state = StateHandle::new(bus.clone());
        let board = TaskBoard::new(bus.clone());
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TaskPlanTool(board.clone())));
        reg.register(Arc::new(TaskStepTool(board.clone())));
        let a = Agent::new(
            p.clone(),
            Arc::new(reg),
            Arc::new(LowRiskOnlyGate),
            bus,
            state,
            "s".into(),
        );
        a.set_task_board(board.clone());
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Всё, дальше не могу.".into())
        );
        let reqs = p.requests.lock();
        assert_eq!(reqs.len(), 5);
        let last_user = reqs[2]
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .unwrap();
        assert!(last_user.content.contains("не закрыты шаги 1, 2"));
        let last_user = reqs[4]
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .unwrap();
        assert!(last_user.content.contains("не закрыты шаги 2"));
        let t = board.get().unwrap();
        assert!(t.finished && !t.ok, "step 2 never ran");
    }

    /// A tool that never ends and ignores its token.
    struct Stuck;

    #[async_trait]
    impl crate::tools::Tool for Stuck {
        fn spec(&self) -> crate::llm::ToolSpec {
            crate::tools::spec("stuck", "never ends", json!({"type": "object"}))
        }
        fn assess(&self, _: &Value) -> Result<crate::tools::Assessment, crate::tools::ToolError> {
            Ok(crate::tools::Assessment::new(
                crate::tools::RiskLevel::Low,
                "s",
                "t",
                "r",
            ))
        }
        async fn run(
            &self,
            _: Value,
            _: crate::gate::Approval,
            _: CancellationToken,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn stop_ends_a_tool_that_ignores_its_token() {
        let p = FakeProvider::new(vec![Some(vec![call("c1", "stuck", json!({}))])]);
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let state = StateHandle::new(bus.clone());
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(Stuck));
        let a = Agent::new(
            p,
            Arc::new(reg),
            Arc::new(LowRiskOnlyGate),
            bus,
            state.clone(),
            "s".into(),
        );
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            c2.cancel();
        });
        let r = tokio::time::timeout(std::time::Duration::from_secs(2), a.handle("x", cancel))
            .await
            .expect("stop must end the turn");
        assert_eq!(r, Err(AgentError::Cancelled));
        assert_eq!(state.get(), AssistantState::Idle);
        assert!(drain(&mut rx).iter().any(
            |e| matches!(e, Event::ToolFinished { ok: false, output, .. } if output.contains("Остановлено"))
        ));
    }

    #[tokio::test]
    async fn cancel_stops_stream_and_returns_to_idle() {
        let p = FakeProvider::new(vec![None]);
        let (a, mut rx, state) = agent(p);
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            c2.cancel();
        });
        let started = Instant::now();
        assert_eq!(a.handle("x", cancel).await, Err(AgentError::Cancelled));
        assert!(started.elapsed().as_millis() < 100);
        assert_eq!(state.get(), AssistantState::Idle);
        assert!(!drain(&mut rx)
            .iter()
            .any(|e| matches!(e, Event::Error { .. })));
    }

    #[tokio::test]
    async fn provider_error_publishes_error_and_keeps_chat_usable() {
        let p = FakeProvider::new(vec![
            Some(vec![text("Нача"), Err(LlmError::Network("обрыв".into()))]),
            Some(vec![text("Снова тут")]),
        ]);
        let (a, mut rx, state) = agent(p);
        assert!(matches!(
            a.handle("1", CancellationToken::new()).await,
            Err(AgentError::Llm(LlmError::Network(_)))
        ));
        assert!(drain(&mut rx)
            .iter()
            .any(|e| matches!(e, Event::Error { message } if message.contains("обрыв"))));
        assert_eq!(state.get(), AssistantState::Idle);
        assert_eq!(a.history(), vec![ChatMessage::user("1")]);
        assert_eq!(
            a.handle("2", CancellationToken::new()).await,
            Ok("Снова тут".into())
        );
    }

    #[test]
    fn old_history_is_trimmed_at_user_boundary() {
        let long = "x".repeat(100);
        let h = vec![
            ChatMessage::user("старый вопрос"),
            ChatMessage::assistant_with_calls(
                "",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "echo".into(),
                    arguments: json!({}),
                }],
            ),
            ChatMessage::tool_result("c1", long.clone()),
            ChatMessage::assistant("старый ответ"),
            ChatMessage::user("новый вопрос"),
            ChatMessage::assistant("новый ответ"),
        ];
        // Budget too small for the old turn: it goes whole, never half.
        let t = trim_history(&h, 60);
        assert_eq!(t[0], ChatMessage::user("новый вопрос"));
        assert_eq!(t.len(), 2);
        // Big budget keeps everything.
        assert_eq!(trim_history(&h, 10_000).len(), 6);
        // Budget smaller than the last turn sends no history at all.
        assert!(trim_history(&h, 5).is_empty());
        assert!(trim_history(&[], 10).is_empty());
    }

    #[tokio::test]
    async fn store_persists_exchange() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let p = FakeProvider::new(vec![Some(vec![text("ок")])]);
        let (a, _rx, _) = agent(p);
        let a = a.with_store(db.clone(), "main");
        a.handle("hi", CancellationToken::new()).await.unwrap();
        assert_eq!(db.load_messages("main").unwrap().len(), 2);
    }

    fn filter(chunks: &[&str]) -> (String, Vec<Emotion>) {
        let mut f = TagFilter::default();
        let mut text = String::new();
        let mut cues = Vec::new();
        for c in chunks {
            let (t, e) = f.push(c);
            text.push_str(&t);
            cues.extend(e);
        }
        text.push_str(&f.finish());
        (text, cues)
    }

    #[test]
    fn tag_filter_strips_tag_at_start() {
        assert_eq!(
            filter(&["[joy] Привет!"]),
            ("Привет!".into(), vec![Emotion::Joy])
        );
        assert_eq!(
            filter(&["[Радость]\nПривет!"]),
            ("Привет!".into(), vec![Emotion::Joy])
        );
    }

    #[test]
    fn tag_filter_handles_tag_split_across_chunks() {
        assert_eq!(
            filter(&["[jo", "y] При", "вет"]),
            ("Привет".into(), vec![Emotion::Joy])
        );
        assert_eq!(
            filter(&["[", "wink", "]", " ", "Ха"]),
            ("Ха".into(), vec![Emotion::Wink])
        );
    }

    #[test]
    fn tag_filter_drops_invented_word_tags_at_the_start_only() {
        let mut f = TagFilter::default();
        let (out, cues) = f.push("[smile] Проводник открыла. [x] и [note] видны.");
        assert_eq!(out, "Проводник открыла. [x] и [note] видны.");
        assert_eq!(cues, vec![Emotion::Joy]);
        let mut f = TagFilter::default();
        let (out, cues) = f.push("[sparkle] Готово.");
        assert_eq!(out, "Готово.");
        assert!(cues.is_empty());
        let mut f = TagFilter::default();
        assert_eq!(f.push("[Sparkle] Привет").0, "[Sparkle] Привет");
    }

    #[test]
    fn the_whole_tag_is_cut_at_the_start_and_noticed_once() {
        for (reply, cues) in [
            ("[aloud] Жили-были дед да баба.", vec![]),
            ("[joy] [aloud] Жили-были дед да баба.", vec![Emotion::Joy]),
            ("[aloud][wink]\nЖили-были дед да баба.", vec![Emotion::Wink]),
            ("[Вслух] Жили-были дед да баба.", vec![]),
        ] {
            let mut f = TagFilter::default();
            let (out, found) = f.push(reply);
            assert_eq!(out, "Жили-были дед да баба.", "{reply}");
            assert_eq!(found, cues, "{reply}");
            assert!(f.take_whole(), "{reply}");
            assert!(!f.take_whole(), "{reply}");
        }
        // With voice marks on, a direction stays for the mark filter, and the
        // tag is cut on either side of it: the voice never gets `[aloud]`.
        for (reply, kept) in [
            ("[aloud] Жили-были.", "Жили-были."),
            ("[joy] [aloud] Жили-были.", "[joy] Жили-были."),
            ("[aloud] [softly] Жили-были.", "[softly] Жили-были."),
        ] {
            let mut f = TagFilter::keeping_directions();
            assert_eq!(f.push(reply).0, kept, "{reply}");
            assert!(f.take_whole(), "{reply}");
        }
        let mut f = TagFilter::keeping_directions();
        assert_eq!(
            f.push("[softly] Жили-были. [aloud] Вот.").0,
            "[softly] Жили-были. [aloud] Вот."
        );
        assert!(!f.take_whole());
        // Split across chunks.
        let mut f = TagFilter::default();
        assert_eq!(f.push("[alo").0, "");
        assert!(!f.take_whole());
        assert_eq!(f.push("ud] Раз.").0, "Раз.");
        assert!(f.take_whole());
        // Only at the very start: later it is text like any other.
        let mut f = TagFilter::default();
        assert_eq!(f.push("Скажу [aloud] потом.").0, "Скажу [aloud] потом.");
        assert!(!f.take_whole());
        // No tag, no request.
        let mut f = TagFilter::default();
        f.push("[joy] Привет!");
        assert!(!f.take_whole());
    }

    #[tokio::test]
    async fn the_whole_tag_reaches_the_speaker_before_the_text() {
        let p = FakeProvider::new(vec![Some(vec![
            text("[alo"),
            text("ud] Жили"),
            text("-были."),
        ])]);
        let (a, mut rx, _) = agent(p);
        assert_eq!(
            a.handle("расскажи сказку", CancellationToken::new()).await,
            Ok("Жили-были.".into())
        );
        let ev = drain(&mut rx);
        let whole = ev.iter().position(|e| *e == Event::SayWhole).unwrap();
        let first = ev
            .iter()
            .position(|e| matches!(e, Event::AssistantDelta { .. }))
            .unwrap();
        assert!(whole < first, "{ev:?}");
        assert_eq!(ev.iter().filter(|e| **e == Event::SayWhole).count(), 1);

        // A reply without the tag asks for nothing.
        let p = FakeProvider::new(vec![Some(vec![text("[joy] Привет")])]);
        let (a, mut rx, _) = agent(p);
        a.handle("x", CancellationToken::new()).await.unwrap();
        assert!(!drain(&mut rx).contains(&Event::SayWhole));
    }

    #[test]
    fn the_prompt_tells_her_how_to_be_read_whole() {
        let p = system_prompt(&AssistantConfig::default());
        assert!(p.contains("[aloud]"), "{p}");
        assert!(p.contains("рассказывай полностью, а не описывай"), "{p}");
    }

    #[test]
    fn promises_without_actions_are_noticed() {
        assert!(promises_action(
            "Сейчас я попробую его перезапустить. Подожди немножко."
        ));
        assert!(promises_action(
            "Как только найду, отправлю. Подождёшь секунду?"
        ));
        assert!(!promises_action(
            "Готово! Хочешь, чтобы я прочитала, что там написано?"
        ));
        assert!(!promises_action("В Перми сейчас пасмурно."));
    }

    #[tokio::test]
    async fn a_promise_or_silence_after_tools_gets_a_reminder() {
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "echo", json!({"text": "a"}))]),
            Some(vec![]),
            Some(vec![text("Нашла Алексея, подожди секунду.")]),
            Some(vec![text("Отправила «Привет».")]),
        ]);
        let (a, _rx, _) = agent(p.clone());
        assert_eq!(
            a.handle("напиши", CancellationToken::new()).await,
            Ok("Отправила «Привет».".into())
        );
        let reqs = p.requests.lock();
        assert_eq!(reqs.len(), 4);
        for r in &reqs[2..] {
            let last = r.messages.last().unwrap();
            assert_eq!(last.role, Role::User);
            assert!(last.content.contains("не доделав"));
        }
    }

    #[tokio::test]
    async fn a_promise_after_work_went_to_the_background_is_an_answer() {
        let started = format!(
            "{} задание №1 «Сжать «отпуск.mp4»».",
            crate::bgjobs::STARTED_PREFIX
        );
        let reply = "Запустила в фоне, подожди немного: скажу, когда закончится.";
        assert!(promises_action(reply));
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "echo", json!({ "text": started }))]),
            Some(vec![text(reply)]),
        ]);
        let (a, _rx, _) = agent(p.clone());
        assert_eq!(
            a.handle("сожми", CancellationToken::new()).await,
            Ok(reply.into())
        );
        assert_eq!(p.requests.lock().len(), 2, "no reminder was sent");

        // Silence is still no answer, and the next message starts anew.
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "echo", json!({ "text": started }))]),
            Some(vec![]),
            Some(vec![text("Запустила в фоне.")]),
            Some(vec![text("Подожди секунду.")]),
            Some(vec![text("Ничего не делаю.")]),
        ]);
        let (a, _rx, _) = agent(p.clone());
        assert_eq!(
            a.handle("сожми", CancellationToken::new()).await,
            Ok("Запустила в фоне.".into())
        );
        assert_eq!(p.requests.lock().len(), 3);
        assert_eq!(
            a.handle("а теперь?", CancellationToken::new()).await,
            Ok("Ничего не делаю.".into())
        );
        let reqs = p.requests.lock();
        assert_eq!(reqs.len(), 5);
        assert!(reqs[4]
            .messages
            .last()
            .unwrap()
            .content
            .contains("не доделав"));
    }

    #[test]
    fn tag_filter_drops_invented_emoji_tags() {
        assert_eq!(filter(&["[🤔] Шаг 1"]), ("Шаг 1".into(), vec![]));
        assert_eq!(filter(&["Готово [😊😊]!"]), ("Готово !".into(), vec![]));
    }

    #[test]
    fn tag_filter_keeps_ordinary_brackets() {
        for text in [
            "Смотри [1] и [файл.txt].",
            "[ ] задача и [x] сделано",
            "[—]",
            "массив[0] = [a, b]",
            "[[joy",
            "[очень длинный текст в скобках, не метка] дальше",
        ] {
            assert_eq!(filter(&[text]), (text.to_string(), vec![]), "{text}");
        }
    }

    #[test]
    fn tag_filter_flushes_unclosed_bracket_on_finish() {
        assert_eq!(filter(&["итог: [jo"]), ("итог: [jo".into(), vec![]));
    }

    #[test]
    fn tag_filter_strips_tag_in_the_middle() {
        assert_eq!(
            filter(&["Готово. [wink] Ещё что-то?"]),
            ("Готово. Ещё что-то?".into(), vec![Emotion::Wink])
        );
    }

    #[tokio::test]
    async fn reply_tag_becomes_cue_and_is_not_stored() {
        let p = FakeProvider::new(vec![Some(vec![text("[jo"), text("y] При"), text("вет")])]);
        let (a, mut rx, _) = agent(p);
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Привет".into())
        );
        let ev = drain(&mut rx);
        assert!(ev.contains(&Event::EmotionCue {
            emotion: Emotion::Joy
        }));
        assert!(ev.contains(&Event::AssistantText {
            text: "Привет".into(),
            spoken: None,
        }));
        let deltas: String = ev
            .iter()
            .filter_map(|e| match e {
                Event::AssistantDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, "Привет");
        assert_eq!(a.history()[1].content, "Привет");
    }

    #[test]
    fn system_prompt_mentions_persona_and_tags() {
        let mut cfg = AssistantConfig::default();
        let p = system_prompt(&cfg);
        assert!(p.starts_with("Ты Мика,"));
        assert!(p.contains("заботливая"));
        assert!(p.contains("[joy]") && p.contains("[worry]"));

        cfg.name = "Айри".into();
        cfg.persona = crate::config::Persona::Custom;
        cfg.custom_prompt = "  Говоришь как пират.  ".into();
        let p = system_prompt(&cfg);
        assert!(p.starts_with("Ты Айри,"));
        assert!(p.contains("\nГоворишь как пират.\n"));
        assert!(!p.contains("заботливая"));
    }

    /// Returns a card next to its text.
    struct CardTool;

    #[async_trait]
    impl crate::tools::Tool for CardTool {
        fn spec(&self) -> crate::llm::ToolSpec {
            crate::tools::spec("card", "test", json!({"type": "object"}))
        }
        fn assess(
            &self,
            _args: &Value,
        ) -> Result<crate::tools::Assessment, crate::tools::ToolError> {
            Ok(crate::tools::Assessment::new(
                crate::tools::RiskLevel::Low,
                "s",
                "t",
                "r",
            ))
        }
        async fn run(
            &self,
            _args: Value,
            _approval: crate::gate::Approval,
            _cancel: CancellationToken,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            Ok(crate::tools::ToolOutput::new("Громкость 40%.")
                .with_data(json!({"kind": "volume", "level": 40, "was": 60})))
        }
    }

    #[tokio::test]
    async fn agent_forwards_tool_data_to_the_bus_but_not_to_the_llm() {
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "card", json!({}))]),
            Some(vec![text("Тише")]),
        ]);
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(CardTool));
        let a = Agent::new(
            p.clone(),
            Arc::new(reg),
            Arc::new(LowRiskOnlyGate),
            bus.clone(),
            StateHandle::new(bus),
            "sys".into(),
        );
        a.handle("тише", CancellationToken::new()).await.unwrap();
        assert_eq!(tool_results(&p.requests.lock()[1]), vec!["Громкость 40%."]);
        assert!(drain(&mut rx).iter().any(|e| matches!(
            e,
            Event::ToolFinished { data: Some(d), .. } if d["kind"] == "volume" && d["level"] == 40
        )));
    }

    /// Reports progress while it runs; with `ready: false` it cannot work yet.
    struct SlowTool {
        ready: bool,
    }

    #[async_trait]
    impl crate::tools::Tool for SlowTool {
        fn spec(&self) -> crate::llm::ToolSpec {
            crate::tools::spec("slow", "test", json!({"type": "object"}))
        }
        fn assess(
            &self,
            args: &Value,
        ) -> Result<crate::tools::Assessment, crate::tools::ToolError> {
            if !self.ready && args.get("late").is_none() {
                return Err(ToolError::NotReady {
                    message: "нет программы".into(),
                    data: json!({"kind": "ffmpeg_missing"}),
                });
            }
            Ok(crate::tools::Assessment::new(
                crate::tools::RiskLevel::Low,
                "s",
                "t",
                "r",
            ))
        }
        async fn run(
            &self,
            _args: Value,
            _approval: crate::gate::Approval,
            _cancel: CancellationToken,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            if !self.ready {
                return Err(ToolError::NotReady {
                    message: "пропала программа".into(),
                    data: json!({"kind": "ffmpeg_missing"}),
                });
            }
            let progress = crate::tools::progress_handle();
            // Another task, as a process callback would be.
            tokio::spawn(async move { progress.set(50) }).await.unwrap();
            Ok(crate::tools::ToolOutput::new("готово"))
        }
    }

    #[tokio::test]
    async fn agent_publishes_tool_progress_with_the_call_id() {
        let p = FakeProvider::new(vec![
            Some(vec![call("c7", "slow", json!({}))]),
            Some(vec![text("Готово")]),
        ]);
        let bus = EventBus::new(64);
        let mut rx = bus.subscribe();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(SlowTool { ready: true }));
        let a = Agent::new(
            p.clone(),
            Arc::new(reg),
            Arc::new(LowRiskOnlyGate),
            bus.clone(),
            StateHandle::new(bus),
            "sys".into(),
        );
        a.handle("давай", CancellationToken::new()).await.unwrap();
        assert!(drain(&mut rx).iter().any(|e| matches!(
            e,
            Event::ToolProgress { id, share: 50 } if id == "c7"
        )));
    }

    #[tokio::test]
    async fn agent_passes_the_card_of_a_not_ready_tool() {
        for (args, output) in [
            (json!({}), "Ошибка: нет программы"),
            (json!({"late": true}), "Ошибка: пропала программа"),
        ] {
            let p = FakeProvider::new(vec![
                Some(vec![call("c1", "slow", args)]),
                Some(vec![text("Нужен FFmpeg")]),
            ]);
            let bus = EventBus::new(64);
            let mut rx = bus.subscribe();
            let mut reg = ToolRegistry::new();
            reg.register(Arc::new(SlowTool { ready: false }));
            let a = Agent::new(
                p.clone(),
                Arc::new(reg),
                Arc::new(LowRiskOnlyGate),
                bus.clone(),
                StateHandle::new(bus),
                "sys".into(),
            );
            a.handle("сожми", CancellationToken::new()).await.unwrap();
            assert_eq!(tool_results(&p.requests.lock()[1]), vec![output]);
            assert!(drain(&mut rx).iter().any(|e| matches!(
                e,
                Event::ToolFinished { ok: false, data: Some(d), output: o, .. }
                    if d["kind"] == "ffmpeg_missing" && o == output
            )));
        }
    }

    #[tokio::test]
    async fn prompt_extras_and_new_registry_reach_the_next_request() {
        let p = FakeProvider::new(vec![Some(vec![text("а")]), Some(vec![text("б")])]);
        let (a, _rx, _) = agent(p.clone());
        a.add_prompt_extra(Arc::new(|| Some("Помни: кот Барсик".into())));
        a.add_prompt_extra(Arc::new(|| None));
        a.handle("1", CancellationToken::new()).await.unwrap();
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        a.set_registry(Arc::new(reg));
        a.handle("2", CancellationToken::new()).await.unwrap();
        let reqs = p.requests.lock();
        assert_eq!(reqs[0].messages[0].content, "sys\n\nПомни: кот Барсик");
        assert_eq!(reqs[0].tools.len(), 2);
        assert_eq!(reqs[1].tools.len(), 1);
    }

    struct FixedRecall;

    #[async_trait]
    impl Recall for FixedRecall {
        async fn recall(&self, user_text: &str) -> Option<String> {
            user_text
                .contains("кот")
                .then(|| "Факт: кот Барсик".to_string())
        }
    }

    #[tokio::test]
    async fn first_piece_of_a_turn_is_timed_once() {
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "echo", json!({"text": "x"}))]),
            Some(vec![text("Готово")]),
        ]);
        let (a, mut rx, _) = agent(p);
        a.handle("x", CancellationToken::new()).await.unwrap();
        let timings: Vec<Event> = drain(&mut rx)
            .into_iter()
            .filter(|e| matches!(e, Event::Timing { .. }))
            .collect();
        assert_eq!(timings.len(), 1);
        assert!(matches!(&timings[0], Event::Timing { stage, .. } if stage == "llm"));
    }

    #[tokio::test]
    async fn recall_joins_the_prompt_only_for_its_own_message() {
        let p = FakeProvider::new(vec![Some(vec![text("а")]), Some(vec![text("б")])]);
        let (a, _rx, _) = agent(p.clone());
        a.set_recall(Arc::new(FixedRecall));
        a.handle("как мой кот?", CancellationToken::new())
            .await
            .unwrap();
        a.handle("погода", CancellationToken::new()).await.unwrap();
        let reqs = p.requests.lock();
        assert_eq!(reqs[0].messages[0].content, "sys\n\nФакт: кот Барсик");
        assert_eq!(reqs[1].messages[0].content, "sys");
    }

    #[tokio::test]
    async fn expanded_message_shows_short_text_and_sends_full_one() {
        let p = FakeProvider::new(vec![Some(vec![text("ок")])]);
        let (a, mut rx, _) = agent(p.clone());
        a.handle_expanded("отбой", "отбой\n\nВыключи звук", CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            drain(&mut rx)[0],
            Event::UserText {
                text: "отбой".into()
            }
        );
        let reqs = p.requests.lock();
        assert_eq!(
            reqs[0].messages.last().unwrap().content,
            "отбой\n\nВыключи звук"
        );
    }

    #[tokio::test]
    async fn tool_call_written_as_text_is_run() {
        let p = FakeProvider::new(vec![
            Some(vec![
                text("[{\"name\": \"echo\", "),
                text("\"arguments\": {\"text\": \"пинг\"}}]"),
            ]),
            Some(vec![text("Готово")]),
        ]);
        let (a, mut rx, _) = agent(p.clone());
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Готово".into())
        );
        assert_eq!(tool_results(&p.requests.lock()[1]), vec!["пинг"]);
        let ev = drain(&mut rx);
        assert!(ev
            .iter()
            .any(|e| matches!(e, Event::ToolFinished { ok: true, .. })));
        // The raw call never reached the chat.
        assert!(!ev
            .iter()
            .any(|e| matches!(e, Event::AssistantDelta { text, .. } if text.contains("echo"))));
    }

    #[tokio::test]
    async fn bracketed_text_that_is_not_a_tool_call_stays_text() {
        for reply in [
            "[{\"a\": 1}]",
            "[{\"name\": \"no_such_tool\", \"arguments\": {}}]",
            "[{\"name\": \"echo\", \"arguments\": {\"text\": \"x\"}}] и ещё слова",
            "[{незакрытая скобка",
        ] {
            let p = FakeProvider::new(vec![Some(vec![text(reply)])]);
            let (a, mut rx, _) = agent(p);
            assert_eq!(
                a.handle("x", CancellationToken::new()).await,
                Ok(reply.to_string()),
                "{reply}"
            );
            assert!(drain(&mut rx).contains(&Event::AssistantDelta {
                text: reply.to_string(),
                spoken: None,
            }));
        }
    }

    /// Shown and spoken text of every delta.
    fn deltas(ev: &[Event]) -> Vec<(String, Option<String>)> {
        ev.iter()
            .filter_map(|e| match e {
                Event::AssistantDelta { text, spoken } => Some((text.clone(), spoken.clone())),
                _ => None,
            })
            .collect()
    }

    fn marks_switch(on: bool) -> MarksSwitch {
        Arc::new(move || on)
    }

    #[tokio::test]
    async fn voice_marks_reach_only_the_spoken_copy() {
        let p = FakeProvider::new(vec![Some(vec![
            text("[jo"),
            text("y] [softly] Привет, "),
            text("<whis"),
            text("per>тихо</whisper>."),
        ])]);
        let (a, mut rx, _) = agent(p.clone());
        a.set_marks_switch(marks_switch(true));
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Привет, тихо.".into())
        );
        let spoken = "[joy] [softly] Привет, <whisper>тихо</whisper>.";
        let ev = drain(&mut rx);
        assert!(ev.contains(&Event::EmotionCue {
            emotion: Emotion::Joy
        }));
        assert!(ev.contains(&Event::AssistantText {
            text: "Привет, тихо.".into(),
            spoken: Some(spoken.into()),
        }));
        let d = deltas(&ev);
        let shown: String = d.iter().map(|(t, _)| t.as_str()).collect();
        let said: String = d.iter().filter_map(|(_, s)| s.as_deref()).collect();
        assert_eq!(shown, "Привет, тихо.");
        assert_eq!(said, spoken);
        assert!(d.iter().all(|(t, s)| !t.is_empty() && s.is_some()), "{d:?}");
        assert_eq!(a.history()[1].content, "Привет, тихо.");
        assert_eq!(
            p.requests.lock()[0].messages[0].content,
            format!("sys\n\n{MARKS_HINT}")
        );
    }

    #[tokio::test]
    async fn spoken_text_without_shown_text_waits_for_the_next_delta() {
        let p = FakeProvider::new(vec![Some(vec![
            text("Привет."),
            text(" [sigh] "),
            text("Ну."),
            text(" [laughs]"),
        ])]);
        let (a, mut rx, _) = agent(p);
        a.set_marks_switch(marks_switch(true));
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Привет. Ну.".into())
        );
        let ev = drain(&mut rx);
        // The last direction has no words after it: an empty delta carries
        // it to the speaker, which joins it to the last phrase.
        assert_eq!(
            deltas(&ev),
            vec![
                ("Привет.".into(), Some("Привет.".into())),
                (" Ну.".into(), Some(" [sigh] Ну.".into())),
                ("".into(), Some(" [laughs]".into())),
            ]
        );
        assert!(ev.contains(&Event::AssistantText {
            text: "Привет. Ну.".into(),
            spoken: Some("Привет. [sigh] Ну. [laughs]".into()),
        }));

        // A step of marks only shows nothing and has no delta at all.
        let p = FakeProvider::new(vec![
            Some(vec![text("[sigh]")]),
            Some(vec![text("Готово.")]),
        ]);
        let (a, mut rx, _) = agent(p);
        a.set_marks_switch(marks_switch(true));
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Готово.".into())
        );
        assert_eq!(
            deltas(&drain(&mut rx)),
            vec![("Готово.".into(), Some("Готово.".into()))]
        );
    }

    #[tokio::test]
    async fn an_emotion_tag_the_chat_loses_is_not_spoken_either() {
        for (reply, shown, cue) in [
            ("Хорошо[joy] Сделаю.", "ХорошоСделаю.", Some(Emotion::Joy)),
            (
                "[deep_thought] Хм, давай подумаем.",
                "Хм, давай подумаем.",
                None,
            ),
            ("[🤔] Хм, давай подумаем.", "Хм, давай подумаем.", None),
            ("Готово [😊😊]!", "Готово !", None),
        ] {
            let p = FakeProvider::new(vec![Some(vec![text(reply)])]);
            let (a, mut rx, _) = agent(p);
            a.set_marks_switch(marks_switch(true));
            assert_eq!(
                a.handle("x", CancellationToken::new()).await,
                Ok(shown.to_string()),
                "{reply}"
            );
            let ev = drain(&mut rx);
            assert!(ev.contains(&Event::AssistantText {
                text: shown.into(),
                spoken: Some(shown.into()),
            }));
            let said: String = deltas(&ev).iter().filter_map(|(_, s)| s.clone()).collect();
            assert_eq!(said, shown, "{reply}");
            assert_eq!(
                ev.iter()
                    .filter_map(|e| match e {
                        Event::EmotionCue { emotion } => Some(*emotion),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                cue.into_iter().collect::<Vec<_>>(),
                "{reply}"
            );
        }
        // An emotion tag that is a direction still reaches the voice, once.
        let p = FakeProvider::new(vec![Some(vec![text("[joy] Ура, получилось!")])]);
        let (a, mut rx, _) = agent(p);
        a.set_marks_switch(marks_switch(true));
        a.handle("x", CancellationToken::new()).await.unwrap();
        let ev = drain(&mut rx);
        assert!(ev.contains(&Event::AssistantText {
            text: "Ура, получилось!".into(),
            spoken: Some("[joy] Ура, получилось!".into()),
        }));
        let cues = ev
            .iter()
            .filter(|e| matches!(e, Event::EmotionCue { .. }))
            .count();
        assert_eq!(cues, 1);
    }

    #[tokio::test]
    async fn a_new_step_of_a_reply_is_announced_while_marks_are_on() {
        for on in [true, false] {
            let p = FakeProvider::new(vec![
                Some(vec![text("Минутку, сейчас я посмотрю")]),
                Some(vec![text("[cheerfully] Завтра будет солнечно.")]),
            ]);
            let (a, mut rx, _) = agent(p);
            a.set_marks_switch(marks_switch(on));
            a.handle("x", CancellationToken::new()).await.unwrap();
            let ev = drain(&mut rx);
            let step = ev.iter().position(|e| *e == Event::StepStarted);
            assert_eq!(step.is_some(), on);
            if let Some(at) = step {
                // Between the deltas of the two steps.
                let first = ev
                    .iter()
                    .position(|e| matches!(e, Event::AssistantDelta { .. }))
                    .unwrap();
                let last = ev
                    .iter()
                    .rposition(|e| matches!(e, Event::AssistantDelta { .. }))
                    .unwrap();
                assert!(first < at && at < last, "{ev:?}");
                assert_eq!(ev.iter().filter(|e| **e == Event::StepStarted).count(), 1);
            }
        }
    }

    #[tokio::test]
    async fn without_the_switch_brackets_stay_and_nothing_is_spoken_apart() {
        let reply = "[очень длинный текст в скобках, не метка] дальше <whisper>да</whisper>";
        for switch in [None, Some(marks_switch(false))] {
            let p = FakeProvider::new(vec![Some(vec![text(reply)])]);
            let (a, mut rx, _) = agent(p.clone());
            if let Some(s) = switch {
                a.set_marks_switch(s);
            }
            assert_eq!(
                a.handle("x", CancellationToken::new()).await,
                Ok(reply.to_string())
            );
            let ev = drain(&mut rx);
            assert!(ev.contains(&Event::AssistantText {
                text: reply.into(),
                spoken: None,
            }));
            assert!(deltas(&ev).iter().all(|(_, s)| s.is_none()));
            assert_eq!(p.requests.lock()[0].messages[0].content, "sys");
        }
    }

    #[tokio::test]
    async fn the_switch_is_read_at_every_step() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let p = FakeProvider::new(vec![
            Some(vec![text("Ну [sigh] а")]),
            Some(vec![text("Ну [sigh] б")]),
        ]);
        let (a, mut rx, _) = agent(p.clone());
        let on = Arc::new(AtomicBool::new(true));
        let flag = on.clone();
        a.add_prompt_extra(Arc::new(|| Some("Помни: кот Барсик".into())));
        a.set_marks_switch(Arc::new(move || flag.load(Ordering::Relaxed)));
        assert_eq!(
            a.handle("1", CancellationToken::new()).await,
            Ok("Ну а".into())
        );
        on.store(false, Ordering::Relaxed);
        assert_eq!(
            a.handle("2", CancellationToken::new()).await,
            Ok("Ну [sigh] б".into())
        );
        let reqs = p.requests.lock();
        assert_eq!(
            reqs[0].messages[0].content,
            format!("sys\n\nПомни: кот Барсик\n\n{MARKS_HINT}")
        );
        assert_eq!(reqs[1].messages[0].content, "sys\n\nПомни: кот Барсик");
        let finals: Vec<Option<String>> = drain(&mut rx)
            .into_iter()
            .filter_map(|e| match e {
                Event::AssistantText { spoken, .. } => Some(spoken),
                _ => None,
            })
            .collect();
        assert_eq!(finals, vec![Some("Ну [sigh] а".into()), None]);
    }

    #[tokio::test]
    async fn written_calls_are_not_spoken_with_marks_on() {
        let p = FakeProvider::new(vec![
            Some(vec![
                text("[{\"name\": \"echo\", "),
                text("\"arguments\": {\"text\": \"пинг\"}}]"),
            ]),
            Some(vec![text("[wink] Готово")]),
        ]);
        let (a, mut rx, _) = agent(p.clone());
        a.set_marks_switch(marks_switch(true));
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok("Готово".into())
        );
        assert_eq!(tool_results(&p.requests.lock()[1]), vec!["пинг"]);
        let ev = drain(&mut rx);
        assert_eq!(
            deltas(&ev),
            vec![("Готово".into(), Some("[wink] Готово".into()))]
        );
        assert!(ev.contains(&Event::EmotionCue {
            emotion: Emotion::Wink
        }));

        // Text that only looks like calls is shown and spoken as it came.
        let reply = "[{\"a\": 1}]";
        let p = FakeProvider::new(vec![Some(vec![text(reply)])]);
        let (a, mut rx, _) = agent(p);
        a.set_marks_switch(marks_switch(true));
        assert_eq!(
            a.handle("x", CancellationToken::new()).await,
            Ok(reply.to_string())
        );
        let ev = drain(&mut rx);
        assert!(ev.contains(&Event::AssistantDelta {
            text: reply.into(),
            spoken: Some(reply.into()),
        }));
        assert!(ev.contains(&Event::AssistantText {
            text: reply.into(),
            spoken: Some(reply.into()),
        }));
    }

    #[tokio::test]
    async fn text_that_turns_out_to_be_no_calls_keeps_its_marks_for_the_voice() {
        for chunks in [
            vec!["[{\"a\": 1}] это пример. [laughs] Шучу."],
            vec!["[sigh] ", "[{\"a\": 1}] это ", "пример. [laughs] Шучу."],
        ] {
            let script = chunks.iter().map(|c| text(c)).collect();
            let p = FakeProvider::new(vec![Some(script)]);
            let (a, mut rx, _) = agent(p);
            a.set_marks_switch(marks_switch(true));
            let shown = "[{\"a\": 1}] это пример. Шучу.";
            assert_eq!(
                a.handle("x", CancellationToken::new()).await,
                Ok(shown.into())
            );
            let ev = drain(&mut rx);
            let said: String = deltas(&ev).iter().filter_map(|(_, s)| s.clone()).collect();
            let spoken = chunks.concat();
            assert_eq!(said, spoken);
            assert!(ev.contains(&Event::AssistantText {
                text: shown.into(),
                spoken: Some(spoken),
            }));
        }
    }

    #[tokio::test]
    async fn calls_written_as_text_keep_their_bracketed_arguments_with_marks_on() {
        for (call, said) in [
            (
                "[{\"name\":\"echo\",\"arguments\":{\"text\":\"x\",\"names\":[\"Файл\",\"Сохранить\"]}}]",
                "x",
            ),
            (
                "[{\"name\":\"echo\",\"arguments\":{\"text\":\"купить [молоко]\"}}]",
                "купить [молоко]",
            ),
            // A direction in front of the calls is no part of them.
            (
                "[sigh] [{\"name\":\"echo\",\"arguments\":{\"text\":\"пинг\"}}]",
                "пинг",
            ),
        ] {
            let p = FakeProvider::new(vec![Some(vec![text(call)]), Some(vec![text("Готово")])]);
            let (a, mut rx, _) = agent(p.clone());
            a.set_marks_switch(marks_switch(true));
            assert_eq!(
                a.handle("x", CancellationToken::new()).await,
                Ok("Готово".into()),
                "{call}"
            );
            assert_eq!(tool_results(&p.requests.lock()[1]), vec![said], "{call}");
            assert!(
                deltas(&drain(&mut rx)).iter().all(|(t, _)| t == "Готово"),
                "{call}"
            );
        }
    }

    /// Keeps the arguments of each call; named like a tool with text for people.
    struct Recorder {
        name: &'static str,
        got: Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl crate::tools::Tool for Recorder {
        fn spec(&self) -> crate::llm::ToolSpec {
            crate::tools::spec(self.name, "test", json!({"type": "object"}))
        }
        fn assess(
            &self,
            args: &Value,
        ) -> Result<crate::tools::Assessment, crate::tools::ToolError> {
            let text = args["text"].as_str().unwrap_or_default();
            Ok(crate::tools::Assessment::new(
                crate::tools::RiskLevel::Low,
                format!("Напомнить: {text}"),
                text,
                "r",
            ))
        }
        async fn run(
            &self,
            args: Value,
            _approval: crate::gate::Approval,
            _cancel: CancellationToken,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            self.got.lock().push(args);
            Ok(crate::tools::ToolOutput::new("ок"))
        }
    }

    #[tokio::test]
    async fn marks_that_leak_into_text_for_people_are_cut_before_the_call() {
        for on in [true, false] {
            let p = FakeProvider::new(vec![
                Some(vec![call(
                    "c1",
                    "reminder_set",
                    json!({"text": "[softly] Выпей <whisper>воды</whisper>", "in_minutes": 5}),
                )]),
                Some(vec![text("Готово")]),
            ]);
            let bus = EventBus::new(64);
            let mut rx = bus.subscribe();
            let tool = Arc::new(Recorder {
                name: "reminder_set",
                got: Mutex::new(Vec::new()),
            });
            let mut reg = ToolRegistry::new();
            reg.register(tool.clone());
            let a = Agent::new(
                p,
                Arc::new(reg),
                Arc::new(LowRiskOnlyGate),
                bus.clone(),
                StateHandle::new(bus),
                "sys".into(),
            );
            a.set_marks_switch(marks_switch(on));
            a.handle("напомни", CancellationToken::new()).await.unwrap();
            let text = if on {
                "Выпей воды"
            } else {
                "[softly] Выпей <whisper>воды</whisper>"
            };
            assert_eq!(
                *tool.got.lock(),
                [json!({"text": text, "in_minutes": 5})],
                "{on}"
            );
            assert!(drain(&mut rx).iter().any(|e| matches!(
                e,
                Event::ToolStarted { summary, .. } if summary.contains(text)
            )));
        }
    }

    #[test]
    fn only_text_for_people_loses_its_marks() {
        assert_eq!(
            unmark_args(
                "messenger_send",
                json!({"contact": "[Аня]", "text": "<whisper>Привет</whisper>, [softly] как ты?"})
            ),
            json!({"contact": "[Аня]", "text": "Привет, как ты?"})
        );
        assert_eq!(
            unmark_args(
                "task_plan",
                json!({"title": "[sigh] Уборка", "steps": ["[softly] Открыть папку", "Переложить [1] файл"]})
            ),
            json!({"title": "Уборка", "steps": ["Открыть папку", "Переложить [1] файл"]})
        );
        // Text without marks keeps its spacing; data is never touched.
        let untouched = [
            ("memory_save", json!({"text": "  Любит чай  "})),
            ("search_text", json!({"pattern": "[a-z] [тихо]"})),
            ("move_path", json!({"from": "C:\\Docs\\[Архив] старое"})),
            ("write_file", json!({"content": "Нажми [Сохранить]"})),
            ("ui_type", json!({"text": "[Сохранить]"})),
        ];
        for (name, args) in untouched {
            assert_eq!(unmark_args(name, args.clone()), args, "{name}");
        }
    }

    #[test]
    fn marks_hint_names_every_effect_tag() {
        for tag in &crate::marks::EFFECT_TAGS[..7] {
            assert!(MARKS_HINT.contains(&format!("<{tag}>…</{tag}>")), "{tag}");
        }
        assert!(MARKS_HINT.contains(".\n- Ремарка: ") && MARKS_HINT.contains("нет.\nПользователь"));
    }

    #[test]
    fn marks_hint_states_what_the_filter_takes_for_a_mark() {
        // The model cannot keep to limits it is not told: these match `marks`.
        assert!(MARKS_HINT.contains(&format!("не больше {} знаков", crate::marks::MAX_DIRECTION)));
        for rule in [
            "без цифр",
            "[pause 2s]",
            "Перед [ ставь пробел",
            "[/whisper]",
            "<break>",
        ] {
            assert!(MARKS_HINT.contains(rule), "{rule}");
        }
    }

    #[tokio::test]
    async fn direct_turn_shows_the_question_and_keeps_the_pair() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let p = FakeProvider::new(vec![]);
        let (a, mut rx, state) = agent(p.clone());
        let a = a
            .with_history(vec![ChatMessage::user("раньше")])
            .with_store(db.clone(), "main");
        let reply = a
            .handle_direct(
                "  Вопрос про окно «Отчёт»: что тут?  ",
                async { Ok("  Это таблица.\n".to_string()) },
                CancellationToken::new(),
            )
            .await;
        assert_eq!(reply.as_deref(), Some("Это таблица."));
        assert_eq!(
            drain(&mut rx),
            vec![
                Event::UserText {
                    text: "Вопрос про окно «Отчёт»: что тут?".into()
                },
                Event::StateChanged {
                    from: AssistantState::Idle,
                    to: AssistantState::Thinking
                },
                Event::AssistantText {
                    text: "Это таблица.".into(),
                    spoken: None
                },
                Event::StateChanged {
                    from: AssistantState::Thinking,
                    to: AssistantState::Idle
                },
            ]
        );
        assert_eq!(state.get(), AssistantState::Idle);
        let pair = vec![
            ChatMessage::user("Вопрос про окно «Отчёт»: что тут?"),
            ChatMessage::assistant("Это таблица."),
        ];
        assert_eq!(a.history()[1..], pair[..]);
        assert_eq!(db.load_messages("main").unwrap(), pair);
        // The model of the agent is never asked.
        assert!(p.requests.lock().is_empty());
    }

    #[tokio::test]
    async fn direct_turn_error_is_published_and_nothing_is_kept() {
        let (a, mut rx, state) = agent(FakeProvider::new(vec![]));
        let reply = a
            .handle_direct(
                "вопрос",
                async { Err("нет связи".to_string()) },
                CancellationToken::new(),
            )
            .await;
        assert_eq!(reply, None);
        let ev = drain(&mut rx);
        assert!(ev.contains(&Event::Error {
            message: "нет связи".into()
        }));
        assert!(!ev.iter().any(|e| matches!(e, Event::AssistantText { .. })));
        assert!(a.history().is_empty());
        assert_eq!(state.get(), AssistantState::Idle);
    }

    #[tokio::test]
    async fn direct_turn_with_an_empty_answer_is_an_error() {
        let (a, mut rx, state) = agent(FakeProvider::new(vec![]));
        let reply = a
            .handle_direct(
                "вопрос",
                async { Ok(" \n ".to_string()) },
                CancellationToken::new(),
            )
            .await;
        assert_eq!(reply, None);
        let ev = drain(&mut rx);
        assert!(ev.contains(&Event::Error {
            message: "модель ничего не ответила".into()
        }));
        assert!(!ev.iter().any(|e| matches!(e, Event::AssistantText { .. })));
        assert!(a.history().is_empty());
        assert_eq!(state.get(), AssistantState::Idle);
    }

    #[tokio::test]
    async fn direct_turn_stopped_says_nothing_more() {
        let (a, mut rx, state) = agent(FakeProvider::new(vec![]));
        // Stopped before the answer came.
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            c2.cancel();
        });
        let reply = a
            .handle_direct(
                "вопрос",
                std::future::pending::<Result<String, String>>(),
                cancel,
            )
            .await;
        assert_eq!(reply, None);
        // A token cancelled beforehand wins over a ready answer.
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let reply = a
            .handle_direct("ещё вопрос", async { Ok("ответ".to_string()) }, cancelled)
            .await;
        assert_eq!(reply, None);
        let ev = drain(&mut rx);
        assert!(!ev
            .iter()
            .any(|e| matches!(e, Event::AssistantText { .. } | Event::Error { .. })));
        assert!(a.history().is_empty());
        assert_eq!(state.get(), AssistantState::Idle);
    }

    #[tokio::test]
    async fn direct_turn_waits_for_the_running_reply() {
        let (a, mut rx, state) = agent(FakeProvider::new(vec![None]));
        let a = Arc::new(a);
        assert!(!a.turn_running());
        let cancel = CancellationToken::new();
        let running = {
            let (a, cancel) = (a.clone(), cancel.clone());
            tokio::spawn(async move { a.handle("долгий вопрос", cancel).await })
        };
        for _ in 0..1000 {
            if state.get() == AssistantState::Thinking {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert!(a.turn_running());
        let direct = {
            let a = a.clone();
            tokio::spawn(async move {
                a.handle_direct(
                    "вопрос про окно",
                    async { Ok("ответ".to_string()) },
                    CancellationToken::new(),
                )
                .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let shown = Event::UserText {
            text: "вопрос про окно".into(),
        };
        assert!(!drain(&mut rx).contains(&shown));
        assert!(!direct.is_finished());

        cancel.cancel();
        assert_eq!(running.await.unwrap(), Err(AgentError::Cancelled));
        assert_eq!(direct.await.unwrap().as_deref(), Some("ответ"));
        let ev = drain(&mut rx);
        assert!(ev.contains(&shown));
        assert!(ev.contains(&Event::AssistantText {
            text: "ответ".into(),
            spoken: None
        }));
        assert_eq!(state.get(), AssistantState::Idle);
        assert!(!a.turn_running());
    }

    // ---- skills (decision 0047) ----

    /// A tool of a given name and risk that counts its runs.
    struct Act {
        name: &'static str,
        level: RiskLevel,
        runs: Arc<Mutex<Vec<Value>>>,
    }

    #[async_trait]
    impl crate::tools::Tool for Act {
        fn spec(&self) -> crate::llm::ToolSpec {
            crate::tools::spec(self.name, "test", json!({"type": "object"}))
        }
        fn assess(&self, _: &Value) -> Result<crate::tools::Assessment, ToolError> {
            Ok(crate::tools::Assessment::new(
                self.level, self.name, "t", "r",
            ))
        }
        async fn run(
            &self,
            args: Value,
            _: crate::gate::Approval,
            _: CancellationToken,
        ) -> Result<crate::tools::ToolOutput, ToolError> {
            self.runs.lock().push(args);
            Ok(crate::tools::ToolOutput::new(format!(
                "{} сделано",
                self.name
            )))
        }
    }

    /// Stands for `skill_run`: hands its steps to the loop.
    struct FakeSkillRun(Vec<crate::skills::SkillStep>);

    #[async_trait]
    impl crate::tools::Tool for FakeSkillRun {
        fn spec(&self) -> crate::llm::ToolSpec {
            crate::tools::spec("skill_run", "test", json!({"type": "object"}))
        }
        fn assess(&self, _: &Value) -> Result<crate::tools::Assessment, ToolError> {
            Ok(crate::tools::Assessment::new(
                RiskLevel::Low,
                "навык",
                "t",
                "r",
            ))
        }
        async fn run(
            &self,
            _: Value,
            _: crate::gate::Approval,
            _: CancellationToken,
        ) -> Result<crate::tools::ToolOutput, ToolError> {
            Ok(
                crate::tools::ToolOutput::new("Навык «Утро»").with_replay(crate::tools::Replay {
                    skill: "Утро".into(),
                    steps: self.0.clone(),
                }),
            )
        }
    }

    /// Approves everything but the tools it was told to refuse, and
    /// remembers what it was asked and at which risk.
    struct Watching {
        refuse: &'static [&'static str],
        asked: Mutex<Vec<(String, RiskLevel)>>,
    }

    #[async_trait]
    impl Gate for Watching {
        async fn check(&self, tool: &str, _: &Value, a: &crate::tools::Assessment) -> GateDecision {
            self.asked.lock().push((tool.to_string(), a.level));
            if self.refuse.contains(&tool) {
                GateDecision::Denied("пользователь отклонил".into())
            } else {
                GateDecision::Approved(crate::gate::Approval::given("test"))
            }
        }
    }

    type Runs = Arc<Mutex<Vec<Value>>>;

    fn act(reg: &mut ToolRegistry, name: &'static str, level: RiskLevel) -> Runs {
        let runs: Runs = Arc::default();
        reg.register(Arc::new(Act {
            name,
            level,
            runs: runs.clone(),
        }));
        runs
    }

    fn skill_agent(
        p: Arc<FakeProvider>,
        reg: ToolRegistry,
        gate: Arc<dyn Gate>,
    ) -> (Agent, crate::skills::SkillBook, Receiver<Event>) {
        let bus = EventBus::new(256);
        let rx = bus.subscribe();
        let a = Agent::new(
            p,
            Arc::new(reg),
            gate,
            bus.clone(),
            StateHandle::new(bus),
            "sys".into(),
        );
        let book = crate::skills::SkillBook::default();
        a.set_skill_book(book.clone());
        (a, book, rx)
    }

    fn step(tool: &str, args: Value) -> crate::skills::SkillStep {
        crate::skills::SkillStep {
            tool: tool.into(),
            args,
        }
    }

    #[tokio::test]
    async fn a_turn_of_three_actions_is_recorded_and_offered_after_the_reply() {
        let p = FakeProvider::new(vec![
            Some(vec![
                call("c1", "open_app", json!({"name": "notepad"})),
                call("c2", "get_time", json!({})),
                call("c3", "volume", json!({"level": 30})),
                call("c4", "open_url", json!({"url": "https://mail.example"})),
            ]),
            Some(vec![text("Готово")]),
        ]);
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(TimeTool));
        act(&mut reg, "open_app", RiskLevel::Medium);
        act(&mut reg, "volume", RiskLevel::Low);
        act(&mut reg, "open_url", RiskLevel::Medium);
        let gate = Arc::new(Watching {
            refuse: &[],
            asked: Mutex::default(),
        });
        let (a, book, mut rx) = skill_agent(p, reg, gate);
        a.handle("рабочий режим", CancellationToken::new())
            .await
            .unwrap();
        let last = book.last().expect("the turn is recorded");
        let tools: Vec<&str> = last.steps.iter().map(|s| s.tool.as_str()).collect();
        assert_eq!(tools, ["open_app", "get_time", "volume", "open_url"]);
        assert_eq!(last.steps[0].args, json!({"name": "notepad"}));
        assert!(last.steps[1].read && !last.steps[2].read);
        let ev = drain(&mut rx);
        let reply = ev
            .iter()
            .position(|e| matches!(e, Event::AssistantText { .. }))
            .unwrap();
        let offer = ev
            .iter()
            .position(|e| matches!(e, Event::SkillOffer { id, actions: 3 } if *id == last.id))
            .expect("the offer comes");
        assert!(offer > reply, "under the reply");
    }

    #[tokio::test]
    async fn refused_and_failed_calls_are_not_recorded_and_stop_the_offer() {
        let p = FakeProvider::new(vec![
            Some(vec![
                call("c1", "open_app", json!({})),
                call("c2", "volume", json!({})),
                call("c3", "open_url", json!({})),
                call("c4", "kill_process", json!({})),
                call("c5", "nothing_here", json!({})),
            ]),
            Some(vec![text("Почти")]),
        ]);
        let mut reg = ToolRegistry::new();
        act(&mut reg, "open_app", RiskLevel::Medium);
        act(&mut reg, "volume", RiskLevel::Low);
        act(&mut reg, "open_url", RiskLevel::Medium);
        act(&mut reg, "kill_process", RiskLevel::High);
        let gate = Arc::new(Watching {
            refuse: &["kill_process"],
            asked: Mutex::default(),
        });
        let (a, book, mut rx) = skill_agent(p, reg, gate);
        a.handle("x", CancellationToken::new()).await.unwrap();
        let tools: Vec<String> = book
            .last()
            .unwrap()
            .steps
            .into_iter()
            .map(|s| s.tool)
            .collect();
        assert_eq!(tools, ["open_app", "volume", "open_url"]);
        assert!(!drain(&mut rx)
            .iter()
            .any(|e| matches!(e, Event::SkillOffer { .. })));
    }

    #[tokio::test]
    async fn skill_steps_run_one_by_one_through_the_gate_and_stop_at_a_refusal() {
        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "skill_run", json!({"name": "Утро"}))]),
            Some(vec![text("Второй шаг ты не разрешил")]),
        ]);
        let mut reg = ToolRegistry::new();
        let first = act(&mut reg, "open_app", RiskLevel::Low);
        let second = act(&mut reg, "run_shell", RiskLevel::High);
        let third = act(&mut reg, "open_url", RiskLevel::Medium);
        reg.register(Arc::new(FakeSkillRun(vec![
            step("open_app", json!({"name": "notepad"})),
            step("run_shell", json!({"command": "Get-Date"})),
            step("open_url", json!({"url": "https://example.com"})),
        ])));
        let gate = Arc::new(Watching {
            refuse: &["run_shell"],
            asked: Mutex::default(),
        });
        let (a, book, mut rx) = skill_agent(p.clone(), reg, gate.clone());
        a.handle("утро", CancellationToken::new()).await.unwrap();
        // Each step was asked for on its own, the high one at its own risk.
        assert_eq!(
            *gate.asked.lock(),
            vec![
                ("skill_run".to_string(), RiskLevel::Low),
                ("open_app".to_string(), RiskLevel::Low),
                ("run_shell".to_string(), RiskLevel::High),
            ]
        );
        assert_eq!(*first.lock(), vec![json!({"name": "notepad"})]);
        assert!(second.lock().is_empty() && third.lock().is_empty());
        let result = &tool_results(&p.requests.lock()[1])[0];
        assert!(result.contains("1 из 3"), "{result}");
        assert!(result.contains("2. run_shell"), "{result}");
        assert!(result.contains("пользователь отклонил"), "{result}");
        assert!(result.contains("3. open_url"), "{result}");
        // Every step has its own row in the chat.
        let ev = drain(&mut rx);
        assert!(ev
            .iter()
            .any(|e| matches!(e, Event::ToolFinished { id, ok: true, .. } if id == "c1-step1")));
        assert!(ev
            .iter()
            .any(|e| matches!(e, Event::ToolFinished { id, ok: false, .. } if id == "c1-step2")));
        // A replay is no new recording and offers nothing.
        assert_eq!(book.last(), None);
        assert!(!ev.iter().any(|e| matches!(e, Event::SkillOffer { .. })));
    }

    #[tokio::test]
    async fn a_high_step_of_a_skill_asks_the_user_and_offers_no_always() {
        use crate::confirm::{Answer, Confirmations, DecidedBy};
        use crate::policy::{AuditLog, PolicyGate, RuleStore};

        let p = FakeProvider::new(vec![
            Some(vec![call("c1", "skill_run", json!({}))]),
            Some(vec![text("Хорошо")]),
        ]);
        let mut reg = ToolRegistry::new();
        let shell = act(&mut reg, "run_shell", RiskLevel::High);
        reg.register(Arc::new(FakeSkillRun(vec![step(
            "run_shell",
            json!({"command": "Remove-Item x"}),
        )])));
        let bus = EventBus::new(256);
        let state = StateHandle::new(bus.clone());
        let db = Arc::new(crate::storage::Db::open_in_memory().unwrap());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let gate = Arc::new(PolicyGate::new(
            RuleStore::new(db.clone()),
            AuditLog::new(db),
            confirms.clone(),
        ));
        let a = Agent::new(p.clone(), Arc::new(reg), gate, bus, state, "s".into());
        a.set_skill_book(crate::skills::SkillBook::default());
        let asking = confirms.clone();
        let answer = tokio::spawn(async move {
            let mut asked = Vec::new();
            for _ in 0..1000 {
                if let Some(q) = asking.pending() {
                    asked.push(q.tool.clone());
                    if q.tool == "run_shell" {
                        assert!(!q.allow_always, "a high step is never allowed forever");
                        asking
                            .answer(q.id, Answer::Deny, DecidedBy::Button)
                            .unwrap();
                        return asked;
                    }
                    asking
                        .answer(q.id, Answer::Once, DecidedBy::Button)
                        .unwrap();
                }
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
            asked
        });
        a.handle("x", CancellationToken::new()).await.unwrap();
        // The skill itself, then its high step on its own card.
        assert_eq!(answer.await.unwrap(), ["skill_run", "run_shell"]);
        assert!(shell.lock().is_empty());
        assert!(tool_results(&p.requests.lock()[1])[0].contains("0 из 1"));
    }
}
