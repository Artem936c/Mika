//! Wake phrase: a recognizer on this computer listens for a short list of
//! phrases; no sound leaves it.
//!
//! Vosk with a small Russian model is the main engine: it was trained on
//! thousands of voices, so it hears anyone without training. It recognizes
//! all speech with its whole vocabulary, and the name wakes her only as a
//! whole word: «микрофон» and «Михаил» stay themselves. When its files are not
//! downloaded, the speech recognizer built into Windows listens for a list of
//! phrases instead; it needs the Windows speech pack for the language.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mic::{resample, SystemMicrophone, TARGET_RATE};
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

/// The phrase that wakes her, if the recognizer heard the name. Partial text
/// counts: the whole vocabulary keeps it clean, and waiting for the pause
/// after the name would make her slow.
pub fn woke(heard: &Heard, name: &str) -> Option<String> {
    let text = match heard {
        Heard::Partial(t) | Heard::Final { text: t, .. } => t,
    };
    has_name(text, name).then(|| text.clone())
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
    /// The ways to say it, from `wake_phrases`, for the Windows recognizer.
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
    log::info!("vosk model loaded in {:?}", started.elapsed());
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
            log::warn!("vosk is unavailable: {e}");
            (setup.on_status)(Err(e));
            return;
        }
    };
    let mut last_error = String::new();
    while !stop.load(Ordering::Relaxed) {
        match vosk_session(&model, &name, setup, stop) {
            Ok(()) => break,
            Err(e) => {
                if e != last_error {
                    log::warn!("wake phrase is unavailable: {e}");
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

/// Sound given to Vosk at a time.
const BLOCK_MS: u32 = 100;
/// After the name, the rest of the word is not heard again.
const COOLDOWN: Duration = Duration::from_millis(1500);

/// Feeds sound at `rate` to the recognizer in 100 ms blocks at 16 kHz and
/// calls `on_wake` when the name is heard. Leftover sound stays in `buffer`.
pub struct Listener {
    recognizer: Recognizer,
    name: String,
    rate: u32,
    buffer: Vec<f32>,
    /// Sound fed since the start, in blocks; the cooldown is counted in sound
    /// time, so a file in a test behaves as the microphone does.
    blocks: u64,
    quiet_until: u64,
}

impl Listener {
    pub fn new(model: &Arc<Model>, name: &str, rate: u32) -> Result<Self, String> {
        Ok(Self {
            recognizer: Recognizer::new(model.clone(), TARGET_RATE as f32)?,
            name: name.trim().to_lowercase(),
            rate,
            buffer: Vec::new(),
            blocks: 0,
            quiet_until: 0,
        })
    }

    /// Mono samples at the rate given to `new`. Returns the phrases heard.
    pub fn feed(&mut self, chunk: &[f32]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        let block = (self.rate * BLOCK_MS / 1000).max(1) as usize;
        let cooldown = COOLDOWN.as_millis() as u64 / BLOCK_MS as u64;
        let mut heard = Vec::new();
        while self.buffer.len() >= block {
            let samples: Vec<i16> = resample(&self.buffer[..block], self.rate, TARGET_RATE)
                .iter()
                .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .collect();
            self.buffer.drain(..block);
            self.blocks += 1;
            if self.blocks < self.quiet_until {
                continue;
            }
            if let Some(phrase) = woke(&self.recognizer.accept(&samples), &self.name) {
                self.recognizer.reset();
                self.quiet_until = self.blocks + cooldown;
                heard.push(phrase);
            }
        }
        heard
    }
}

fn vosk_session(
    model: &Arc<Model>,
    name: &str,
    setup: &WakeSetup,
    stop: &AtomicBool,
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let (stream, rate) = setup.mic.open(tx).map_err(|e| e.to_string())?;
    let mut listener = Listener::new(model, name, rate)?;
    log::info!("wake phrase (vosk) is listening for «{name}»");
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
    while !stop.load(Ordering::Relaxed) {
        match session(language, phrases, on_wake, stop) {
            Ok(Ended::Stopped) => break,
            Ok(Ended::Completed) => {
                last_error.clear();
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) => {
                if e != last_error {
                    log::warn!("wake phrase is unavailable: {e}");
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
    log::info!("wake phrase is listening for {phrases:?}");
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
                log::info!("wake phrase session ended with code {}", status.0);
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
}
