//! Wake phrase: the speech recognizer built into Windows listens for a short list
//! of phrases. It runs on this computer; no sound leaves it. It works when Windows
//! has speech recognition for the language installed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

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

/// Stops listening when dropped.
pub struct WakeHandle {
    stop: Arc<AtomicBool>,
}

impl Drop for WakeHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts listening on a thread of its own and restarts after any interruption.
pub fn start_wake(
    language: &str,
    phrases: Vec<String>,
    on_wake: OnWake,
    on_status: OnStatus,
) -> WakeHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let language = language.to_string();
    std::thread::Builder::new()
        .name("wake-phrase".into())
        .spawn(move || listen_loop(&language, &phrases, &on_wake, &on_status, &flag))
        .expect("spawn wake phrase thread");
    WakeHandle { stop }
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
                for _ in 0..60 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
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
    fn her_own_name_in_the_reply_is_an_echo() {
        assert!(is_echo("Мика", "Меня зовут МИКА, чем помочь?"));
        assert!(!is_echo("Мика", "Сегодня минус десять."));
        assert!(!is_echo("", "что угодно"));
    }
}
