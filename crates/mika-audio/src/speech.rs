//! Turns the reply stream into sentences and hands them to a voice.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use mika_core::events::{Event, EventBus};
use mika_core::schedule::ReminderKind;
use regex::Regex;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;

/// A sentence shorter than this waits for the next one: "т. е." is not a sentence.
const MIN_SENTENCE: usize = 12;
/// A reply without any sentence end is cut here, so speech does not wait for the whole text.
const MAX_SENTENCE: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VoiceInfo {
    pub name: String,
    /// Language tag: `ru-RU`.
    pub language: String,
}

/// Makes the sound of a sentence somewhere else, for example at a speech provider.
/// Blocks until the audio file is ready. Runs on its own thread, never on an async worker.
pub type RemoteSynth = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;

/// Something that can speak. Sentences queue up; `stop` drops the queue at once.
pub trait Voice: Send + Sync {
    fn speak(&self, text: &str);
    fn stop(&self);
    fn voices(&self) -> Vec<VoiceInfo>;
    /// Picks the voice by name (empty: a Russian one if installed) and the rate, 0.5..=2.0.
    fn configure(&self, voice: &str, rate: f32);
    /// With a remote synthesizer the sound of each sentence comes from it; a sentence
    /// it fails on is read by the local voice. `None` returns to the local voice.
    fn set_remote(&self, _remote: Option<RemoteSynth>) {}
    /// Called with the milliseconds from "say this" to the first sound, once for
    /// the first sentence after each `stop`.
    fn set_observer(&self, _observer: Option<FirstSound>) {}
}

/// Gets the time to the first sound of a spoken answer.
pub type FirstSound = Arc<dyn Fn(u64) + Send + Sync>;

/// Splits streamed text into sentences.
#[derive(Debug, Default)]
pub struct SentenceSplitter {
    buf: String,
}

impl SentenceSplitter {
    /// Byte index just past the first sentence that is long enough, if any.
    fn boundary(&self) -> Option<usize> {
        let mut chars = self.buf.char_indices().peekable();
        let mut count = 0usize;
        while let Some((i, c)) = chars.next() {
            count += 1;
            let end = i + c.len_utf8();
            let next = chars.peek().map(|(_, n)| *n);
            let stop = match c {
                '\n' => true,
                '.' | '!' | '?' | '…' => next.is_some_and(char::is_whitespace),
                _ => false,
            };
            if stop && (count >= MIN_SENTENCE || c == '\n') && !self.buf[..end].trim().is_empty() {
                return Some(end);
            }
            if count >= MAX_SENTENCE && c.is_whitespace() {
                return Some(end);
            }
        }
        None
    }

    /// Adds a piece of the reply and returns the sentences it completed.
    pub fn push(&mut self, piece: &str) -> Vec<String> {
        self.buf.push_str(piece);
        let mut out = Vec::new();
        while let Some(end) = self.boundary() {
            let sentence: String = self.buf.drain(..end).collect();
            let sentence = sentence.trim();
            if !sentence.is_empty() {
                out.push(sentence.to_string());
            }
        }
        out
    }

    /// The text left when the reply ends.
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        let rest = rest.trim();
        (!rest.is_empty()).then(|| rest.to_string())
    }
}

/// Text as it should be read aloud: no markup, no links, no emoji.
pub fn clean_for_speech(text: &str) -> String {
    static URL: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    let url = URL.get_or_init(|| Regex::new(r"https?://\S+").unwrap());
    let spaces = SPACES.get_or_init(|| Regex::new(r"\s+").unwrap());
    let text = url.replace_all(text, "ссылка");
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '#' | '_' | '~' | '|' | '>' | '<'))
        .filter(|c| {
            let u = *c as u32;
            // Emoji and pictographs: a synthesizer reads their names or stumbles.
            !(0x1F000..=0x1FAFF).contains(&u)
                && !(0x2600..=0x27BF).contains(&u)
                && !(0xFE00..=0xFE0F).contains(&u)
                && u != 0x200D
        })
        .collect();
    spaces.replace_all(text.trim(), " ").into_owned()
}

/// The "speak replies" switch shared with the settings.
#[derive(Clone, Default)]
pub struct SpeakerSwitch(Arc<AtomicBool>);

impl SpeakerSwitch {
    pub fn new(on: bool) -> Self {
        Self(Arc::new(AtomicBool::new(on)))
    }

    pub fn set(&self, on: bool) {
        self.0.store(on, Ordering::Relaxed);
    }

