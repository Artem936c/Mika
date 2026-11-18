//! Wake phrase: a recognizer on this computer listens for the name and the
//! stop words; no sound leaves it.
//!
//! The user picks the engine (decision 0048). sherpa-onnx with a small
//! streaming Russian model hears the name and the stop words as hot words.
//! Vosk with a small Russian model recognizes all speech with its whole
//! vocabulary. Both hear anyone without training, and the name wakes her only
//! as a whole word: «микрофон» and «Михаил» stay themselves. When neither is
//! downloaded, the speech recognizer built into Windows listens for a list of
//! phrases instead; it needs the Windows speech pack for the language.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mic::{resample, SystemMicrophone, TARGET_RATE};
use crate::sherpa::SherpaLib;
use crate::vosk::{Heard, Model, Recognizer, VoskLib};

/// Called with the phrase that was heard.
pub type OnWake = Arc<dyn Fn(String) + Send + Sync>;
/// Called with `Ok` when listening starts and with the reason when it cannot.
pub type OnStatus = Arc<dyn Fn(Result<(), String>) + Send + Sync>;

/// Phrases that wake her for the given name: «Мика», «эй, Мика», «Мика, слушай»…
pub fn wake_phrases(name: &str) -> Vec<String> {
    let n = name.trim().to_lowercase();
    if n.is_empty() {
        return Vec::new();
    }
    vec![
        n.clone(),
        format!("эй {n}"),
        format!("привет {n}"),
        format!("{n} слушай"),
        format!("слушай {n}"),
    ]
}

/// True when the reply she is speaking contains her own name: her voice in the
/// speakers must not wake her.
pub fn is_echo(name: &str, speaking: &str) -> bool {
    let n = name.trim().to_lowercase();
    !n.is_empty() && speaking.to_lowercase().contains(&n)
}

/// Who listens for the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeEngine {
    /// sherpa-onnx from the downloaded folders (decision 0048).
    Sherpa { library: PathBuf, model: PathBuf },
    /// Vosk from the downloaded folders.
    Vosk { library: PathBuf, model: PathBuf },
    /// The speech recognizer of Windows, for this language (`ru-RU`).
    Windows { language: String },
}

/// Words of the name in what the recognizer heard, as whole words:
/// «эй мика» has «мика», «микаэла» and «мик» do not.
pub fn has_name(heard: &str, name: &str) -> bool {
    let name: Vec<String> = name.split_whitespace().map(str::to_lowercase).collect();
    let heard: Vec<String> = heard.split_whitespace().map(str::to_lowercase).collect();
    !name.is_empty() && heard.windows(name.len()).any(|w| w == name.as_slice())
}

/// Words that stop her, one list for the whole app (decisions 0045, 0048).
pub use mika_core::pause::{only_stop_words, stop_word, STOP_WORDS};

/// Stop phrases of more than one word the Windows recognizer listens for.
const STOP_PHRASES: [&str; 2] = ["перестань слушать", "перестань говорить"];

/// The phrases the Windows recognizer listens for: the ways to say the name
/// and the stop words (decision 0045). Empty without a name.
pub fn grammar_phrases(name: &str) -> Vec<String> {
    let mut phrases = wake_phrases(name);
    if phrases.is_empty() {
        return phrases;
    }
    for word in STOP_WORDS.iter().chain(STOP_PHRASES.iter()) {
        if !phrases.iter().any(|p| p == word) {
            phrases.push(word.to_string());
        }
    }
    phrases
}

/// The hot words of sherpa-onnx: the name and the stop phrases. The decoder
/// leans towards them, so they are heard in noise (decision 0048).
pub fn hotwords(name: &str) -> Vec<String> {
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return Vec::new();
    }
    let mut words = vec![name];
    words.extend(STOP_WORDS.iter().map(|w| w.to_string()));
    words.extend(STOP_PHRASES.iter().map(|w| w.to_string()));
    words
}

/// What a heard phrase asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeardKind {
    /// The name: listen to the user.
    Name,
    /// A stop word without the name: stop everything.
    Stop,
}

/// The name wins over a stop word: «Мика, стоп» is a call.
pub fn heard_kind(heard: &str, name: &str) -> Option<HeardKind> {
    if has_name(heard, name) {
        Some(HeardKind::Name)
    } else if stop_word(heard).is_some() {
        Some(HeardKind::Stop)
    } else {
        None
    }
}

