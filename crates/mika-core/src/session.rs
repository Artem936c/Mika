//! Entry point for user text: answers an open question or starts a reply.

use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::confirm::{parse_answer, ConfirmError, Confirmations, DecidedBy};
use crate::events::{Event, EventBus};

pub const ANSWER_HINT: &str =
    "Сначала ответь на вопрос о действии: «да», «разреши всегда» или «нет».";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submitted {
    Empty,
    Answered,
    Hinted,
    Started,
}

pub struct Session {
    pub agent: Arc<Agent>,
    pub confirms: Arc<Confirmations>,
    bus: EventBus,
    cancel: Mutex<CancellationToken>,
}

impl Session {
    pub fn new(agent: Arc<Agent>, confirms: Arc<Confirmations>, bus: EventBus) -> Arc<Self> {
        Arc::new(Self {
            agent,
            confirms,
            bus,
            cancel: Mutex::new(CancellationToken::new()),
        })
    }

    /// Must run inside a tokio runtime: a reply starts as a task.
    pub fn submit(self: &Arc<Self>, text: &str) -> Submitted {
        let text = text.trim();
        if text.is_empty() {
            return Submitted::Empty;
        }
        if let Some(p) = self.confirms.pending() {
            self.bus.publish(Event::UserText { text: text.into() });
            let hint = match parse_answer(text) {
                Some(answer) => match self.confirms.answer(p.id, answer, DecidedBy::Chat) {
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
                    Err(ConfirmError::NotPending) => ANSWER_HINT.to_string(),
                },
                None => ANSWER_HINT.to_string(),
            };
            self.bus.publish(Event::AssistantText { text: hint });
            return Submitted::Hinted;
        }
        let token = self.cancel.lock().clone();
        let this = self.clone();
        let text = text.to_string();
        tokio::spawn(async move {
            let _ = this.agent.handle(&text, token).await;
        });
        Submitted::Started
    }

    /// Stops the running reply, drops queued ones and denies an open question.
    pub fn stop(&self) {
        let mut c = self.cancel.lock();
        c.cancel();
        *c = CancellationToken::new();
        drop(c);
        self.confirms.deny_pending(DecidedBy::Policy);
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
            .any(|e| matches!(e, Event::AssistantText { text } if text.starts_with("итог: "))));
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
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::AssistantText { text } if text == ANSWER_HINT)));
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
}
