//! Emotion core: picks one emotion from state, events and reply tags (design, section 9).

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::Receiver;

use crate::config::Persona;
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

    /// Pitch and pace of the local voice for this emotion, in percent of the usual.
    pub fn prosody(self) -> (i8, i8) {
        match self {
            Emotion::Neutral | Emotion::Think => (0, 0),
            Emotion::Joy => (8, 6),
            Emotion::Wink => (6, 4),
            Emotion::Surprise => (12, 8),
            Emotion::Shy => (4, -6),
            Emotion::Sad => (-8, -12),
            Emotion::Sleepy => (-6, -15),
            Emotion::Worry => (4, 6),
            Emotion::Angry => (-4, 8),
        }
    }

    /// One line about the tone for a provider voice that takes instructions.
    pub fn tone_line(self) -> Option<&'static str> {
        Some(match self {
            Emotion::Neutral | Emotion::Think => return None,
            Emotion::Joy => "Сейчас радостно, с улыбкой в голосе.",
            Emotion::Wink => "Сейчас игриво и с лёгкой хитринкой.",
            Emotion::Surprise => "Сейчас удивлённо и живо.",
            Emotion::Shy => "Сейчас смущённо, чуть тише и мягче.",
            Emotion::Sad => "Сейчас грустно и тихо, медленнее.",
            Emotion::Sleepy => "Сейчас сонно и неторопливо.",
            Emotion::Worry => "Сейчас встревоженно и серьёзно.",
            Emotion::Angry => "Сейчас недовольно, но без грубости.",
        })
    }

    /// The mood as a direction for a voice that performs marks (decision 0029).
    /// Every value fits `marks::is_direction`.
    pub fn direction(self) -> Option<&'static str> {
        Some(match self {
            Emotion::Neutral | Emotion::Think => return None,
            Emotion::Joy => "cheerful, with a smile",
            Emotion::Wink => "playful, teasing",
            Emotion::Surprise => "surprised, lively",
            Emotion::Shy => "shy, softly",
            Emotion::Sad => "sad, quietly",
            Emotion::Sleepy => "sleepy, slowly",
            Emotion::Worry => "worried, serious",
            Emotion::Angry => "annoyed, firmly",
        })
    }

    /// Slot name from a reply tag: the English slot name or its Russian word, any case.
    pub fn from_tag(tag: &str) -> Option<Emotion> {
        let t = tag.trim().to_lowercase();
        Emotion::ALL
            .into_iter()
            .find(|e| e.as_str() == t)
            .or(match t.as_str() {
                "спокойная" | "спокойно" | "calm" => Some(Emotion::Neutral),
                "радость" | "улыбка" | "smile" | "happy" | "laugh" | "grin" | "excited" => {
                    Some(Emotion::Joy)
                }
                "смущение" | "blush" | "embarrassed" => Some(Emotion::Shy),
                "удивление" | "surprised" | "wow" | "shocked" => Some(Emotion::Surprise),
                "грусть" | "cry" | "crying" | "sorry" => Some(Emotion::Sad),
                "злость" | "сердится" | "mad" => Some(Emotion::Angry),
                "думает" | "thinking" | "hmm" => Some(Emotion::Think),
                "подмигивает" | "подмигивание" => Some(Emotion::Wink),
                "сонная" | "tired" => Some(Emotion::Sleepy),
                "тревога" | "worried" | "nervous" | "scared" => Some(Emotion::Worry),
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

/// The part of her a click on a living model hit (decision 0040). Chest,
/// hips and legs are never told apart: they are `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchPart {
    /// The top of her head: she is patted.
    Crown,
    /// Her face or cheek.
    Face,
    /// An arm or a hand.
    Hand,
    Other,
}

/// Counts clicks on the avatar into series.
#[derive(Debug, Default)]
pub struct Pokes {
    last: Option<(Instant, u32)>,
}

impl Pokes {
    /// The reaction to a click at `now` on `part` of her, in the manner of her
    /// character (decision 0055). The first two clicks of a series answer the
    /// part; from the third the series decides, the same for every character.
    pub fn poke(&mut self, now: Instant, part: Option<TouchPart>, persona: Persona) -> Emotion {
        let n = match self.last {
            Some((t, n)) if now.duration_since(t) < POKE_SERIES => n + 1,
            _ => 1,
        };
        self.last = Some((now, n));
        match part {
            Some(part) if n <= 2 => touch_face(part, persona),
            _ => poke_reaction(n),
        }
    }
}

/// Her face for a click on `part`, by her character. An own character
/// answers as the caring one. A click on her body never pleases her: she
/// startles, and the bold one is cross at once.
pub fn touch_face(part: TouchPart, persona: Persona) -> Emotion {
    use Emotion::*;
    use Persona as P;
    let persona = if persona == P::Custom {
        P::Caring
    } else {
        persona
    };
    match (part, persona) {
        (TouchPart::Crown, P::Bold | P::Business) => Shy,
        (TouchPart::Crown, P::Sarcastic) => Wink,
        (TouchPart::Crown, P::Sleepy) => Sleepy,
        (TouchPart::Crown, _) => Joy,
        (TouchPart::Face, P::Cheerful) => Joy,
        (TouchPart::Face, P::Bold) => Angry,
        (TouchPart::Face, P::Business) => Surprise,
        (TouchPart::Face, P::Sarcastic) => Wink,
        (TouchPart::Face, P::Sleepy) => Sleepy,
        (TouchPart::Face, _) => Shy,
        (TouchPart::Hand, P::Bold | P::Sarcastic) => Wink,
        (TouchPart::Hand, P::Sleepy) => Sleepy,
        (TouchPart::Hand, _) => Joy,
        (TouchPart::Other, P::Bold) => Angry,
        (TouchPart::Other, _) => Surprise,
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
        let faces: Vec<Emotion> = (0..6)
            .map(|i| p.poke(t + s * i, None, Persona::Caring))
            .collect();
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
        assert_eq!(
            p.poke(t + s * 5 + POKE_SERIES, None, Persona::Caring),
            Emotion::Joy
        );
    }

    #[test]
    fn the_first_two_clicks_answer_the_part_of_her_body() {
        let t = Instant::now();
        let s = Duration::from_secs(1);
        let mut p = Pokes::default();
        assert_eq!(
            p.poke(t, Some(TouchPart::Face), Persona::Caring),
            Emotion::Shy
        );
        assert_eq!(
            p.poke(t + s, Some(TouchPart::Crown), Persona::Caring),
            Emotion::Joy
        );
        // From the third click of a series the series decides, whatever the part.
        assert_eq!(
            p.poke(t + s * 2, Some(TouchPart::Crown), Persona::Caring),
            Emotion::Wink
        );
        assert_eq!(
            p.poke(t + s * 3, Some(TouchPart::Face), Persona::Caring),
            Emotion::Surprise
        );
        assert_eq!(
            p.poke(t + s * 4, Some(TouchPart::Hand), Persona::Caring),
            Emotion::Angry
        );

        let mut p = Pokes::default();
        assert_eq!(
            p.poke(t, Some(TouchPart::Hand), Persona::Caring),
            Emotion::Joy
        );
    }

    #[test]
    fn a_click_on_her_body_startles_her_rather_than_pleases() {
        let t = Instant::now();
        let s = Duration::from_secs(1);
        let mut p = Pokes::default();
        assert_eq!(
            p.poke(t, Some(TouchPart::Other), Persona::Caring),
            Emotion::Surprise
        );
        assert_eq!(
            p.poke(t + s, Some(TouchPart::Other), Persona::Caring),
            Emotion::Surprise
        );
        let mut p = Pokes::default();
        assert_eq!(
            p.poke(t, Some(TouchPart::Other), Persona::Bold),
            Emotion::Angry
        );
    }

    #[test]
    fn each_character_answers_a_click_in_her_own_way() {
        let t = Instant::now();
        let first = |part, persona| Pokes::default().poke(t, Some(part), persona);
        assert_eq!(first(TouchPart::Crown, Persona::Bold), Emotion::Shy);
        assert_eq!(first(TouchPart::Crown, Persona::Sarcastic), Emotion::Wink);
        assert_eq!(first(TouchPart::Crown, Persona::Sleepy), Emotion::Sleepy);
        assert_eq!(first(TouchPart::Face, Persona::Cheerful), Emotion::Joy);
        assert_eq!(first(TouchPart::Face, Persona::Bold), Emotion::Angry);
        assert_eq!(first(TouchPart::Face, Persona::Business), Emotion::Surprise);
        assert_eq!(first(TouchPart::Hand, Persona::Sarcastic), Emotion::Wink);
        // An own character answers as the caring one.
        assert_eq!(
            first(TouchPart::Face, Persona::Custom),
            first(TouchPart::Face, Persona::Caring)
        );
    }

    #[test]
    fn every_character_ends_a_long_series_cross() {
        let t = Instant::now();
        let s = Duration::from_secs(1);
        for persona in Persona::READY {
            let mut p = Pokes::default();
            let last = (0..5)
                .map(|i| p.poke(t + s * i, Some(TouchPart::Crown), persona))
                .last();
            assert_eq!(last, Some(Emotion::Angry), "{persona:?}");
        }
    }

    #[test]
    fn a_part_of_the_body_is_read_in_snake_case() {
        let part: TouchPart = serde_json::from_str("\"crown\"").unwrap();
        assert_eq!(part, TouchPart::Crown);
        assert!(serde_json::from_str::<TouchPart>("\"chest\"").is_err());
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
        let reply = Event::AssistantText {
            text: "ok".into(),
            spoken: None,
        };

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
        assert_eq!(Emotion::from_tag("smile"), Some(Emotion::Joy));
        assert_eq!(Emotion::from_tag("Thinking"), Some(Emotion::Think));
        assert_eq!(Emotion::from_tag("1"), None);
        assert_eq!(Emotion::from_tag("файл.txt"), None);
        for e in Emotion::ALL {
            assert_eq!(Emotion::from_tag(e.as_str()), Some(e));
        }
    }

    #[test]
    fn calm_faces_keep_the_usual_voice_and_moods_change_it() {
        assert_eq!(Emotion::Neutral.prosody(), (0, 0));
        assert_eq!(Emotion::Neutral.tone_line(), None);
        assert!(Emotion::Joy.prosody().0 > 0 && Emotion::Sad.prosody().1 < 0);
        for e in Emotion::ALL {
            let (pitch, rate) = e.prosody();
            assert!(pitch.abs() <= 20 && rate.abs() <= 20, "{e:?}");
        }
    }

    #[test]
    fn every_mood_has_a_direction_the_voice_reads_as_one() {
        use crate::marks::{is_direction, tokens, Token, MAX_DIRECTION};
        assert_eq!(Emotion::Neutral.direction(), None);
        assert_eq!(Emotion::Think.direction(), None);
        let mut seen = Vec::new();
        for e in Emotion::ALL {
            let Some(d) = e.direction() else {
                assert!(matches!(e, Emotion::Neutral | Emotion::Think), "{e:?}");
                continue;
            };
            assert!(is_direction(d), "{d}");
            // The settings text promises at most 25 characters per phrase.
            assert!(d.chars().count() + 3 <= 25, "{d}");
            assert!(d.chars().count() <= MAX_DIRECTION);
            // The parser of decision 0026 reads it as a direction, not as text.
            let read = tokens(&format!("[{d}] Привет"));
            assert_eq!(read[0], Token::Direction(d.to_string()), "{d}");
            assert_eq!(read[1], Token::Text(" Привет".into()), "{d}");
            // Not an emotion tag: the face is not changed by her own direction.
            assert!(Emotion::from_tag(d).is_none(), "{d}");
            assert!(!seen.contains(&d), "{d} twice");
            seen.push(d);
        }
        assert_eq!(seen.len(), 8);
        // A mood with a tone line has a direction and the other way round.
        for e in Emotion::ALL {
            assert_eq!(e.direction().is_some(), e.tone_line().is_some(), "{e:?}");
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
        bus.publish(Event::AssistantDelta {
            text: "a".into(),
            spoken: None,
        });
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