/// The phrase that wakes or stops her, if the recognizer heard the name or a
/// stop word. Partial text counts: the whole vocabulary keeps it clean, and
/// waiting for the pause after the name would make her slow.
pub fn woke(heard: &Heard, name: &str) -> Option<String> {
    let text = match heard {
        Heard::Partial(t) | Heard::Final { text: t, .. } => t,
    };
    heard_kind(text, name).map(|_| text.clone())
}

/// Sound blocks a phrase of stop words alone waits before it counts: the
/// start of «стопка» or «останови машину» is heard as a stop, and sherpa
/// keeps such a piece for up to 0.6 s (decision 0048).
pub const STOP_GATE_BLOCKS: u64 = 8;

/// Decides, block by block, when the words heard so far wake or stop her.
/// The name counts at once. A phrase that may still be a stop and nothing
/// else waits for its end or for `STOP_GATE_BLOCKS`; a stop word inside a
/// longer phrase counts at once, and the app decides what it means.
#[derive(Debug, Default)]
pub struct Spotter {
    name: String,
    /// The block the waiting stop phrase was first heard at, and its words.
    pending: Option<(u64, String)>,
}

impl Spotter {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.trim().to_lowercase(),
            pending: None,
        }
    }

    /// `text`: the words of the phrase so far at sound block `block`;
    /// `ended`: a pause has just ended the phrase. Returns the phrase that
    /// counts; the caller then forgets the phrase and waits a cooldown.
    pub fn hear(&mut self, block: u64, text: &str, ended: bool) -> Option<String> {
        match heard_kind(text, &self.name) {
            Some(HeardKind::Name) => {
                self.pending = None;
                Some(text.to_string())
            }
            Some(HeardKind::Stop) if !ended && mika_core::pause::may_be_stop_phrase(text) => {
                let since = self.pending.get_or_insert((block, String::new())).0;
                if block.saturating_sub(since) >= STOP_GATE_BLOCKS {
                    self.pending = None;
                    Some(text.to_string())
                } else {
                    self.pending = Some((since, text.to_string()));
                    None
                }
            }
            Some(HeardKind::Stop) => {
                self.pending = None;
                Some(text.to_string())
            }
            None => match self.pending.take() {
                // The phrase ended or was dropped: what waited was whole.
                Some((_, last)) if text.trim().is_empty() || ended => Some(last),
                // Other words came: «стоп…» was «стопка».
                _ => None,
            },
        }
    }
}

