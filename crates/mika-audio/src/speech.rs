//! Turns the reply stream into sentences and hands them to a voice.

use std::future::Future;
use std::pin::Pin;
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
    /// True while queued sentences are still to be said.
    fn busy(&self) -> bool {
        false
    }
    /// Pitch and pace in percent for each new sentence of the local voice, so
    /// her tone follows her mood. `None`: always the usual voice.
    fn set_tone(&self, _tone: Option<ToneSource>) {}
}

/// Pitch and pace in percent of the usual, asked for each sentence.
pub type ToneSource = Arc<dyn Fn() -> (i8, i8) + Send + Sync>;

/// SSML that says `text` with the pitch and pace changed by these percents.
pub fn prosody_ssml(text: &str, language: &str, (pitch, rate): (i8, i8)) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    let lang = if language.trim().is_empty() {
        "ru-RU"
    } else {
        language.trim()
    };
    format!(
        "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"{lang}\">\
<prosody pitch=\"{pitch:+}%\" rate=\"{rate:+}%\">{escaped}</prosody></speak>"
    )
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

/// Whether anyone can hear her now: false while the system sound is muted.
pub type Audible = Arc<dyn Fn() -> bool + Send + Sync>;

/// Makes a short retelling of a long reply for the voice; `None` when it fails.
pub type Summarize =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// A reply longer than this, characters, is not read out when retelling is on.
pub const LONG_REPLY: usize = 300;

/// The "speak replies" switch shared with the settings, and how long replies sound.
#[derive(Clone, Default)]
pub struct SpeakerSwitch {
    on: Arc<AtomicBool>,
    audible: Option<Audible>,
    /// Long replies are retold briefly instead of read out.
    brief: Arc<AtomicBool>,
    summarize: Option<Summarize>,
}

impl SpeakerSwitch {
    pub fn new(on: bool) -> Self {
        Self {
            on: Arc::new(AtomicBool::new(on)),
            ..Default::default()
        }
    }

    /// Speech is not made while `audible` says nobody would hear it: a paid
    /// synthesizer is not asked for a reply that plays into silence.
    pub fn with_audible(mut self, audible: Audible) -> Self {
        self.audible = Some(audible);
        self
    }

    /// Who retells a long reply. Without it she says its first sentence.
    pub fn with_summarizer(mut self, summarize: Summarize) -> Self {
        self.summarize = Some(summarize);
        self
    }

    pub fn set(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
    }

