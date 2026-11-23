//! Event bus shared by every part of the assistant.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::confirm::{Answer, DecidedBy};
use crate::emotion::Emotion;
use crate::schedule::ReminderKind;
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
    /// The files attached to the user message just shown (decision 0053):
    /// the chat draws them under it.
    UserAttachments {
        items: Vec<crate::attach::AttachmentCard>,
    },
    /// Final text of one reply.
    AssistantText {
        text: String,
        /// The same text with voice marks, for the speaker only (decision 0026).
        /// Never serialized; `None` means speak `text`.
        #[serde(skip)]
        spoken: Option<String>,
    },
    /// Streamed piece of the current reply.
    AssistantDelta {
        text: String,
        /// The same piece with voice marks, for the speaker only (decision 0026).
        /// Never serialized; `None` means speak `text`.
        #[serde(skip)]
        spoken: Option<String>,
    },
    /// Another request to the model for the same reply begins, while voice
    /// marks are on: the marks of its text are found anew, so the speaker
    /// ends the text before it (decision 0026).
    StepStarted,
    /// The model asked for the reply that begins now to be said whole,
    /// however long: a joke, a poem, a story (decision 0037). Comes before
    /// the first piece of that reply.
    SayWhole,
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
    /// How far a long tool call has got, 0..=100: converting a video.
    ToolProgress {
        id: String,
        share: u8,
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
    /// A reminder, timer or scheduled task whose time has come.
    ReminderFired {
        id: i64,
        text: String,
        kind: ReminderKind,
        /// Seconds past the due time: more than a few when the app was closed.
        late_s: u64,
    },
    /// A Windows notification of another program, after the privacy filters.
    Notification {
        app: String,
        title: String,
        text: String,
    },
    /// A short line of her own, made without the model: a break, the end of
    /// focus, a meeting soon, a greeting, the summary of the day, a budget
    /// warning, a refused dictation or screen question, what she watched for,
    /// a line about a scenario, the end of a background job. `kind`: `break`,
    /// `focus`, `calendar`, `life`, `day`, `cost`, `dictation`, `screen_ask`,
    /// `watch`, `trigger`, `job`, `stop` (what a full stop ended), `pause`
    /// (what the pause skipped).
    Remark {
        text: String,
        kind: String,
    },
    /// Screen rectangle of the element about to be clicked, physical pixels.
    UiTarget {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    /// How long one stage of a reply took: `stt` (speech to text), `llm` (first
    /// piece of the answer), `tts` (first sound of the spoken answer).
    Timing {
        stage: String,
        ms: u64,
    },
    /// The whole state of the running multi-step task, sent after every change.
    TaskUpdated {
        task: crate::task::Task,
    },
    /// The whole state of a background job, sent after every change.
    BgJobUpdated {
        job: crate::bgjobs::JobView,
    },
    Error {
        message: String,
    },
    /// A phrase of the voice sounds now (decision 0040). `levels` is its
    /// loudness every `frame_ms`, 0..=255 of its loudest moment; empty when
    /// the sound could not be read. `played_ms` is how far the sound already
    /// got: the lips start there. Never written to the journal: it carries
    /// what she says.
    VoicePhrase {
        text: String,
        frame_ms: u32,
        levels: Vec<u8>,
        #[serde(default)]
        played_ms: u32,
    },
    /// The phrase that was sounding is over or was cut short.
    VoiceQuiet,
    /// The last turn did at least three actions without an error: the chat
    /// offers under the reply to save them as a skill (decision 0047). `id`
    /// names the recording, so a late click saves nothing else.
    SkillOffer {
        id: u64,
        actions: usize,
    },
    /// The pause after a full stop or her sleep began or ended (decisions
    /// 0045 and 0050): while it lasts she starts nothing on her own.
    /// `sleeping`: she was asked to sleep; then nothing of hers sounds.
    PauseChanged {
        paused: bool,
        #[serde(default)]
        sleeping: bool,
    },
    /// The avatar makes a gesture (decision 0040): one of her own by `name`,
    /// or an animation the user brought in, by its id in `motion`.
    AvatarGesture {
        name: String,
        motion: Option<String>,
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
    fn a_gesture_names_the_animation_it_plays() {
        let built_in = serde_json::to_value(Event::AvatarGesture {
            name: "wave".into(),
            motion: None,
        })
        .unwrap();
        assert_eq!(
            built_in,
            serde_json::json!({ "type": "avatar_gesture", "name": "wave", "motion": null })
        );
        let own = serde_json::to_value(Event::AvatarGesture {
            name: "Dance".into(),
            motion: Some("0123456789abcdef".into()),
        })
        .unwrap();
        assert_eq!(own["motion"], "0123456789abcdef");
    }

    #[test]
    fn voice_events_serialize_as_snake_case() {
        let phrase = Event::VoicePhrase {
            text: "Привет!".into(),
            frame_ms: 20,
            levels: vec![0, 128, 255],
            played_ms: 35,
        };
        let json = serde_json::to_value(&phrase).unwrap();
        assert_eq!(json["type"], "voice_phrase");
        assert_eq!(json["frame_ms"], 20);
        assert_eq!(json["played_ms"], 35);
        assert_eq!(json["levels"], serde_json::json!([0, 128, 255]));
        let quiet = serde_json::to_value(Event::VoiceQuiet).unwrap();
        assert_eq!(quiet, serde_json::json!({ "type": "voice_quiet" }));
    }

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
    fn spoken_copy_with_marks_is_never_serialized() {
        for e in [
            Event::AssistantDelta {
                text: "Привет".into(),
                spoken: Some("[sigh] <whisper>Привет</whisper>".into()),
            },
            Event::AssistantText {
                text: "Привет".into(),
                spoken: Some("[sigh] <whisper>Привет</whisper>".into()),
            },
        ] {
            let json = serde_json::to_string(&e).unwrap();
            assert!(!json.contains("spoken"), "{json}");
            assert!(
                !json.contains("sigh") && !json.contains("whisper"),
                "{json}"
            );
            match serde_json::from_str::<Event>(&json).unwrap() {
                Event::AssistantDelta { text, spoken } | Event::AssistantText { text, spoken } => {
                    assert_eq!((text.as_str(), spoken), ("Привет", None));
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn tool_progress_serializes_with_id_and_share() {
        let e = Event::ToolProgress {
            id: "c1".into(),
            share: 42,
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({ "type": "tool_progress", "id": "c1", "share": 42 })
        );
    }

    #[test]
    fn bg_job_updated_serializes_with_the_job_object() {
        use crate::bgjobs::{JobState, JobView};
        let mut job = JobView {
            id: 3,
            kind: "video_compress".into(),
            title: "Сжать «отпуск.mp4»".into(),
            state: JobState::Running,
            live: false,
            share: 42,
            note: String::new(),
            text: String::new(),
            data: None,
            silent: false,
            remote: false,
        };
        let e = Event::BgJobUpdated { job: job.clone() };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "type": "bg_job_updated",
                "job": {
                    "id": 3, "kind": "video_compress", "title": "Сжать «отпуск.mp4»",
                    "state": "running", "live": false, "share": 42
                }
            })
        );
        assert_eq!(serde_json::from_value::<Event>(v).unwrap(), e);

        job.state = JobState::Done;
        job.share = 100;
        job.text = "Готово: C:\\v\\отпуск (сжато).mp4, 48,7 МБ.".into();
        job.data = Some(serde_json::json!({
            "kind": "av_files",
            "files": [{ "path": "C:\\v\\отпуск (сжато).mp4", "bytes": 51066000, "duration_s": 751.2 }]
        }));
        let v = serde_json::to_value(Event::BgJobUpdated { job }).unwrap();
        assert_eq!(v["job"]["state"], "done");
        assert_eq!(v["job"]["data"]["files"][0]["bytes"], 51066000);
        assert!(v["job"].get("note").is_none() && v["job"].get("silent").is_none());
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