/// Stops listening when dropped.
pub struct WakeHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WakeHandle {
    /// Stops listening and waits for the thread to end: its files are free after that.
    pub fn stop_and_wait(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for WakeHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Who hears the name and what to call when it is heard.
pub struct WakeSetup {
    pub engine: WakeEngine,
    /// What wakes her: «мика».
    pub name: String,
    /// What the Windows recognizer listens for, from `grammar_phrases`.
    pub phrases: Vec<String>,
    pub mic: Arc<SystemMicrophone>,
    pub on_wake: OnWake,
    pub on_status: OnStatus,
}

/// Starts listening on a thread of its own and restarts after any interruption.
pub fn start_wake(setup: WakeSetup) -> WakeHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("wake-phrase".into())
        .spawn(move || match &setup.engine {
            WakeEngine::Windows { language } => listen_loop(
                language,
                &setup.phrases,
                &setup.on_wake,
                &setup.on_status,
                &flag,
            ),
            WakeEngine::Vosk { library, model } => vosk_loop(library, model, &setup, &flag),
            WakeEngine::Sherpa { library, model } => sherpa_loop(library, model, &setup, &flag),
        })
        .expect("spawn wake phrase thread");
    WakeHandle {
        stop,
        thread: Some(thread),
    }
}

/// Waits `secs`, or less when told to stop. True when told to stop.
fn pause(stop: &AtomicBool, secs: u64) -> bool {
    for _ in 0..secs * 2 {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    stop.load(Ordering::Relaxed)
}

/// Loads Vosk and checks it knows the name.
pub fn load_vosk(library: &Path, model_dir: &Path, name: &str) -> Result<Arc<Model>, String> {
    let started = Instant::now();
    let model = VoskLib::load(library).and_then(|lib| Model::load(lib, model_dir))?;
    log::info!("vosk model loaded: ms={}", started.elapsed().as_millis());
    // Words outside the vocabulary are dropped from the grammar silently.
    if let Some(unknown) = name.split_whitespace().find(|w| !model.knows(w)) {
        return Err(format!(
            "модель Vosk не знает слова «{unknown}»: выбери другое имя или удали модель, тогда имя будет слушать Windows"
        ));
    }
    Ok(model)
}

/// Loads Vosk once, then listens and reopens the microphone after any failure.
fn vosk_loop(library: &Path, model_dir: &Path, setup: &WakeSetup, stop: &AtomicBool) {
    let name = setup.name.trim().to_lowercase();
    let model = match load_vosk(library, model_dir, &name) {
        Ok(m) => m,
        Err(e) => {
            log::error!("wake phrase does not work: engine=vosk error={e}");
            (setup.on_status)(Err(e));
            return;
        }
    };
    listen_with("vosk", setup, stop, |rate| {
        Listener::new(&model, &name, rate)
    });
}

/// Loads sherpa-onnx once, then listens and reopens the microphone after
/// any failure (decision 0048).
fn sherpa_loop(library: &Path, model_dir: &Path, setup: &WakeSetup, stop: &AtomicBool) {
    let started = Instant::now();
    let lib = match SherpaLib::load(library) {
        Ok(lib) => lib,
        Err(e) => {
            log::error!("wake phrase does not work: engine=sherpa error={e}");
            (setup.on_status)(Err(e));
            return;
        }
    };
    log::info!(
        "sherpa library loaded: ms={}",
        started.elapsed().as_millis()
    );
    let name = setup.name.trim().to_lowercase();
    listen_with("sherpa", setup, stop, |rate| {
        Listener::sherpa(lib.clone(), model_dir, &name, rate)
    });
}

/// Opens the microphone, feeds a listener made by `make` for its rate and
/// starts again every 30 s after a failure, until told to stop.
fn listen_with(
    engine: &str,
    setup: &WakeSetup,
    stop: &AtomicBool,
    make: impl Fn(u32) -> Result<Listener, String>,
) {
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        match session_with(engine, setup, stop, &make) {
            Ok(()) => {
                log::info!("wake phrase stopped listening: engine={engine}");
                break;
            }
            Err(e) => {
                if e != last_error {
                    log::warn!(
                        "wake phrase is unavailable, trying again every 30 s: engine={engine} error={e}"
                    );
                    (setup.on_status)(Err(e.clone()));
                    last_error = e;
                }
                if pause(stop, 30) {
                    break;
                }
            }
        }
    }
}

/// Sound given to the recognizer at a time.
const BLOCK_MS: u32 = 100;
/// After the name, the rest of the word is not heard again.
const COOLDOWN: Duration = Duration::from_millis(1500);

/// A recognizer of free speech fed 16 kHz blocks.
enum Hearer {
    Vosk(Recognizer),
    Sherpa(crate::sherpa::Recognizer),
}

impl Hearer {
    /// The words of the phrase so far and whether a pause has just ended it.
    fn accept(&mut self, samples: &[f32]) -> (String, bool) {
        match self {
            Hearer::Vosk(r) => {
                let shorts: Vec<i16> = samples
                    .iter()
                    .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                    .collect();
                match r.accept(&shorts) {
                    Heard::Partial(t) => (t, false),
                    Heard::Final { text, .. } => (text, true),
                }
            }
            Hearer::Sherpa(r) => r.accept(samples, TARGET_RATE),
        }
    }

    fn reset(&mut self) {
        match self {
            Hearer::Vosk(r) => r.reset(),
            Hearer::Sherpa(r) => r.reset(),
        }
    }
}

/// Feeds sound at `rate` to the recognizer in 100 ms blocks at 16 kHz and
/// returns the phrases with the name or a stop. Leftover sound stays in
/// `buffer`.
pub struct Listener {
    hearer: Hearer,
    spotter: Spotter,
    rate: u32,
    buffer: Vec<f32>,
    /// Sound fed since the start, in blocks; the cooldown is counted in sound
    /// time, so a file in a test behaves as the microphone does.
    blocks: u64,
    quiet_until: u64,
}

impl Listener {
    /// Vosk with its whole vocabulary.
    pub fn new(model: &Arc<Model>, name: &str, rate: u32) -> Result<Self, String> {
        let recognizer = Recognizer::new(model.clone(), TARGET_RATE as f32)?;
        Ok(Self::with(Hearer::Vosk(recognizer), name, rate))
    }

