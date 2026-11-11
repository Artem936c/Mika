//! Open confirmation requests. One at a time: tools run one after another.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::events::{Event, EventBus};
use crate::policy::forever_block;
use crate::state::{AssistantState, StateHandle};
use crate::tools::Assessment;
#[cfg(test)]
use crate::tools::RiskLevel;

pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Once,
    Always,
    Deny,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    Rule,
    Button,
    Chat,
    Timeout,
    Policy,
}

impl DecidedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedBy::Rule => "rule",
            DecidedBy::Button => "button",
            DecidedBy::Chat => "chat",
            DecidedBy::Timeout => "timeout",
            DecidedBy::Policy => "policy",
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfirmError {
    #[error("этот запрос уже закрыт")]
    NotPending,
    #[error("{0}")]
    ForeverNotAllowed(&'static str),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PendingView {
    pub id: u64,
    pub tool: String,
    pub assessment: Assessment,
    pub allow_always: bool,
}

struct Pending {
    view: PendingView,
    tx: oneshot::Sender<(Answer, DecidedBy)>,
}

pub struct Confirmations {
    bus: EventBus,
    state: StateHandle,
    next_id: AtomicU64,
    pending: Mutex<Option<Pending>>,
}

/// Closes the request if the asking future is dropped (the reply was stopped).
struct DropGuard<'a> {
    owner: &'a Confirmations,
    id: u64,
    done: bool,
}

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut p = self.owner.pending.lock();
        if p.as_ref().is_some_and(|p| p.view.id == self.id) {
            *p = None;
            drop(p);
            self.owner.bus.publish(Event::ConfirmResolved {
                id: self.id,
                answer: Answer::Deny,
                by: DecidedBy::Policy,
            });
        }
    }
}

impl Confirmations {
    pub fn new(bus: EventBus, state: StateHandle) -> Arc<Self> {
        Arc::new(Self {
            bus,
            state,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(None),
        })
    }

    /// Opens a request and waits for an answer or the timeout.
    pub async fn ask(
        &self,
        tool: &str,
        assessment: &Assessment,
        timeout: Duration,
    ) -> (Answer, DecidedBy) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let view = PendingView {
            id,
            tool: tool.into(),
            assessment: assessment.clone(),
            allow_always: forever_block(assessment.level, &assessment.scope).is_none(),
        };
        *self.pending.lock() = Some(Pending {
            view: view.clone(),
            tx,
        });
        let mut guard = DropGuard {
            owner: self,
            id,
            done: false,
        };
        self.state.set(AssistantState::AwaitingConfirm);
        self.bus.publish(Event::ConfirmRequested {
            id,
            tool: tool.into(),
            assessment: assessment.clone(),
            allow_always: view.allow_always,
            timeout_s: timeout.as_secs(),
        });

        let result = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => (Answer::Deny, DecidedBy::Policy),
            Err(_) => (Answer::Timeout, DecidedBy::Timeout),
        };
        {
            let mut p = self.pending.lock();
            if p.as_ref().is_some_and(|p| p.view.id == id) {
                *p = None;
            }
        }
        guard.done = true;
        self.bus.publish(Event::ConfirmResolved {
            id,
            answer: result.0,
            by: result.1,
        });
        self.state.set(AssistantState::Thinking);
        result
    }

    pub fn answer(&self, id: u64, answer: Answer, by: DecidedBy) -> Result<(), ConfirmError> {
        let mut slot = self.pending.lock();
        let Some(p) = slot.as_ref().filter(|p| p.view.id == id) else {
            return Err(ConfirmError::NotPending);
        };
        if answer == Answer::Always {
            if let Some(why) = forever_block(p.view.assessment.level, &p.view.assessment.scope) {
                return Err(ConfirmError::ForeverNotAllowed(why));
            }
        }
        let p = slot.take().expect("checked above");
        p.tx.send((answer, by))
            .map_err(|_| ConfirmError::NotPending)
    }

    pub fn pending(&self) -> Option<PendingView> {
        self.pending.lock().as_ref().map(|p| p.view.clone())
    }

    /// Denies the open request, if any.
    pub fn deny_pending(&self, by: DecidedBy) {
        if let Some(p) = self.pending() {
            let _ = self.answer(p.id, Answer::Deny, by);
        }
    }
}

