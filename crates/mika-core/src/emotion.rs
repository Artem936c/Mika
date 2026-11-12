//! Emotion core: picks one emotion from state, events and reply tags (design, section 9).

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::Receiver;

use crate::events::{Event, EventBus};
use crate::state::AssistantState;
use crate::tools::RiskLevel;

/// How long a one-off emotion stays before the face returns to the state emotion.
pub const PULSE_HOLD: Duration = Duration::from_secs(4);
/// Idle time after which the assistant looks sleepy.
pub const SLEEPY_AFTER: Duration = Duration::from_secs(300);

/// Ten slots, the same for every avatar model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Emotion {
    #[default]
    Neutral,
    Joy,
    Shy,
    Surprise,
    Sad,
    Angry,
    Think,
    Wink,
    Sleepy,
    Worry,
}

impl Emotion {
    pub const ALL: [Emotion; 10] = [
        Emotion::Neutral,
        Emotion::Joy,
        Emotion::Shy,
        Emotion::Surprise,
        Emotion::Sad,
        Emotion::Angry,
        Emotion::Think,
        Emotion::Wink,
        Emotion::Sleepy,
        Emotion::Worry,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Emotion::Neutral => "neutral",
            Emotion::Joy => "joy",
            Emotion::Shy => "shy",
            Emotion::Surprise => "surprise",
            Emotion::Sad => "sad",
            Emotion::Angry => "angry",
            Emotion::Think => "think",
            Emotion::Wink => "wink",
            Emotion::Sleepy => "sleepy",
            Emotion::Worry => "worry",
        }
    }

    /// Slot name from a reply tag: the English slot name or its Russian word, any case.
    pub fn from_tag(tag: &str) -> Option<Emotion> {
        let t = tag.trim().to_lowercase();
        Emotion::ALL
            .into_iter()
            .find(|e| e.as_str() == t)
            .or(match t.as_str() {
                "спокойная" | "спокойно" => Some(Emotion::Neutral),
                "радость" => Some(Emotion::Joy),
                "смущение" => Some(Emotion::Shy),
                "удивление" => Some(Emotion::Surprise),
                "грусть" => Some(Emotion::Sad),
                "злость" | "сердится" => Some(Emotion::Angry),
                "думает" => Some(Emotion::Think),
                "подмигивает" | "подмигивание" => Some(Emotion::Wink),
                "сонная" => Some(Emotion::Sleepy),
                "тревога" => Some(Emotion::Worry),
                _ => None,
            })
    }
}

/// Clicks on the avatar closer together than this count as one series.
pub const POKE_SERIES: Duration = Duration::from_secs(6);

/// Face for the `n`-th click of a series: pleased, shy, playful, surprised, then cross.
pub fn poke_reaction(n: u32) -> Emotion {
    match n {
        0 | 1 => Emotion::Joy,
        2 => Emotion::Shy,
        3 => Emotion::Wink,
        4 => Emotion::Surprise,
        _ => Emotion::Angry,
    }
}

/// Counts clicks on the avatar into series.
#[derive(Debug, Default)]
pub struct Pokes {
    last: Option<(Instant, u32)>,
}

impl Pokes {
    /// The reaction to a click at `now`.
    pub fn poke(&mut self, now: Instant) -> Emotion {
        let n = match self.last {
            Some((t, n)) if now.duration_since(t) < POKE_SERIES => n + 1,
            _ => 1,
        };
        self.last = Some((now, n));
        poke_reaction(n)
    }
}

#[derive(Debug, Clone, Copy)]
struct Pulse {
    emotion: Emotion,
    priority: u8,
    /// `None`: held until cleared.
    until: Option<Instant>,
}

impl Pulse {
    fn active(&self, now: Instant) -> bool {
        self.until.is_none_or(|t| now < t)
    }
}

const PRIORITY_REPLY: u8 = 1;
const PRIORITY_FAILURE: u8 = 2;
const PRIORITY_DANGER: u8 = 3;

