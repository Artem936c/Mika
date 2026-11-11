//! Assistant state with change notifications on the event bus.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::events::{Event, EventBus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantState {
    Idle,
    Listening,
    Thinking,
    Speaking,
    AwaitingConfirm,
}

impl AssistantState {
    pub fn as_str(self) -> &'static str {
        match self {
            AssistantState::Idle => "idle",
            AssistantState::Listening => "listening",
            AssistantState::Thinking => "thinking",
            AssistantState::Speaking => "speaking",
            AssistantState::AwaitingConfirm => "awaiting_confirm",
        }
    }
}

/// Shared handle to the current state. Every change is published as `Event::StateChanged`.
#[derive(Clone)]
pub struct StateHandle {
    inner: Arc<Mutex<AssistantState>>,
    bus: EventBus,
}

impl StateHandle {
    pub fn new(bus: EventBus) -> Self {
        Self {
            inner: Arc::new(Mutex::new(AssistantState::Idle)),
            bus,
        }
    }

    pub fn get(&self) -> AssistantState {
        *self.inner.lock()
    }

    /// Returns false when the state did not change.
    pub fn set(&self, next: AssistantState) -> bool {
        // Change and publish under one lock so events keep the order of changes.
        let mut cur = self.inner.lock();
        if *cur == next {
            return false;
        }
        let from = *cur;
        *cur = next;
        self.bus.publish(Event::StateChanged { from, to: next });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_publishes_state_changed() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        let state = StateHandle::new(bus);
        assert_eq!(state.get(), AssistantState::Idle);
        assert!(state.set(AssistantState::Listening));
        assert_eq!(state.get(), AssistantState::Listening);
        assert_eq!(
            rx.recv().await.unwrap(),
            Event::StateChanged {
                from: AssistantState::Idle,
                to: AssistantState::Listening
            }
        );
    }

    #[test]
    fn set_same_value_returns_false_and_publishes_nothing() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        let state = StateHandle::new(bus);
        assert!(!state.set(AssistantState::Idle));
        assert!(rx.try_recv().is_err());
    }
}