fn clean(text: &str) -> String {
    text.to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Chat phrases that answer an open request. Only whole phrases count.
pub fn parse_answer(text: &str) -> Option<Answer> {
    const ALWAYS: &[&str] = &[
        "всегда",
        "разреши всегда",
        "разрешаю всегда",
        "разрешать всегда",
        "всегда разрешай",
        "всегда разрешать",
        "да всегда",
    ];
    const ONCE: &[&str] = &[
        "да",
        "ага",
        "ок",
        "окей",
        "ok",
        "yes",
        "подтверждаю",
        "разреши",
        "разрешаю",
        "можно",
        "давай",
        "да давай",
        "да разрешаю",
        "выполняй",
        "делай",
        "один раз",
        "разреши один раз",
        "разрешить один раз",
    ];
    const DENY: &[&str] = &[
        "нет",
        "no",
        "отмена",
        "отмени",
        "не надо",
        "не нужно",
        "отклони",
        "отклоняю",
        "запрещаю",
        "нельзя",
        "стоп",
        "нет не надо",
    ];
    let t = clean(text);
    if ALWAYS.contains(&t.as_str()) {
        Some(Answer::Always)
    } else if ONCE.contains(&t.as_str()) {
        Some(Answer::Once)
    } else if DENY.contains(&t.as_str()) {
        Some(Answer::Deny)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (
        Arc<Confirmations>,
        StateHandle,
        tokio::sync::broadcast::Receiver<Event>,
    ) {
        let bus = EventBus::new(64);
        let rx = bus.subscribe();
        let state = StateHandle::new(bus.clone());
        (Confirmations::new(bus, state.clone()), state, rx)
    }

    fn assessment(level: RiskLevel) -> Assessment {
        Assessment::new(level, "Сделать", "цель", "причина")
    }

    #[test]
    fn parse_answer_phrases() {
        for t in ["да", "Да!", "подтверждаю", "разреши", "ок"] {
            assert_eq!(parse_answer(t), Some(Answer::Once), "{t}");
        }
        for t in ["разреши всегда", "Всегда."] {
            assert_eq!(parse_answer(t), Some(Answer::Always), "{t}");
        }
        for t in ["нет", "Отмена", "не надо", "отклони"] {
            assert_eq!(parse_answer(t), Some(Answer::Deny), "{t}");
        }
        for t in ["да нет наверное", "привет", "удали всё", ""] {
            assert_eq!(parse_answer(t), None, "{t}");
        }
    }

    #[tokio::test]
    async fn answer_resolves_and_publishes() {
        let (c, state, mut rx) = setup();
        let c2 = c.clone();
        let task = tokio::spawn(async move {
            c2.ask("t", &assessment(RiskLevel::Low), CONFIRM_TIMEOUT)
                .await
        });
        tokio::task::yield_now().await;
        while c.pending().is_none() {
            tokio::task::yield_now().await;
        }
        assert_eq!(state.get(), AssistantState::AwaitingConfirm);
        let id = c.pending().unwrap().id;
        c.answer(id, Answer::Once, DecidedBy::Button).unwrap();
        assert_eq!(task.await.unwrap(), (Answer::Once, DecidedBy::Button));
        assert!(c.pending().is_none());
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ConfirmRequested {
                allow_always: true,
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ConfirmResolved {
                answer: Answer::Once,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn high_risk_always_is_rejected_and_request_stays_open() {
        let (c, _state, _rx) = setup();
        let c2 = c.clone();
        let task = tokio::spawn(async move {
            c2.ask("t", &assessment(RiskLevel::High), CONFIRM_TIMEOUT)
                .await
        });
        while c.pending().is_none() {
            tokio::task::yield_now().await;
        }
        let p = c.pending().unwrap();
        assert!(!p.allow_always);
        assert_eq!(
            c.answer(p.id, Answer::Always, DecidedBy::Chat),
            Err(ConfirmError::ForeverNotAllowed(
                crate::policy::HIGH_RISK_FOREVER
            ))
        );
        assert!(c.pending().is_some());
        c.answer(p.id, Answer::Deny, DecidedBy::Chat).unwrap();
        assert_eq!(task.await.unwrap().0, Answer::Deny);
    }

    #[tokio::test]
    async fn wildcard_scope_disables_always() {
        let (c, _state, _rx) = setup();
        let c2 = c.clone();
        let task = tokio::spawn(async move {
            let a = assessment(RiskLevel::Medium).with_scope("c:/docs/*");
            c2.ask("write_file", &a, CONFIRM_TIMEOUT).await
        });
        while c.pending().is_none() {
            tokio::task::yield_now().await;
        }
        let p = c.pending().unwrap();
        assert!(!p.allow_always);
        assert!(matches!(
            c.answer(p.id, Answer::Always, DecidedBy::Button),
            Err(ConfirmError::ForeverNotAllowed(_))
        ));
        c.answer(p.id, Answer::Once, DecidedBy::Button).unwrap();
        assert_eq!(task.await.unwrap().0, Answer::Once);
    }

    #[tokio::test]
    async fn timeout_denies_and_late_answer_is_rejected() {
        let (c, _state, _rx) = setup();
        let (answer, by) = c
            .ask("t", &assessment(RiskLevel::Low), Duration::from_millis(20))
            .await;
        assert_eq!((answer, by), (Answer::Timeout, DecidedBy::Timeout));
        assert_eq!(
            c.answer(1, Answer::Once, DecidedBy::Button),
            Err(ConfirmError::NotPending)
        );
    }

    #[tokio::test]
    async fn dropped_request_is_closed() {
        let (c, _state, mut rx) = setup();
        let a = assessment(RiskLevel::Low);
        {
            let fut = c.ask("t", &a, CONFIRM_TIMEOUT);
            let _ = tokio::time::timeout(Duration::from_millis(10), fut).await;
        }
        assert!(c.pending().is_none());
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ConfirmResolved {
                answer: Answer::Deny,
                by: DecidedBy::Policy,
                ..
            }
        )));
    }
}
