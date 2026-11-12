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
use crate::llm::{ChatMessage, LlmChunk, LlmError, LlmProvider, LlmRequest, ToolCall};
use crate::memory::Recall;
use crate::state::{AssistantState, StateHandle};
use crate::storage::Db;
use crate::task::{TaskBoard, TASK_MAX_STEPS};
use crate::tools::ToolRegistry;

/// LLM calls per user message. A task with a plan gets `task::TASK_MAX_STEPS`.
pub const MAX_STEPS: usize = 8;
/// Reminders per message that a plan still has open steps.
const MAX_NUDGES: usize = 2;
/// Tool output sent back to the LLM, characters.
const MAX_TOOL_RESULT: usize = 16_000;
/// Tool output shown in the chat row, characters.
const MAX_TOOL_PREVIEW: usize = 400;
/// Past conversation sent with each request, characters. Small local models have small contexts.
pub const HISTORY_BUDGET: usize = 40_000;

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
}

pub fn system_prompt(assistant: &AssistantConfig) -> String {
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
по номерам элементов; после каждого действия читай окно заново.\n\
{persona}\n\
В самом начале ответа можешь поставить одну метку эмоции в квадратных скобках: \
[joy] радость, [shy] смущение, [surprise] удивление, [sad] грусть, [angry] сердишься, \
[think] задумалась, [wink] шутишь, [sleepy] сонная, [worry] тревога. \
Ставь метку, только когда эмоция уместна. Пользователь метку не видит, не упоминай её.",
        name = assistant.name,
        persona = assistant.persona_prompt(),
    )
}

/// Longest text between `[` and `]` that can still be an emotion tag.
const MAX_TAG: usize = 16;

/// Cuts emotion tags like `[joy]` out of streamed reply text.
///
/// A tag may arrive split across chunks, so text after `[` is held back until `]`
/// comes or the text is too long to be a tag. Brackets that are not tags pass through.
#[derive(Debug, Default)]
pub struct TagFilter {
    held: String,
    started: bool,
    skip_space: bool,
}

