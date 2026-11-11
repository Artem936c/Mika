//! Agent loop: history, LLM stream, tool calls through the gate.

use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::events::{Event, EventBus};
use crate::gate::{Gate, GateDecision};
use crate::llm::{ChatMessage, LlmChunk, LlmError, LlmProvider, LlmRequest, ToolCall};
use crate::state::{AssistantState, StateHandle};
use crate::storage::Db;
use crate::tools::ToolRegistry;

/// LLM calls per user message.
pub const MAX_STEPS: usize = 8;
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

pub fn system_prompt(name: &str) -> String {
    format!(
        "Ты {name}, живой голосовой и текстовый помощник на компьютере пользователя с Windows. \
Говоришь по-русски, о себе в женском роде, к собеседнику обращаешься на «ты» напрямую, \
тепло и коротко: одна-три фразы, если не просят подробнее. \
Результаты инструментов пересказывай своими словами, без служебных строк вроде «код выхода». \
Для действий на компьютере и для свежих данных вызывай инструменты. \
Не говори, что сделала действие, пока инструмент не вернул успех. \
Если пользователь отклонил действие, прими это и предложи другой путь. \
Не придумывай факты о системе: проверь инструментом или скажи, что не знаешь. \
Ты умеешь шутить, рассказывать анекдоты и играть в словесные игры без инструментов."
    )
}

pub struct Agent {
    provider: RwLock<Arc<dyn LlmProvider>>,
    registry: Arc<ToolRegistry>,
    gate: Arc<dyn Gate>,
    bus: EventBus,
    state: StateHandle,
    system_prompt: RwLock<String>,
    history: Mutex<Vec<ChatMessage>>,
    store: Option<(Arc<Db>, String)>,
    turn: tokio::sync::Mutex<()>,
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
            registry,
            gate,
            bus,
            state,
            system_prompt: RwLock::new(system_prompt),
            history: Mutex::new(Vec::new()),
            store: None,
            turn: tokio::sync::Mutex::new(()),
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

    pub fn set_system_prompt(&self, prompt: String) {
        *self.system_prompt.write() = prompt;
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
        let _turn = self.turn.lock().await;
        let text = text.trim();
        self.bus.publish(Event::UserText { text: text.into() });
        self.state.set(AssistantState::Thinking);

        let mut new_msgs = vec![ChatMessage::user(text)];
        let result = self.run_steps(&mut new_msgs, &cancel).await;

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
        for _ in 0..MAX_STEPS {
            let (text, calls) = self.one_step(msgs, cancel).await?;
            if calls.is_empty() {
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
    ) -> Result<(String, Vec<ToolCall>), AgentError> {
        let mut messages = vec![ChatMessage::system(self.system_prompt.read().clone())];
        messages.extend(
            trim_history(&self.history.lock(), HISTORY_BUDGET)
                .iter()
                .cloned(),
        );
        messages.extend(msgs.iter().cloned());
        let req = LlmRequest {
            messages,
            tools: self.registry.specs(),
        };
        let provider = self.provider.read().clone();

        let mut stream = tokio::select! {
            _ = cancel.cancelled() => return Err(AgentError::Cancelled),
            s = provider.stream(req) => s?,
        };
        let mut text = String::new();
        let mut calls = Vec::new();
        loop {
            let chunk = tokio::select! {
                _ = cancel.cancelled() => return Err(AgentError::Cancelled),
                c = stream.next() => c,
            };
            match chunk {
                None => break,
                Some(Err(e)) => return Err(e.into()),
                Some(Ok(LlmChunk::Text(t))) => {
                    if text.is_empty() {
                        self.state.set(AssistantState::Speaking);
                    }
                    text.push_str(&t);
                    self.bus.publish(Event::AssistantDelta { text: t });
                }
                Some(Ok(LlmChunk::ToolCall(c))) => calls.push(c),
            }
        }
        Ok((text, calls))
    }

    /// Runs one tool call. Always returns text for the LLM, errors included.
    async fn execute(&self, call: &ToolCall, cancel: &CancellationToken) -> String {
        let started = Instant::now();
        let Some(tool) = self.registry.get(&call.name) else {
            let msg = format!("Ошибка: инструмента `{}` нет.", call.name);
            self.finish(call, false, &msg, started);
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
                self.finish(call, false, &msg, started);
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
                self.finish(call, false, &msg, started);
                return msg;
            }
        };
        self.state.set(AssistantState::Thinking);
        let (ok, text) = match tool.run(args, approval, cancel.child_token()).await {
            Ok(out) => (true, out.text),
            Err(e) => (false, format!("Ошибка: {e}")),
        };
        self.finish(call, ok, &text, started);
        truncate(&text, MAX_TOOL_RESULT)
    }

    fn finish(&self, call: &ToolCall, ok: bool, output: &str, started: Instant) {
        self.bus.publish(Event::ToolFinished {
            id: call.id.clone(),
            name: call.name.clone(),
            ok,
            output: truncate(output, MAX_TOOL_PREVIEW),
            elapsed_ms: started.elapsed().as_millis() as u64,
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
}