    pub fn get(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn set_brief(&self, on: bool) {
        self.brief.store(on, Ordering::Relaxed);
    }

    pub fn brief(&self) -> bool {
        self.brief.load(Ordering::Relaxed)
    }

    fn heard(&self) -> bool {
        self.audible.as_ref().is_none_or(|a| a())
    }
}

/// What she says instead of a long reply when no retelling came: its first
/// sentence and a pointer to the chat.
pub fn fallback_brief(reply: &str) -> String {
    let mut s = SentenceSplitter::default();
    let first = s
        .push(reply)
        .into_iter()
        .map(|x| clean_for_speech(&x))
        .find(|x| !x.is_empty())
        .or_else(|| s.finish().map(|x| clean_for_speech(&x)))
        .unwrap_or_default();
    let first: String = first.chars().take(LONG_REPLY).collect();
    if first.is_empty() {
        "Ответ в чате.".into()
    } else {
        format!("{first} Подробности в чате.")
    }
}

/// The reply being spoken and what is known about it so far.
#[derive(Default)]
struct Turn {
    splitter: SentenceSplitter,
    /// Sentences held back until it is clear the reply is short.
    held: Vec<String>,
    /// Characters of the current piece of the reply.
    chars: usize,
    /// The reply grew long: nothing of it is read out.
    long: bool,
    /// A multi-step task runs: steps are not spoken, only the end.
    task: bool,
}

/// Reads replies, questions about actions and reminders aloud until the bus closes.
///
/// With retelling on, a short reply is said whole once it ends, and a long one
/// is replaced by a short retelling. During a multi-step task nothing of the
/// steps is said: only the final reply, or why the task broke off.
pub async fn run_speaker(bus: EventBus, voice: Arc<dyn Voice>, switch: SpeakerSwitch) {
    let mut events = bus.subscribe();
    let mut turn = Turn::default();
    // True once a piece of the current reply has arrived as a delta.
    let mut streamed = false;
    // Grows with every new message: a retelling of an older reply is dropped.
    let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let say = |text: &str| {
        let clean = clean_for_speech(text);
        if !clean.is_empty() && switch.heard() {
            voice.speak(&clean);
        }
    };
    let retell = |reply: String| {
        let gen = generation.load(Ordering::Relaxed);
        let (voice, switch, generation) = (voice.clone(), switch.clone(), generation.clone());
        let summarize = switch.summarize.clone();
        tokio::spawn(async move {
            let short = match summarize {
                Some(s) => s(reply.clone()).await,
                None => None,
            };
            let text = short
                .map(|s| clean_for_speech(&s))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| fallback_brief(&reply));
            if generation.load(Ordering::Relaxed) == gen && switch.get() && switch.heard() {
                voice.speak(&text);
            }
        });
    };
    loop {
        let event = match events.recv().await {
            Ok(e) => e,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        };
        let brief = switch.brief();
        match event {
            // A new message interrupts whatever she was saying.
            Event::UserText { .. } => {
                voice.stop();
                generation.fetch_add(1, Ordering::Relaxed);
                turn = Turn::default();
                streamed = false;
            }
            Event::Error { message } => {
                voice.stop();
                generation.fetch_add(1, Ordering::Relaxed);
                let broke_off = turn.task;
                turn = Turn::default();
                streamed = false;
                if broke_off && switch.get() {
                    say(&format!("Задача оборвалась. {message}"));
                }
            }
            Event::TaskUpdated { task } => {
                if !task.finished && !turn.task {
                    // What was waiting to be said belongs to the steps now.
                    turn.held.clear();
                    turn.splitter = SentenceSplitter::default();
                }
                // Stays set to the end of the turn: the final reply comes whole.
                turn.task = true;
            }
            _ if !switch.get() => {}
            Event::AssistantDelta { .. } if turn.task => streamed = true,
            Event::AssistantDelta { text } => {
                streamed = true;
                let sentences = turn.splitter.push(&text);
                if !brief {
                    for sentence in sentences {
                        say(&sentence);
                    }
                    continue;
                }
                turn.chars += text.chars().count();
                if turn.chars > LONG_REPLY {
                    turn.long = true;
                    turn.held.clear();
                }
                if !turn.long {
                    turn.held.extend(sentences);
                }
            }
            Event::AssistantText { text } => {
                let whole = !streamed || turn.task;
                if !brief {
                    if whole {
                        say(&text);
                    } else if let Some(rest) = turn.splitter.finish() {
                        say(&rest);
                    }
                } else if whole {
                    if text.chars().count() > LONG_REPLY {
                        retell(text);
                    } else {
                        say(&text);
                    }
                } else if turn.long {
                    retell(text);
                } else {
                    for sentence in turn.held.drain(..) {
                        say(&sentence);
                    }
                    if let Some(rest) = turn.splitter.finish() {
                        say(&rest);
                    }
                }
                turn = Turn::default();
                streamed = false;
            }
            Event::ToolStarted { .. } if turn.task => {}
            Event::ToolStarted { .. } => {
                // Words said before a tool call are a finished phrase, unless long.
                let rest = turn.splitter.finish();
                if !turn.long {
                    for sentence in turn.held.drain(..) {
                        say(&sentence);
                    }
                    if let Some(rest) = rest {
                        say(&rest);
                    }
                }
                turn.held.clear();
                turn.chars = 0;
                turn.long = false;
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
    fn tone_goes_into_ssml_and_text_is_escaped() {
        assert_eq!(
            prosody_ssml("A & <b>", "", (8, -6)),
            "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"ru-RU\">\
<prosody pitch=\"+8%\" rate=\"-6%\">A &amp; &lt;b&gt;</prosody></speak>"
        );
        assert!(prosody_ssml("x", "en-US", (0, 0)).contains("xml:lang=\"en-US\""));
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

    #[tokio::test]
    async fn nothing_is_synthesized_while_the_system_sound_is_muted() {
        let bus = EventBus::new(64);
        let voice = Arc::new(FakeVoice::default());
        let muted = Arc::new(AtomicBool::new(true));
        let m = muted.clone();
        let switch =
            SpeakerSwitch::new(true).with_audible(Arc::new(move || !m.load(Ordering::Relaxed)));
        let job = tokio::spawn(run_speaker(bus.clone(), voice.clone(), switch));
        tokio::time::sleep(Duration::from_millis(10)).await;
        bus.publish(Event::AssistantText {
            text: "В тишину.".into(),
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(voice.said.lock().is_empty());
        muted.store(false, Ordering::Relaxed);
        bus.publish(Event::AssistantText {
            text: "Теперь слышно.".into(),
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        job.abort();
        assert_eq!(*voice.said.lock(), ["Теперь слышно."]);
    }

    async fn run_with(events: Vec<Event>, switch: SpeakerSwitch) -> Arc<FakeVoice> {
        let bus = EventBus::new(64);
        let voice = Arc::new(FakeVoice::default());
        let job = tokio::spawn(run_speaker(bus.clone(), voice.clone(), switch));
        tokio::time::sleep(Duration::from_millis(10)).await;
        for e in events {
            bus.publish(e);
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        job.abort();
        voice
    }

    fn brief_switch() -> SpeakerSwitch {
        let s = SpeakerSwitch::new(true).with_summarizer(Arc::new(|reply: String| {
            Box::pin(async move {
                Some(format!("Коротко: {} знаков.", reply.chars().count()))
            })
        }));
        s.set_brief(true);
        s
    }

    fn task(finished: bool) -> Event {
        Event::TaskUpdated {
            task: mika_core::task::Task {
                title: "Задача".into(),
                steps: Vec::new(),
                finished,
                ok: finished,
            },
        }
    }

    #[tokio::test]
    async fn a_short_reply_is_said_whole_and_a_long_one_is_retold() {
        let short = "Сейчас минус десять градусов. Оденься теплее.";
        let v = run_with(
            vec![delta(short), Event::AssistantText { text: short.into() }],
            brief_switch(),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec!["Сейчас минус десять градусов.", "Оденься теплее."]
        );

        let long = "Это очень длинный ответ про погоду на неделю. ".repeat(10);
        let v = run_with(
            vec![delta(&long), Event::AssistantText { text: long.clone() }],
            brief_switch(),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec![format!("Коротко: {} знаков.", long.chars().count())]
        );
    }

    #[tokio::test]
    async fn steps_of_a_task_are_silent_and_only_the_end_is_said() {
        let v = run_with(
            vec![
                task(false),
                delta("Шаг один готов. "),
                Event::ToolStarted {
                    id: "1".into(),
                    name: "open_app".into(),
                    summary: "Открыть".into(),
                },
                delta("Шаг два готов. "),
                task(true),
                Event::AssistantText {
                    text: "Всё сделала.".into(),
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(*v.said.lock(), vec!["Всё сделала."]);

        // A task that broke off says why.
        let v = run_with(
            vec![
                task(false),
                delta("Шаг один. "),
                Event::Error {
                    message: "слишком много шагов подряд".into(),
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec!["Задача оборвалась. слишком много шагов подряд"]
        );
    }

    #[test]
    fn without_a_retelling_the_first_sentence_points_to_the_chat() {
        assert_eq!(
            fallback_brief("**Готово!** Вот подробный план на неделю. Пункт первый."),
            "Готово! Вот подробный план на неделю. Подробности в чате."
        );
        assert_eq!(fallback_brief("```код```"), "код Подробности в чате.");
    }
}
