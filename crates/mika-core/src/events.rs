//! Event bus shared by every part of the assistant.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::confirm::{Answer, DecidedBy};
use crate::emotion::Emotion;
use crate::state::AssistantState;
use crate::tools::Assessment;

/// Everything that happens in the assistant goes through the bus as one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    StateChanged {
        from: AssistantState,
        to: AssistantState,
    },
    UserText {
        text: String,
    },
    /// Final text of one reply.
    AssistantText {
        text: String,
    },
    /// Streamed piece of the current reply.
    AssistantDelta {
        text: String,
    },
    ToolStarted {
        id: String,
        name: String,
        summary: String,
    },
    ToolFinished {
        id: String,
        name: String,
        ok: bool,
        output: String,
        elapsed_ms: u64,
        /// Structured result for a chat card: weather, volume.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<serde_json::Value>,
    },
    ConfirmRequested {
        id: u64,
        tool: String,
        assessment: Assessment,
        allow_always: bool,
        timeout_s: u64,
    },
    ConfirmResolved {
        id: u64,
        answer: Answer,
        by: DecidedBy,
    },
    /// Emotion tag found in the reply, already cut out of the text.
    EmotionCue {
        emotion: Emotion,
    },
    /// The face to show now, chosen by the emotion core.
    EmotionChanged {
        emotion: Emotion,
    },
    Error {
        message: String,
    },
}

/// Broadcast bus. Cloning gives another handle to the same channel.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Returns the number of subscribers that received the event, 0 without subscribers.
    pub fn publish(&self, event: Event) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_without_subscribers_returns_zero() {
        let bus = EventBus::new(8);
        assert_eq!(bus.publish(Event::UserText { text: "a".into() }), 0);
    }

    #[tokio::test]
    async fn two_subscribers_both_receive() {
        let bus = EventBus::new(8);
        let (mut a, mut b) = (bus.subscribe(), bus.subscribe());
        assert_eq!(
            bus.publish(Event::UserText {
                text: "привет".into()
            }),
            2
        );
        assert_eq!(
            a.recv().await.unwrap(),
            Event::UserText {
                text: "привет".into()
            }
        );
        assert_eq!(
            b.recv().await.unwrap(),
            Event::UserText {
                text: "привет".into()
            }
        );
    }

    #[tokio::test]
    async fn lagged_subscriber_gets_lagged_error_and_recovers() {
        use tokio::sync::broadcast::error::RecvError;
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        for i in 0..3 {
            bus.publish(Event::UserText {
                text: i.to_string(),
            });
        }
        assert!(matches!(rx.recv().await, Err(RecvError::Lagged(1))));
        assert_eq!(
            rx.recv().await.unwrap(),
            Event::UserText { text: "1".into() }
        );
    }

    #[test]
    fn event_serializes_with_snake_case_tag() {
        let e = Event::StateChanged {
            from: AssistantState::Idle,
            to: AssistantState::AwaitingConfirm,
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({ "type": "state_changed", "from": "idle", "to": "awaiting_confirm" })
        );
    }

    #[test]
    fn tool_finished_without_data_has_no_field() {
        let mut e = Event::ToolFinished {
            id: "1".into(),
            name: "t".into(),
            ok: true,
            output: "o".into(),
            elapsed_ms: 5,
            data: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert!(v.get("data").is_none(), "{v}");
        assert_eq!(serde_json::from_value::<Event>(v).unwrap(), e);

        if let Event::ToolFinished { data, .. } = &mut e {
            *data = Some(serde_json::json!({"kind": "volume", "level": 40}));
        }
        assert_eq!(serde_json::to_value(&e).unwrap()["data"]["level"], 40);
    }
}