/// Pure state machine. Time comes from the caller, so tests need no clock.
#[derive(Debug, Clone)]
pub struct EmotionCore {
    state: AssistantState,
    pulse: Option<Pulse>,
    /// Open high-risk confirmation that holds the worry.
    danger: Option<u64>,
    ok_tools: u32,
    failed: bool,
    last_event: Instant,
}

impl EmotionCore {
    pub fn new(now: Instant) -> Self {
        Self {
            state: AssistantState::Idle,
            pulse: None,
            danger: None,
            ok_tools: 0,
            failed: false,
            last_event: now,
        }
    }

    fn pulse(&mut self, emotion: Emotion, priority: u8, until: Option<Instant>, now: Instant) {
        if self
            .pulse
            .is_some_and(|p| p.active(now) && p.priority > priority)
        {
            return;
        }
        self.pulse = Some(Pulse {
            emotion,
            priority,
            until,
        });
    }

    pub fn on_event(&mut self, event: &Event, now: Instant) {
        if matches!(event, Event::EmotionChanged { .. }) {
            return;
        }
        self.last_event = now;
        let hold = Some(now + PULSE_HOLD);
        match event {
            Event::StateChanged { to, .. } => self.state = *to,
            Event::UserText { .. } => {
                self.ok_tools = 0;
                self.failed = false;
            }
            Event::ToolFinished { ok: true, .. } => self.ok_tools += 1,
            Event::ToolFinished { ok: false, .. } => {
                self.failed = true;
                self.pulse(Emotion::Sad, PRIORITY_FAILURE, hold, now);
            }
            Event::Error { .. } => self.pulse(Emotion::Sad, PRIORITY_FAILURE, hold, now),
            Event::EmotionCue { emotion } if *emotion != Emotion::Neutral => {
                self.pulse(*emotion, PRIORITY_REPLY, hold, now);
            }
            Event::AssistantText { .. } => {
                let busy = self.pulse.is_some_and(|p| p.active(now));
                if self.ok_tools > 0 && !self.failed && !busy {
                    self.pulse(Emotion::Joy, PRIORITY_REPLY, hold, now);
                }
            }
            Event::ConfirmRequested { id, assessment, .. }
                if assessment.level == RiskLevel::High =>
            {
                self.danger = Some(*id);
                self.pulse(Emotion::Worry, PRIORITY_DANGER, None, now);
            }
            Event::ConfirmResolved { id, .. } if self.danger == Some(*id) => {
                self.danger = None;
                self.pulse = None;
            }
            _ => {}
        }
    }

    pub fn current(&self, now: Instant) -> Emotion {
        if let Some(p) = self.pulse.filter(|p| p.active(now)) {
            return p.emotion;
        }
        match self.state {
            AssistantState::Thinking => Emotion::Think,
            AssistantState::Idle if now.duration_since(self.last_event) >= SLEEPY_AFTER => {
                Emotion::Sleepy
            }
            _ => Emotion::Neutral,
        }
    }

    /// When `current` may change without a new event.
    pub fn next_change(&self, now: Instant) -> Option<Instant> {
        match self.pulse.filter(|p| p.active(now)) {
            Some(p) => p.until,
            None if self.state == AssistantState::Idle => {
                Some(self.last_event + SLEEPY_AFTER).filter(|t| *t > now)
            }
            None => None,
        }
    }
}

/// Last emotion published by the driver.
#[derive(Clone, Default)]
pub struct EmotionHandle(Arc<Mutex<Emotion>>);

impl EmotionHandle {
    pub fn get(&self) -> Emotion {
        *self.0.lock()
    }
}

/// Feeds bus events to the core and publishes `Event::EmotionChanged`.
pub struct EmotionDriver {
    bus: EventBus,
    rx: Receiver<Event>,
    handle: EmotionHandle,
}

impl EmotionDriver {
    /// Subscribes right away, so no event is missed before `run` is spawned.
    pub fn new(bus: EventBus) -> Self {
        Self {
            rx: bus.subscribe(),
            bus,
            handle: EmotionHandle::default(),
        }
    }

    pub fn handle(&self) -> EmotionHandle {
        self.handle.clone()
    }

