//! Turns the reply stream into sentences and hands them to a voice.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use mika_core::events::{Event, EventBus};
use mika_core::schedule::ReminderKind;
use mika_core::state::AssistantState;
use parking_lot::Mutex;
use regex::Regex;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Notify;

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
    /// A lock, so a stop never slips between the check and the queueing of a
    /// retelling.
    retellings: Arc<Mutex<Retellings>>,
    /// Wakes the retellings being made when they are dropped: they stop
    /// waiting for the model at once.
    dropped: Arc<Notify>,
}

/// Retellings being made and which of them still count.
#[derive(Default)]
struct Retellings {
    /// Grows on every stop and new message: a retelling asked for before it
    /// is dropped.
    generation: u64,
    /// Retellings of this generation not yet said.
    pending: usize,
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

    /// Whether anyone would hear her now: false while the system sound is off.
    pub fn heard(&self) -> bool {
        self.audible.as_ref().is_none_or(|a| a())
    }

    /// Drops the retellings still being made: «Стоп», a new message, the
    /// microphone opening.
    /// They stop counting as speech at once, not when the model answers.
    pub fn cancel(&self) {
        {
            let mut r = self.retellings.lock();
            r.generation += 1;
            r.pending = 0;
        }
        self.dropped.notify_waiters();
    }

    /// True while a retelling is being made: she has not finished speaking.
    pub fn retelling(&self) -> bool {
        self.retellings.lock().pending > 0
    }

    /// `voice` as the rest of the app should use it: a retelling still being
    /// made counts as speech, so nobody listens or turns other programs up
    /// over it, and `stop` drops it too.
    pub fn wrap(&self, voice: Arc<dyn Voice>) -> Arc<dyn Voice> {
        Arc::new(Retold {
            inner: voice,
            switch: self.clone(),
        })
    }

    /// Says a short retelling of a long reply once it is made, unless she was
    /// stopped meanwhile. Until then she counts as speaking.
    fn retell(&self, voice: Arc<dyn Voice>, reply: String) {
        // Nobody would hear it: the model is not asked either.
        if !self.heard() {
            return;
        }
        let pending = Pending::start(&self.retellings);
        let switch = self.clone();
        tokio::spawn(async move {
            let dropped = switch.dropped.clone();
            let dropped = dropped.notified();
            tokio::pin!(dropped);
            // A stop before `notified` was made does not wake it.
            if !pending.current() {
                return;
            }
            let summary = async {
                match switch.summarize.clone() {
                    Some(s) => s(reply.clone()).await,
                    None => None,
                }
            };
            let short = tokio::select! {
                short = summary => short,
                () = &mut dropped => return,
            };
            let text = short
                .map(|s| clean_for_speech(&s))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| fallback_brief(&reply));
            let wanted = switch.get() && switch.heard();
            let r = switch.retellings.lock();
            if wanted && r.generation == pending.generation {
                voice.speak(&text);
            }
            drop(r);
            // Let go only after the retelling is queued: the voice is busy by then.
            drop(pending);
        });
    }
}

/// Counts one retelling until it is said or dropped.
struct Pending {
    retellings: Arc<Mutex<Retellings>>,
    generation: u64,
}

impl Pending {
    fn start(retellings: &Arc<Mutex<Retellings>>) -> Self {
        let mut r = retellings.lock();
        r.pending += 1;
        Self {
            retellings: retellings.clone(),
            generation: r.generation,
        }
    }

    /// Whether no stop came since it was asked for.
    fn current(&self) -> bool {
        self.retellings.lock().generation == self.generation
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        let mut r = self.retellings.lock();
        // A stop has already let go of the retellings of older generations.
        if r.generation == self.generation {
            r.pending = r.pending.saturating_sub(1);
        }
    }
}

/// The voice made by [`SpeakerSwitch::wrap`].
struct Retold {
    inner: Arc<dyn Voice>,
    switch: SpeakerSwitch,
}

impl Voice for Retold {
    fn speak(&self, text: &str) {
        self.inner.speak(text);
    }

    fn stop(&self) {
        // The retelling first: once dropped, it cannot reach the voice after the stop.
        self.switch.cancel();
        self.inner.stop();
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        self.inner.voices()
    }

    fn configure(&self, voice: &str, rate: f32) {
        self.inner.configure(voice, rate);
    }

    fn set_remote(&self, remote: Option<RemoteSynth>) {
        self.inner.set_remote(remote);
    }

    fn set_observer(&self, observer: Option<FirstSound>) {
        self.inner.set_observer(observer);
    }

    fn busy(&self) -> bool {
        self.switch.retelling() || self.inner.busy()
    }