    pub fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Reads replies, questions about actions and reminders aloud until the bus closes.
pub async fn run_speaker(bus: EventBus, voice: Arc<dyn Voice>, switch: SpeakerSwitch) {
    let mut events = bus.subscribe();
    let mut splitter = SentenceSplitter::default();
    // True once a piece of the current reply has arrived as a delta.
    let mut streamed = false;
    let say = |text: &str| {
        let clean = clean_for_speech(text);
        if !clean.is_empty() {
            voice.speak(&clean);
        }
    };
    loop {
        let event = match events.recv().await {
            Ok(e) => e,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        };
        match event {
            // A new message interrupts whatever she was saying.
            Event::UserText { .. } => {
                voice.stop();
                splitter = SentenceSplitter::default();
                streamed = false;
            }
            Event::Error { .. } => {
                voice.stop();
                splitter = SentenceSplitter::default();
                streamed = false;
            }
            _ if !switch.get() => {}
            Event::AssistantDelta { text } => {
                streamed = true;
                for sentence in splitter.push(&text) {
                    say(&sentence);
                }
            }
            Event::AssistantText { text } => {
                if streamed {
                    if let Some(rest) = splitter.finish() {
                        say(&rest);
                    }
                } else {
                    // A reply that came whole: a hint, an answer without streaming.
                    say(&text);
                }
                streamed = false;
            }
            Event::ToolStarted { .. } => {
                // Words said before a tool call are a finished phrase.
                if let Some(rest) = splitter.finish() {
                    say(&rest);
                }
                streamed = false;
            }
            Event::ConfirmRequested { assessment, .. } => {
                say(&format!("Нужно подтверждение. {}.", assessment.summary));
            }
            Event::ReminderFired {
                text,
                kind: ReminderKind::Remind,
                ..
            } => say(&format!("Напоминаю: {text}")),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::tools::{Assessment, RiskLevel};
    use parking_lot::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct FakeVoice {
        said: Mutex<Vec<String>>,
        stops: Mutex<usize>,
    }

    impl Voice for FakeVoice {
        fn speak(&self, text: &str) {
            self.said.lock().push(text.into());
        }
        fn stop(&self) {
            *self.stops.lock() += 1;
        }
        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }
        fn configure(&self, _voice: &str, _rate: f32) {}
    }

    fn split(pieces: &[&str]) -> Vec<String> {
        let mut s = SentenceSplitter::default();
        let mut out: Vec<String> = pieces.iter().flat_map(|p| s.push(p)).collect();
        out.extend(s.finish());
        out
    }

    #[test]
    fn sentences_come_out_as_soon_as_they_end() {
        let mut s = SentenceSplitter::default();
        assert!(s.push("Сегодня минус десять").is_empty());
        assert_eq!(
            s.push(" и ветер. Оденься"),
            vec!["Сегодня минус десять и ветер."]
        );
        assert_eq!(s.push(" потеплее!\n"), vec!["Оденься потеплее!"]);
        assert_eq!(s.finish(), None);
    }

    #[test]
    fn short_pieces_and_numbers_do_not_split() {
        assert_eq!(
            split(&["Это т. е. пример на 3.5 балла. Конец"]),
            vec!["Это т. е. пример на 3.5 балла.", "Конец"]
        );
        assert_eq!(split(&["Да. Нет. Наверное, завтра получится."]).len(), 1);
    }

    #[test]
    fn a_line_break_ends_a_sentence_and_long_text_is_cut() {
        assert_eq!(
            split(&["Пункт один\nПункт два"]),
            vec!["Пункт один", "Пункт два"]
        );
        let long = "слово ".repeat(200);
        let parts = split(&[&long]);
        assert!(parts.len() >= 2);
        // The cut waits for the end of the word that crosses the limit.
        assert!(parts.iter().all(|p| p.chars().count() <= MAX_SENTENCE + 30));
    }

    #[test]
    fn markup_links_and_emoji_are_not_read() {
        assert_eq!(
            clean_for_speech("**Готово!** Файл `a.txt` тут: https://ya.ru/x?y=1 😀\n\n# Итог"),
            "Готово! Файл a.txt тут: ссылка Итог"
        );
        assert_eq!(clean_for_speech(" 🎉 "), "");
    }

    async fn run(events: Vec<Event>, on: bool) -> Arc<FakeVoice> {
        let bus = EventBus::new(64);
        let voice = Arc::new(FakeVoice::default());
        let job = tokio::spawn(run_speaker(
            bus.clone(),
            voice.clone(),
            SpeakerSwitch::new(on),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        for e in events {
            bus.publish(e);
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        job.abort();
        voice
    }

    fn delta(t: &str) -> Event {
        Event::AssistantDelta { text: t.into() }
    }

    #[tokio::test]
    async fn reply_is_spoken_by_sentences_and_a_new_message_interrupts() {
        let v = run(
            vec![
                Event::UserText {
                    text: "погода".into(),
                },
                delta("Сейчас минус десять градусов. "),
                delta("Оденься **теплее**"),
                Event::AssistantText {
                    text: "Сейчас минус десять градусов. Оденься **теплее**".into(),
                },
                Event::UserText {
                    text: "стоп".into(),
                },
            ],
            true,
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec!["Сейчас минус десять градусов.", "Оденься теплее"]
        );
        assert_eq!(*v.stops.lock(), 2);
    }

    #[tokio::test]
    async fn whole_reply_question_and_reminder_are_spoken() {
        let v = run(
            vec![
                Event::AssistantText {
                    text: "Сначала ответь на вопрос.".into(),
                },
                Event::ConfirmRequested {
                    id: 1,
                    tool: "power".into(),
                    assessment: Assessment::new(RiskLevel::High, "Выключить компьютер", "x", "y"),
                    allow_always: false,
                    timeout_s: 60,
                },
                Event::ReminderFired {
                    id: 1,
                    text: "чайник".into(),
                    kind: ReminderKind::Remind,
                    late_s: 0,
                },
                Event::ReminderFired {
                    id: 2,
                    text: "скрытая задача".into(),
                    kind: ReminderKind::Task,
                    late_s: 0,
                },
            ],
            true,
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec![
                "Сначала ответь на вопрос.",
                "Нужно подтверждение. Выключить компьютер.",
                "Напоминаю: чайник"
            ]
        );
    }

    #[tokio::test]
    async fn switched_off_speaker_stays_silent_but_still_stops() {
        let v = run(
            vec![
                delta("Это не должно прозвучать. "),
                Event::AssistantText {
                    text: "Это не должно прозвучать.".into(),
                },
                Event::UserText { text: "x".into() },
            ],
            false,
        )
        .await;
        assert!(v.said.lock().is_empty());
        assert_eq!(*v.stops.lock(), 1);
    }
}