    pub async fn run(mut self) {
        let mut core = EmotionCore::new(Instant::now());
        loop {
            let wake = core.next_change(Instant::now());
            let event = match wake {
                Some(at) => {
                    tokio::select! {
                        e = self.rx.recv() => Some(e),
                        _ = tokio::time::sleep_until(at.into()) => None,
                    }
                }
                None => Some(self.rx.recv().await),
            };
            match event {
                Some(Ok(e)) => core.on_event(&e, Instant::now()),
                Some(Err(RecvError::Lagged(n))) => log::warn!("emotion core lagged by {n} events"),
                Some(Err(RecvError::Closed)) => break,
                None => {}
            }
            let next = core.current(Instant::now());
            let changed = {
                let mut cur = self.handle.0.lock();
                let changed = *cur != next;
                *cur = next;
                changed
            };
            if changed {
                self.bus.publish(Event::EmotionChanged { emotion: next });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm::{Answer, DecidedBy};
    use crate::tools::Assessment;

    #[test]
    fn clicks_in_a_series_end_in_anger_and_a_pause_resets() {
        let t = Instant::now();
        let mut p = Pokes::default();
        let s = Duration::from_secs(1);
        let faces: Vec<Emotion> = (0..6).map(|i| p.poke(t + s * i)).collect();
        assert_eq!(
            faces,
            [
                Emotion::Joy,
                Emotion::Shy,
                Emotion::Wink,
                Emotion::Surprise,
                Emotion::Angry,
                Emotion::Angry
            ]
        );
        assert_eq!(p.poke(t + s * 5 + POKE_SERIES), Emotion::Joy);
    }

    #[test]
    fn a_poke_cue_shows_at_once_when_idle() {
        let t = Instant::now();
        let mut c = EmotionCore::new(t);
        c.on_event(
            &Event::EmotionCue {
                emotion: Emotion::Shy,
            },
            t,
        );
        assert_eq!(c.current(t), Emotion::Shy);
    }

    fn state(to: AssistantState) -> Event {
        Event::StateChanged {
            from: AssistantState::Idle,
            to,
        }
    }

    fn tool(ok: bool) -> Event {
        Event::ToolFinished {
            id: "1".into(),
            name: "t".into(),
            ok,
            output: String::new(),
            elapsed_ms: 1,
            data: None,
        }
    }

    fn confirm(id: u64, level: RiskLevel) -> Event {
        Event::ConfirmRequested {
            id,
            tool: "power".into(),
            assessment: Assessment::new(level, "s", "t", "r"),
            allow_always: false,
            timeout_s: 60,
        }
    }

    fn cue(emotion: Emotion) -> Event {
        Event::EmotionCue { emotion }
    }

    #[test]
    fn thinking_state_shows_think() {
        let t = Instant::now();
        let mut c = EmotionCore::new(t);
        assert_eq!(c.current(t), Emotion::Neutral);
        c.on_event(&state(AssistantState::Thinking), t);
        assert_eq!(c.current(t), Emotion::Think);
        c.on_event(&state(AssistantState::Speaking), t);
        assert_eq!(c.current(t), Emotion::Neutral);
    }

    #[test]
    fn high_risk_confirm_beats_cue() {
        let t = Instant::now();
        let mut c = EmotionCore::new(t);
        c.on_event(&confirm(7, RiskLevel::High), t);
        c.on_event(&cue(Emotion::Joy), t);
        assert_eq!(c.current(t + Duration::from_secs(30)), Emotion::Worry);
        assert_eq!(c.next_change(t), None);
        c.on_event(
            &Event::ConfirmResolved {
                id: 7,
                answer: Answer::Deny,
                by: DecidedBy::Button,
            },
            t,
        );
        assert_eq!(c.current(t), Emotion::Neutral);
    }

    #[test]
    fn medium_risk_confirm_does_not_worry() {
        let t = Instant::now();
        let mut c = EmotionCore::new(t);
        c.on_event(&confirm(1, RiskLevel::Medium), t);
        assert_eq!(c.current(t), Emotion::Neutral);
    }

    #[test]
    fn pulse_expires_to_neutral_after_hold() {
        let t = Instant::now();
        let mut c = EmotionCore::new(t);
        c.on_event(&cue(Emotion::Wink), t);
        assert_eq!(c.current(t + PULSE_HOLD / 2), Emotion::Wink);
        assert_eq!(c.next_change(t), Some(t + PULSE_HOLD));
        assert_eq!(c.current(t + PULSE_HOLD), Emotion::Neutral);
    }

    #[test]
    fn sad_on_failed_tool_and_cue_cannot_replace_it() {
        let t = Instant::now();
        let mut c = EmotionCore::new(t);
        c.on_event(&tool(false), t);
        c.on_event(&cue(Emotion::Joy), t);
        assert_eq!(c.current(t), Emotion::Sad);
    }

    #[test]
    fn joy_after_turn_with_ok_tools_only() {
        let t = Instant::now();
        let reply = Event::AssistantText { text: "ok".into() };

        let mut c = EmotionCore::new(t);
        c.on_event(&reply, t);
        assert_eq!(c.current(t), Emotion::Neutral, "no tools, no joy");

        c.on_event(&Event::UserText { text: "a".into() }, t);
        c.on_event(&tool(true), t);
        c.on_event(&reply, t);
        assert_eq!(c.current(t), Emotion::Joy);

        let later = t + PULSE_HOLD * 2;
        c.on_event(&Event::UserText { text: "b".into() }, later);
        c.on_event(&tool(true), later);
        c.on_event(&tool(false), later);
        c.on_event(&reply, later + PULSE_HOLD);
        assert_eq!(c.current(later + PULSE_HOLD), Emotion::Neutral);
    }

    #[test]
    fn sleepy_after_idle_and_wakes_on_event() {
        let t = Instant::now();
        let mut c = EmotionCore::new(t);
        assert_eq!(c.next_change(t), Some(t + SLEEPY_AFTER));
        assert_eq!(c.current(t + SLEEPY_AFTER), Emotion::Sleepy);
        let later = t + SLEEPY_AFTER + Duration::from_secs(1);
        c.on_event(
            &Event::UserText {
                text: "эй".into()
            },
            later,
        );
        assert_eq!(c.current(later), Emotion::Neutral);
    }

    #[test]
    fn own_changed_event_is_not_activity() {
        let t = Instant::now();
        let mut c = EmotionCore::new(t);
        c.on_event(
            &Event::EmotionChanged {
                emotion: Emotion::Sleepy,
            },
            t + SLEEPY_AFTER,
        );
        assert_eq!(c.current(t + SLEEPY_AFTER), Emotion::Sleepy);
    }

    #[test]
    fn from_tag_accepts_russian_and_case() {
        assert_eq!(Emotion::from_tag("JOY"), Some(Emotion::Joy));
        assert_eq!(Emotion::from_tag(" Радость "), Some(Emotion::Joy));
        assert_eq!(Emotion::from_tag("тревога"), Some(Emotion::Worry));
        assert_eq!(Emotion::from_tag("1"), None);
        assert_eq!(Emotion::from_tag("файл.txt"), None);
        for e in Emotion::ALL {
            assert_eq!(Emotion::from_tag(e.as_str()), Some(e));
        }
    }

    #[test]
    fn changed_event_serializes() {
        let e = Event::EmotionChanged {
            emotion: Emotion::Joy,
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({ "type": "emotion_changed", "emotion": "joy" })
        );
    }

    #[tokio::test]
    async fn driver_publishes_changes_once() {
        let bus = EventBus::new(64);
        let driver = EmotionDriver::new(bus.clone());
        let handle = driver.handle();
        let mut rx = bus.subscribe();
        tokio::spawn(driver.run());

        bus.publish(state(AssistantState::Thinking));
        bus.publish(Event::AssistantDelta { text: "a".into() });
        let mut changes = Vec::new();
        while let Ok(Ok(e)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
            if let Event::EmotionChanged { emotion } = e {
                changes.push(emotion);
            }
        }
        assert_eq!(changes, vec![Emotion::Think]);
        assert_eq!(handle.get(), Emotion::Think);
    }
}