    /// sherpa-onnx with the name and the stop phrases as hot words.
    pub fn sherpa(
        lib: Arc<SherpaLib>,
        model_dir: &Path,
        name: &str,
        rate: u32,
    ) -> Result<Self, String> {
        let recognizer = crate::sherpa::Recognizer::new(lib, model_dir, &hotwords(name))?;
        Ok(Self::with(Hearer::Sherpa(recognizer), name, rate))
    }

    fn with(hearer: Hearer, name: &str, rate: u32) -> Self {
        Self {
            hearer,
            spotter: Spotter::new(name),
            rate,
            buffer: Vec::new(),
            blocks: 0,
            quiet_until: 0,
        }
    }

    /// Mono samples at the rate given to `new`. Returns the phrases heard.
    pub fn feed(&mut self, chunk: &[f32]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        let block = (self.rate * BLOCK_MS / 1000).max(1) as usize;
        let cooldown = COOLDOWN.as_millis() as u64 / BLOCK_MS as u64;
        let mut heard = Vec::new();
        while self.buffer.len() >= block {
            let samples = resample(&self.buffer[..block], self.rate, TARGET_RATE);
            self.buffer.drain(..block);
            self.blocks += 1;
            if self.blocks < self.quiet_until {
                continue;
            }
            let (text, ended) = self.hearer.accept(&samples);
            if let Some(phrase) = self.spotter.hear(self.blocks, &text, ended) {
                self.hearer.reset();
                self.quiet_until = self.blocks + cooldown;
                heard.push(phrase);
            } else if ended {
                self.hearer.reset();
            }
        }
        heard
    }
}

fn session_with(
    engine: &str,
    setup: &WakeSetup,
    stop: &AtomicBool,
    make: &impl Fn(u32) -> Result<Listener, String>,
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let (stream, rate) = setup.mic.open(tx).map_err(|e| e.to_string())?;
    let mut listener = make(rate)?;
    log::info!("wake phrase is listening: engine={engine} rate={rate}");
    (setup.on_status)(Ok(()));
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(chunk) => {
                for phrase in listener.feed(&chunk) {
                    (setup.on_wake)(phrase);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err("запись оборвалась".into()),
        }
    };
    drop(stream);
    result
}

fn listen_loop(
    language: &str,
    phrases: &[String],
    on_wake: &OnWake,
    on_status: &OnStatus,
    stop: &AtomicBool,
) {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    // SAFETY: COM is initialised for this thread, which ends with the loop.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut last_error = String::new();
    // The recognizer of Windows ends its session now and then and is started
    // again: only the first start is a milestone.
    let mut sessions: u64 = 0;
    while !stop.load(Ordering::Relaxed) {
        if sessions == 0 {
            log::info!(
                "wake phrase starts listening: engine=windows language={language} phrases={}",
                phrases.len()
            );
        }
        sessions += 1;
        match session(language, phrases, on_wake, stop) {
            Ok(Ended::Stopped) => {
                log::info!("wake phrase stopped listening: engine=windows sessions={sessions}");
                break;
            }
            Ok(Ended::Completed) => {
                last_error.clear();
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) => {
                if e != last_error {
                    log::warn!(
                        "wake phrase is unavailable, trying again every 30 s: engine=windows error={e}"
                    );
                    on_status(Err(e.clone()));
                    last_error = e;
                }
                // Try again later: a microphone may be plugged in, a language pack installed.
                if pause(stop, 30) {
                    return;
                }
            }
        }
    }
}

enum Ended {
    Stopped,
    Completed,
}

fn session(
    language: &str,
    phrases: &[String],
    on_wake: &OnWake,
    stop: &AtomicBool,
) -> Result<Ended, String> {
    use windows::core::HSTRING;
    use windows::Foundation::{TimeSpan, TypedEventHandler};
    use windows::Globalization::Language;
    use windows::Media::SpeechRecognition::{
        SpeechContinuousRecognitionCompletedEventArgs,
        SpeechContinuousRecognitionResultGeneratedEventArgs, SpeechContinuousRecognitionSession,
        SpeechRecognitionConfidence, SpeechRecognitionListConstraint,
        SpeechRecognitionResultStatus, SpeechRecognizer,
    };
    let err = |e: windows::core::Error| {
        let m = e.message();
        if m.is_empty() {
            format!("ошибка Windows {:#x}", e.code().0)
        } else {
            m
        }
    };
    let recognizer = Language::CreateLanguage(&HSTRING::from(language))
        .and_then(|lang| SpeechRecognizer::Create(&lang))
        .or_else(|_| SpeechRecognizer::new())
        .map_err(err)?;
    let list: windows_collections::IIterable<HSTRING> =
        phrases.iter().map(HSTRING::from).collect::<Vec<_>>().into();
    let constraint = SpeechRecognitionListConstraint::Create(&list).map_err(err)?;
    recognizer
        .Constraints()
        .and_then(|c| c.Append(&constraint))
        .map_err(err)?;
    let compiled = recognizer
        .CompileConstraintsAsync()
        .and_then(|op| op.join())
        .map_err(err)?;
    let status = compiled.Status().map_err(err)?;
    if status != SpeechRecognitionResultStatus::Success {
        return Err(match status {
            SpeechRecognitionResultStatus::TopicLanguageNotSupported
            | SpeechRecognitionResultStatus::GrammarLanguageMismatch => {
                "в Windows не установлено распознавание речи для этого языка: добавь его в «Параметры → Время и язык → Речь»".into()
            }
            SpeechRecognitionResultStatus::MicrophoneUnavailable => "микрофон недоступен".into(),
            other => format!("распознаватель Windows не готов: код {}", other.0),
        });
    }

    let session: SpeechContinuousRecognitionSession =
        recognizer.ContinuousRecognitionSession().map_err(err)?;
    // Listen for hours, not seconds: silence is the normal state here.
    let _ = session.SetAutoStopSilenceTimeout(TimeSpan {
        Duration: 24 * 3600 * 10_000_000,
    });
    let heard = on_wake.clone();
    session
        .ResultGenerated(&TypedEventHandler::<
            SpeechContinuousRecognitionSession,
            SpeechContinuousRecognitionResultGeneratedEventArgs,
        >::new(move |_, args| {
            let result = args.ok()?.Result()?;
            let confidence = result.Confidence()?;
            if confidence == SpeechRecognitionConfidence::High
                || confidence == SpeechRecognitionConfidence::Medium
            {
                let text = result.Text()?.to_string();
                if !text.trim().is_empty() {
                    heard(text);
                }
            }
            Ok(())
        }))
        .map_err(err)?;
    let (done_tx, done) = mpsc::channel();
    session
        .Completed(&TypedEventHandler::<
            SpeechContinuousRecognitionSession,
            SpeechContinuousRecognitionCompletedEventArgs,
        >::new(move |_, args| {
            let status = args.ok()?.Status()?;
            let _ = done_tx.send(status);
            Ok(())
        }))
        .map_err(err)?;
    session.StartAsync().and_then(|a| a.join()).map_err(err)?;
    log::debug!("wake phrase session started: engine=windows");
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = session.CancelAsync().and_then(|a| a.join());
            return Ok(Ended::Stopped);
        }
        match done.recv_timeout(Duration::from_millis(300)) {
            Ok(status) if status == SpeechRecognitionResultStatus::MicrophoneUnavailable => {
                return Err("микрофон недоступен".into());
            }
            Ok(status) if status == SpeechRecognitionResultStatus::Success => {
                return Ok(Ended::Completed);
            }
            Ok(status) => {
                log::debug!(
                    "wake phrase session ended, it starts again: engine=windows code={}",
                    status.0
                );
                return Ok(Ended::Completed);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(Ended::Completed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_follow_the_name() {
        let p = wake_phrases(" Мика ");
        assert_eq!(p[0], "мика");
        assert!(p.contains(&"эй мика".to_string()));
        assert!(p.contains(&"мика слушай".to_string()));
        assert!(wake_phrases("  ").is_empty());
    }

    #[test]
    fn the_name_counts_only_as_a_whole_word() {
        assert!(has_name("мика", "Мика"));
        assert!(has_name("эй мика", "мика"));
        assert!(has_name("[unk] мика слушай", "мика"));
        assert!(!has_name("микаэла", "мика"));
        assert!(!has_name("[unk]", "мика"));
        assert!(!has_name("", "мика"));
        assert!(has_name("эй мика ру", "мика ру"));
        assert!(!has_name("мика", "мика ру"));
        assert_eq!(
            woke(&Heard::Partial("эй мика".into()), "мика"),
            Some("эй мика".into())
        );
        assert_eq!(
            woke(
                &Heard::Final {
                    text: "[unk]".into(),
                    words: Vec::new()
                },
                "мика"
            ),
            None
        );
    }

    #[test]
    fn her_own_name_in_the_reply_is_an_echo() {
        assert!(is_echo("Мика", "Меня зовут МИКА, чем помочь?"));
        assert!(!is_echo("Мика", "Сегодня минус десять."));
        assert!(!is_echo("", "что угодно"));
    }

    #[test]
    fn the_windows_grammar_holds_the_name_phrases_and_the_stop_words() {
        let g = grammar_phrases("Мика");
        for p in wake_phrases("Мика") {
            assert!(g.contains(&p), "{p}");
        }
        for w in STOP_WORDS {
            assert!(g.contains(&w.to_string()), "{w}");
        }
        // Each phrase once.
        let mut unique = g.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), g.len());
        // Without a name nothing wakes her, and the grammar is empty.
        assert!(grammar_phrases("  ").is_empty());
    }

    #[test]
    fn a_phrase_of_stop_words_only() {
        assert!(only_stop_words("стоп"));
        assert!(only_stop_words("Хватит, хватит!"));
        assert!(only_stop_words("[unk] остановись"));
        assert!(!only_stop_words("сколько хватит бензина"));
        assert!(!only_stop_words("стопка"));
        assert!(!only_stop_words(""));
        assert!(!only_stop_words("[unk]"));
        // The Windows recognizer gives the word with a capital and a dot.
        assert_eq!(stop_word("Стоп."), Some("стоп"));
    }

    #[test]
    fn a_stop_word_stops_and_the_name_calls() {
        assert_eq!(heard_kind("мика стоп", "мика"), Some(HeardKind::Name));
        assert_eq!(heard_kind("ну хватит", "мика"), Some(HeardKind::Stop));
        assert_eq!(heard_kind("Замолчи", "мика"), Some(HeardKind::Stop));
        assert_eq!(heard_kind("стопка книг", "мика"), None);
        assert_eq!(heard_kind("включи музыку", "мика"), None);
        assert_eq!(stop_word("а теперь тихо"), Some("тихо"));
    }

    /// Feeds the spotter one text per block and returns what counted, with
    /// the block it counted at.
    fn spot(steps: &[(&str, bool)]) -> Vec<(u64, String)> {
        let mut s = Spotter::new("Мика");
        steps
            .iter()
            .enumerate()
            .filter_map(|(i, (text, ended))| s.hear(i as u64, text, *ended).map(|p| (i as u64, p)))
            .collect()
    }

    #[test]
    fn the_name_counts_at_once() {
        assert_eq!(
            spot(&[("", false), ("мика", false)]),
            vec![(1, "мика".into())]
        );
    }

    #[test]
    fn a_lone_stop_waits_for_the_end_of_the_phrase_or_the_gate() {
        // The phrase ends: the stop counts then.
        assert_eq!(
            spot(&[("стоп", false), ("стоп", false), ("стоп", true)]),
            vec![(2, "стоп".into())]
        );
        // The recognizer keeps the word: it counts after the gate.
        let held: Vec<(&str, bool)> = vec![("стой", false); 10];
        assert_eq!(spot(&held), vec![(STOP_GATE_BLOCKS, "стой".into())]);
        // A cut last word that may still finish the stop phrase waits too.
        assert_eq!(
            spot(&[("перестань слу", false), ("перестань слушать", true)]),
            vec![(1, "перестань слушать".into())]
        );
        // The phrase is dropped while it waits: what waited was whole.
        assert_eq!(
            spot(&[("хватит", false), ("", false)]),
            vec![(1, "хватит".into())]
        );
    }

    #[test]
    fn the_start_of_a_longer_word_is_not_a_stop() {
        assert!(spot(&[("стоп", false), ("стопка", false), ("стопка книг", true)]).is_empty());
    }

    #[test]
    fn a_stop_word_inside_a_phrase_counts_and_the_app_decides() {
        assert_eq!(
            spot(&[("останови", false), ("останови машину", false)]),
            vec![(1, "останови машину".into())]
        );
    }

    #[test]
    fn hot_words_are_the_name_and_the_stop_phrases() {
        let h = hotwords(" Мика ");
        assert_eq!(h[0], "мика");
        assert!(h.contains(&"стой".to_string()));
        assert!(h.contains(&"перестань слушать".to_string()));
        assert!(hotwords("").is_empty());
        assert!(grammar_phrases("Мика").contains(&"перестань слушать".to_string()));
    }
}