impl TagFilter {
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
            out.push(ch);
        }
    }

    /// Returns the text to show and the emotions of the tags that ended in this chunk.
    pub fn push(&mut self, chunk: &str) -> (String, Vec<Emotion>) {
        let mut out = String::new();
        let mut cues = Vec::new();
        for ch in chunk.chars() {
            if self.held.is_empty() {
                if ch == '[' {
                    self.held.push(ch);
                } else {
                    self.emit(&mut out, ch.encode_utf8(&mut [0; 4]));
                }
                continue;
            }
            match ch {
                ']' => {
                    let held = std::mem::take(&mut self.held);
                    match Emotion::from_tag(&held[1..]) {
                        Some(e) => {
                            cues.push(e);
                            self.skip_space = true;
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

pub struct Agent {
    provider: RwLock<Arc<dyn LlmProvider>>,
    registry: RwLock<Arc<ToolRegistry>>,
    extras: RwLock<Vec<PromptExtra>>,
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
}

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
        }
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

    /// Replaces the tool set, for example when an MCP server connects.
    pub fn set_registry(&self, registry: Arc<ToolRegistry>) {
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

    fn full_system_prompt(&self) -> String {
        let mut prompt = self.system_prompt.read().clone();
        for extra in self.extras.read().iter() {
            if let Some(text) = extra().filter(|t| !t.trim().is_empty()) {
                prompt.push_str("\n\n");
                prompt.push_str(text.trim());
            }
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
        let mut new_msgs = vec![ChatMessage::user(text)];
        let result = self.run_steps(&mut new_msgs, &cancel).await;
        if let Some(b) = &board {
            b.finish(result.is_err());
        }

        match &result {
            Ok(reply) => {
                self.bus.publish(Event::AssistantText {
                    text: reply.clone(),
                });
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
        result
    }

    fn commit(&self, msgs: Vec<ChatMessage>) {
        if let Some((db, conv)) = &self.store {
            for m in &msgs {
                if let Err(e) = db.append_message(conv, m) {
                    log::warn!("cannot save message: {e}");
                }
            }
        }
        self.history.lock().extend(msgs);
    }

    /// Runs LLM steps. On error `msgs` keeps only finished steps, so history stays valid.
    async fn run_steps(
        &self,
        msgs: &mut Vec<ChatMessage>,
        cancel: &CancellationToken,
    ) -> Result<String, AgentError> {
        let mut step = 0;
        let mut nudges = 0;
        while step < self.step_limit() {
            let (text, calls) = self.one_step(msgs, cancel, step == 0).await?;
            step += 1;
            if calls.is_empty() {
                // Small models stop in the middle of a plan: remind them once or twice.
                let open = self
                    .tasks
                    .read()
                    .as_ref()
                    .map(TaskBoard::open_steps)
                    .unwrap_or_default();
                if !open.is_empty() && nudges < MAX_NUDGES {
                    nudges += 1;
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
                return Ok(text);
            }
            msgs.push(ChatMessage::assistant_with_calls(text, calls.clone()));
            for call in calls {
                let result = self.execute(&call, cancel).await;
                msgs.push(ChatMessage::tool_result(call.id, result));
                if cancel.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
            }
        }
        Err(AgentError::TooManySteps)
    }

    async fn one_step(
        &self,
        msgs: &[ChatMessage],
        cancel: &CancellationToken,
        // The first request of a turn reports how long the provider took to start answering.
        timed: bool,
    ) -> Result<(String, Vec<ToolCall>), AgentError> {
        let asked = Instant::now();
        let mut waiting_first = timed;
        let mut messages = vec![ChatMessage::system(self.full_system_prompt())];
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
        let mut calls = Vec::new();
        let mut tags = TagFilter::default();
        // A reply that starts like tool calls written as text is held back, see `calls_from_text`.
        let mut written_calls = String::new();
        let mut route = |agent: &Self, text: &mut String, piece: String| {
            if !written_calls.is_empty() || (text.is_empty() && piece.starts_with("[{")) {
                written_calls.push_str(&piece);
            } else {
                agent.say(text, piece);
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
                log::info!("first piece of the answer after {ms} ms");
                self.bus.publish(Event::Timing {
                    stage: "llm".into(),
                    ms,
                });
            }
            match chunk {
                None => {
                    let rest = tags.finish();
                    route(self, &mut text, rest);
                    break;
                }
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(LlmChunk::Text(t))) => {
                    let (clean, cues) = tags.push(&t);
                    for emotion in cues {
                        self.bus.publish(Event::EmotionCue { emotion });
                    }
                    route(self, &mut text, clean);
                }
                Some(Ok(LlmChunk::ToolCall(c))) => calls.push(c),
            }
        }
        if !written_calls.is_empty() {
            match self.calls_from_text(&written_calls) {
                Some(found) if calls.is_empty() => calls = found,
                _ => self.say(&mut text, written_calls),
            }
        }
        Ok((text.trim_end().to_string(), calls))
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

    /// Adds reply text that is already free of emotion tags.
    fn say(&self, text: &mut String, piece: String) {
        if piece.is_empty() {
            return;
        }
        if text.is_empty() {
            self.state.set(AssistantState::Speaking);
        }
        text.push_str(&piece);
        self.bus.publish(Event::AssistantDelta { text: piece });
    }

    /// Runs one tool call. Always returns text for the LLM, errors included.
    async fn execute(&self, call: &ToolCall, cancel: &CancellationToken) -> String {
        let started = Instant::now();
        let Some(tool) = self.registry().get(&call.name) else {
            let msg = format!("Ошибка: инструмента `{}` нет.", call.name);
            self.finish(call, false, &msg, None, started);
            return msg;
        };
        let args = match &call.arguments {
            Value::Null => Value::Object(Default::default()),
            v => v.clone(),
        };
        let assessment = match tool.assess(&args) {
            Ok(a) => a,
            Err(e) => {
                let msg = format!("Ошибка: {e}");
                self.finish(call, false, &msg, None, started);
                return msg;
            }
        };
        self.bus.publish(Event::ToolStarted {
            id: call.id.clone(),
            name: call.name.clone(),
            summary: assessment.summary.clone(),
        });

        let decision = tokio::select! {
            _ = cancel.cancelled() => GateDecision::Denied("остановлено пользователем".into()),
            d = self.gate.check(&call.name, &args, &assessment) => d,
        };
        let approval = match decision {
            GateDecision::Approved(a) => a,
            GateDecision::Denied(reason) => {
                let msg = format!("Действие не выполнено: {reason}.");
                self.finish(call, false, &msg, None, started);
                return msg;
            }
        };
        self.state.set(AssistantState::Thinking);
        let (ok, text, data) = match tool.run(args, approval, cancel.child_token()).await {
            Ok(out) => (true, out.text, out.data),
            Err(e) => (false, format!("Ошибка: {e}"), None),
        };
        self.finish(call, ok, &text, data, started);
        truncate(&text, MAX_TOOL_RESULT)
    }

    fn finish(
        &self,
        call: &ToolCall,
        ok: bool,
        output: &str,
        data: Option<Value>,
        started: Instant,
    ) {
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

pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
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
            text: "При".into()
        }));
        assert!(ev.contains(&Event::AssistantText {
            text: "Привет".into()
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
    fn tag_filter_keeps_ordinary_brackets() {
        for text in [
            "Смотри [1] и [файл.txt].",
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
            text: "Привет".into()
        }));
        let deltas: String = ev
            .iter()
            .filter_map(|e| match e {
                Event::AssistantDelta { text } => Some(text.as_str()),
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
            .any(|e| matches!(e, Event::AssistantDelta { text } if text.contains("echo"))));
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
                text: reply.to_string()
            }));
        }
    }
}