    fn set_tone(&self, tone: Option<ToneSource>) {
        self.inner.set_tone(tone);
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
    /// How the task of this turn ended, `Some(ok)`, once it has.
    task_end: Option<bool>,
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
    // A question about an action is open: what the user says answers it, and
    // the reply goes on with its task.
    let mut confirming = false;
    let say = |text: &str| {
        let clean = clean_for_speech(text);
        if !clean.is_empty() && switch.heard() {
            voice.speak(&clean);
        }
    };
    // A whole reply goes to the voice sentence by sentence: in one piece it
    // would start late and be cut off.
    let say_all = |text: &str| {
        let mut s = SentenceSplitter::default();
        for sentence in s.push(text).into_iter().chain(s.finish()) {
            say(&sentence);
        }
    };
    // Silence now, a retelling still being made included.
    let stop = || {
        switch.cancel();
        voice.stop();
    };
    loop {
        let event = match events.recv().await {
            Ok(e) => e,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        };
        match &event {
            Event::ConfirmRequested { .. } => confirming = true,
            Event::ConfirmResolved { .. }
            | Event::StateChanged {
                to: AssistantState::Idle,
                ..
            } => confirming = false,
            _ => {}
        }
        let brief = switch.brief();
        match event {
            // An answer to the question: her voice stops, the turn goes on.
            Event::UserText { .. } if confirming => stop(),
            // A new message interrupts whatever she was saying.
            Event::UserText { .. } => {
                stop();
                turn = Turn::default();
                streamed = false;
            }
            // Not the error of the turn: the agent ends the task before it
            // reports one. The microphone or the selection failed meanwhile.
            Event::Error { .. } if turn.task && turn.task_end.is_none() => {}
            Event::Error { message } => {
                stop();
                let broke_off = turn.task_end == Some(false);
                turn = Turn::default();
                streamed = false;
                if broke_off && switch.get() {
                    say(&format!("Задача оборвалась. {message}"));
                }
            }
            // A task stopped by the user ends without a reply or an error:
            // a later error is not about it.
            Event::StateChanged {
                to: AssistantState::Idle,
                ..
            } if turn.task_end.is_some() => {
                turn = Turn::default();
                streamed = false;
            }
            Event::TaskUpdated { task } => {
                if !task.finished && !turn.task {
                    // What was waiting to be said belongs to the steps now.
                    turn.held.clear();
                    turn.splitter = SentenceSplitter::default();
                }
                // Stays set to the end of the turn: the final reply comes whole.
                turn.task = true;
                turn.task_end = task.finished.then_some(task.ok);
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
            // How to answer the question: the turn goes on.
            Event::AssistantText { text } if confirming => say(&text),
            Event::AssistantText { text } => {
                let whole = !streamed || turn.task;
                if !brief {
                    if whole {
                        say_all(&text);
                    } else if let Some(rest) = turn.splitter.finish() {
                        say(&rest);
                    }
                } else if whole {
                    if text.chars().count() > LONG_REPLY {
                        switch.retell(voice.clone(), text);
                    } else {
                        say(&text);
                    }
                } else if turn.long {
                    switch.retell(voice.clone(), text);
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
    use mika_core::confirm::{Answer, DecidedBy};
    use mika_core::tools::{Assessment, RiskLevel};
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
        task_with(finished, finished)
    }

    fn task_with(finished: bool, ok: bool) -> Event {
        Event::TaskUpdated {
            task: mika_core::task::Task {
                title: "Задача".into(),
                steps: Vec::new(),
                finished,
                ok,
            },
        }
    }

    fn idle() -> Event {
        Event::StateChanged {
            from: AssistantState::Thinking,
            to: AssistantState::Idle,
        }
    }

    fn error(message: &str) -> Event {
        Event::Error {
            message: message.into(),
        }
    }

    fn confirm_requested() -> Event {
        Event::ConfirmRequested {
            id: 7,
            tool: "messenger_send".into(),
            assessment: Assessment::new(RiskLevel::High, "Написать Ане", "x", "y"),
            allow_always: false,
            timeout_s: 60,
        }
    }

    fn confirm_resolved() -> Event {
        Event::ConfirmResolved {
            id: 7,
            answer: Answer::Once,
            by: DecidedBy::Chat,
        }
    }

    /// Long-reply retelling that waits until the test lets it go.
    fn gated_switch() -> (SpeakerSwitch, tokio::sync::oneshot::Sender<()>) {
        let (go, wait) = tokio::sync::oneshot::channel::<()>();
        let wait = Arc::new(Mutex::new(Some(wait)));
        let s = SpeakerSwitch::new(true).with_summarizer(Arc::new(move |_reply: String| {
            let wait = wait.lock().take();
            Box::pin(async move {
                if let Some(w) = wait {
                    let _ = w.await;
                }
                Some("Коротко о главном.".into())
            })
        }));
        s.set_brief(true);
        (s, go)
    }

    /// What happened to a long reply whose retelling the model makes slowly.
    struct SlowRetelling {
        voice: Arc<FakeVoice>,
        /// She counted as speaking while the model was retelling.
        busy_meanwhile: bool,
        /// The retelling still waited for the model when it answered.
        model_waited: bool,
    }

    async fn long_reply_with_a_slow_retelling(stop_meanwhile: bool) -> SlowRetelling {
        let bus = EventBus::new(64);
        let inner = Arc::new(FakeVoice::default());
        let (switch, go) = gated_switch();
        let voice = switch.wrap(inner.clone());
        let job = tokio::spawn(run_speaker(bus.clone(), voice.clone(), switch));
        tokio::time::sleep(Duration::from_millis(10)).await;
        bus.publish(Event::AssistantText {
            text: "Это очень длинный ответ про погоду на неделю. ".repeat(10),
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        // The model is still retelling: she has not finished speaking.
        let busy_meanwhile = voice.busy();
        assert!(inner.said.lock().is_empty());
        if stop_meanwhile {
            // «Стоп», the stop word or the microphone opening.
            voice.stop();
            // Silent at once, not when the model answers.
            assert!(!voice.busy());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let model_waited = go.send(()).is_ok();
        tokio::time::sleep(Duration::from_millis(30)).await;
        job.abort();
        assert!(!voice.busy());
        SlowRetelling {
            voice: inner,
            busy_meanwhile,
            model_waited,
        }
    }

    #[tokio::test]
    async fn a_retelling_being_made_counts_as_speech() {
        let r = long_reply_with_a_slow_retelling(false).await;
        assert!(r.busy_meanwhile);
        assert!(r.model_waited);
        assert_eq!(*r.voice.said.lock(), ["Коротко о главном."]);
    }

    #[tokio::test]
    async fn a_stop_drops_a_retelling_being_made() {
        let r = long_reply_with_a_slow_retelling(true).await;
        assert!(r.busy_meanwhile);
        // The model is not waited for after the stop.
        assert!(!r.model_waited);
        assert!(r.voice.said.lock().is_empty());
        assert_eq!(*r.voice.stops.lock(), 1);
    }

    #[tokio::test]
    async fn a_stop_between_two_retellings_lets_go_of_the_old_one_only() {
        let switch = gated_switch().0;
        let inner = Arc::new(FakeVoice::default());
        let voice = switch.wrap(inner.clone());
        switch.retell(inner.clone(), "Первый длинный ответ.".into());
        assert!(voice.busy());
        voice.stop();
        assert!(!voice.busy());
        // The next reply is retold and counts as speech until it is said.
        switch.retell(inner.clone(), "Второй длинный ответ.".into());
        assert!(voice.busy());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!voice.busy());
        assert_eq!(*inner.said.lock(), ["Коротко о главном."]);
    }

    #[tokio::test]
    async fn an_answer_to_a_question_keeps_the_task_silent() {
        // A step after the answer is not said.
        let v = run_with(
            vec![
                task(false),
                confirm_requested(),
                Event::UserText {
                    text: "эээ".into()
                },
                Event::AssistantText {
                    text: "Скажи «да» или «нет».".into(),
                },
                Event::UserText {
                    text: "да".into()
                },
                confirm_resolved(),
                delta("Отправила. Теперь открою браузер. "),
                Event::ToolStarted {
                    id: "2".into(),
                    name: "open_app".into(),
                    summary: "Открыть".into(),
                },
                task(true),
                Event::AssistantText {
                    text: "Всё сделала.".into(),
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            [
                "Нужно подтверждение. Написать Ане.",
                "Скажи «да» или «нет».",
                "Всё сделала."
            ]
        );

        // Without retelling the end right after the answer is said once.
        let end = "Сообщение Ане отправлено.";
        let v = run_with(
            vec![
                task(false),
                confirm_requested(),
                Event::UserText {
                    text: "да".into()
                },
                confirm_resolved(),
                delta(&format!("{end} ")),
                task(true),
                Event::AssistantText { text: end.into() },
            ],
            SpeakerSwitch::new(true),
        )
        .await;
        assert_eq!(*v.said.lock(), ["Нужно подтверждение. Написать Ане.", end]);
    }

    #[tokio::test]
    async fn an_error_from_elsewhere_is_not_a_broken_task() {
        // The microphone fails while the task runs: the task goes on silently.
        let v = run_with(
            vec![
                task(false),
                error("не услышала речь"),
                delta("Шаг два готов. "),
                Event::ToolStarted {
                    id: "1".into(),
                    name: "open_app".into(),
                    summary: "Открыть".into(),
                },
                task(true),
                Event::AssistantText {
                    text: "Всё сделала.".into(),
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(*v.said.lock(), ["Всё сделала."]);

        // A task stopped by the user; later the microphone hears nothing.
        let v = run_with(
            vec![
                task(false),
                task_with(true, false),
                idle(),
                error("не услышала речь"),
            ],
            brief_switch(),
        )
        .await;
        assert!(v.said.lock().is_empty());
    }

    #[tokio::test]
    async fn without_retelling_the_end_of_a_task_is_said_by_sentences() {
        let end = "Нашла три файла с отчётами. Самый новый — за октябрь. Остальные старше года.";
        let v = run_with(
            vec![
                task(false),
                task(true),
                Event::AssistantText { text: end.into() },
            ],
            SpeakerSwitch::new(true),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            [
                "Нашла три файла с отчётами.",
                "Самый новый — за октябрь.",
                "Остальные старше года."
            ]
        );
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

        // A task that broke off says why. The agent ends the task first.
        let v = run_with(
            vec![
                task(false),
                delta("Шаг один. "),
                task_with(true, false),
                error("слишком много шагов подряд"),
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
