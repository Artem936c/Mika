//! Windows voice: `SpeechSynthesizer` makes the sound, `MediaPlayer` plays it.
//! Both are built into Windows, so speech works offline and adds no downloads.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use windows::core::HSTRING;
use windows::Foundation::TypedEventHandler;
use windows::Media::Core::MediaSource;
use windows::Media::Playback::MediaPlayer;
use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceInformation};
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::speech::{FirstSound, RemoteSynth, Voice, VoiceInfo};

/// One sentence never plays longer than this, whatever the player reports.
const MAX_SENTENCE_PLAY: Duration = Duration::from_secs(90);
/// The remote synthesizer gets this long for one sentence.
const REMOTE_WAIT: Duration = Duration::from_secs(45);

/// Sound of a sentence that a remote synthesizer is making or has made.
type RemoteAudio = mpsc::Receiver<Result<Vec<u8>, String>>;

/// Sentences synthesized ahead of the one playing. Each request to a paid
/// provider costs money, so a long reply is not ordered whole in advance:
/// after a stop only these few are lost.
const LOOKAHEAD: usize = 2;

/// A remote synthesis that waits for its turn.
struct Job {
    generation: u64,
    text: String,
    tx: mpsc::Sender<Result<Vec<u8>, String>>,
    synth: RemoteSynth,
}

/// Remote requests asked for but not yet played, and those waiting to be asked.
#[derive(Default)]
struct Prefetch {
    in_flight: AtomicUsize,
    waiting: Mutex<VecDeque<Job>>,
}

impl Prefetch {
    fn start(&self, job: Job) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let _ = job.tx.send((job.synth)(&job.text));
        });
    }

    /// Asks for the sound now, or later when fewer than `LOOKAHEAD` are ahead.
    fn submit(&self, job: Job) {
        if self.in_flight.load(Ordering::Relaxed) < LOOKAHEAD {
            self.start(job);
        } else {
            self.waiting.lock().push_back(job);
        }
    }

    /// One remote sentence was played or dropped: the next waiting one may be asked
    /// for, unless a stop made it stale.
    fn done(&self, current: u64) {
        let _ = self
            .in_flight
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
        let next = {
            let mut waiting = self.waiting.lock();
            waiting.retain(|j| j.generation == current);
            waiting.pop_front()
        };
        if let Some(job) = next {
            self.start(job);
        }
    }

    /// A stop: nothing that waits is asked for.
    fn clear(&self) {
        self.waiting.lock().clear();
    }
}

/// One sentence to speak.
struct Sentence {
    text: String,
    /// Sentences of a generation older than the current one are dropped.
    generation: u64,
    /// Its sound, when a remote synthesizer makes it.
    remote: Option<RemoteAudio>,
    asked: std::time::Instant,
    /// The first sentence after a stop: its time to the first sound is reported.
    first: bool,
}

enum Cmd {
    Speak(Sentence),
    Configure(String, f32),
}

type Observer = Arc<Mutex<Option<FirstSound>>>;

pub struct WindowsVoice {
    tx: Mutex<mpsc::Sender<Cmd>>,
    /// Grows on every `stop`; sentences of an older generation are dropped.
    generation: Arc<AtomicU64>,
    remote: Mutex<Option<RemoteSynth>>,
    prefetch: Arc<Prefetch>,
    observer: Observer,
    /// True until the first sentence after a stop is queued.
    fresh: AtomicBool,
}

/// Content type of a sound file by its first bytes.
pub fn audio_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"RIFF") {
        "audio/wav"
    } else if bytes.starts_with(b"OggS") {
        "audio/ogg"
    } else if bytes.starts_with(b"fLaC") {
        "audio/flac"
    } else {
        "audio/mpeg"
    }
}

impl WindowsVoice {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let seen = generation.clone();
        let observer: Observer = Arc::new(Mutex::new(None));
        let watching = observer.clone();
        let prefetch = Arc::new(Prefetch::default());
        let ahead = prefetch.clone();
        std::thread::Builder::new()
            .name("voice".into())
            .spawn(move || worker(rx, seen, watching, ahead))
            .expect("spawn voice thread");
        Self {
            tx: Mutex::new(tx),
            generation,
            remote: Mutex::new(None),
            prefetch,
            observer,
            fresh: AtomicBool::new(true),
        }
    }
}

impl Default for WindowsVoice {
    fn default() -> Self {
        Self::new()
    }
}

fn all_voices() -> windows::core::Result<Vec<VoiceInformation>> {
    Ok(SpeechSynthesizer::AllVoices()?.into_iter().collect())
}

fn info(v: &VoiceInformation) -> Option<VoiceInfo> {
    Some(VoiceInfo {
        name: v.DisplayName().ok()?.to_string(),
        language: v.Language().ok()?.to_string(),
    })
}

/// The voice with this name, or the first Russian one for an empty name.
pub fn pick<'a>(voices: &'a [VoiceInfo], wanted: &str) -> Option<&'a VoiceInfo> {
    let wanted = wanted.trim();
    if !wanted.is_empty() {
        if let Some(v) = voices.iter().find(|v| v.name == wanted) {
            return Some(v);
        }
    }
    voices
        .iter()
        .find(|v| v.language.to_lowercase().starts_with("ru"))
}

struct Engine {
    synth: SpeechSynthesizer,
    player: MediaPlayer,
    ended: mpsc::Receiver<()>,
}

impl Engine {
    fn new() -> windows::core::Result<Self> {
        let synth = SpeechSynthesizer::new()?;
        let player = MediaPlayer::new()?;
        // Speech is not "media": keep it out of the system media controls, so the
        // pause key and "what is playing" still mean the user's player.
        player.CommandManager()?.SetIsEnabled(false)?;
        let (tx, ended) = mpsc::channel();
        let done = tx.clone();
        player.MediaEnded(&TypedEventHandler::new(move |_, _| {
            let _ = done.send(());
            Ok(())
        }))?;
        player.MediaFailed(&TypedEventHandler::new(move |_, _| {
            let _ = tx.send(());
            Ok(())
        }))?;
        Ok(Self {
            synth,
            player,
            ended,
        })
    }

    fn configure(&self, voice: &str, rate: f32) -> windows::core::Result<()> {
        let voices = all_voices()?;
        let infos: Vec<VoiceInfo> = voices.iter().filter_map(info).collect();
        if let Some(chosen) = pick(&infos, voice) {
            if let Some(v) = voices
                .iter()
                .find(|v| v.DisplayName().is_ok_and(|n| n == chosen.name.as_str()))
            {
                self.synth.SetVoice(v)?;
            }
        }
        self.synth
            .Options()?
            .SetSpeakingRate(rate.clamp(0.5, 2.0) as f64)
    }

    /// Speaks one sentence with the Windows voice. `ready` runs when the sound exists.
    fn play(
        &self,
        text: &str,
        stopped: impl Fn() -> bool,
        ready: impl FnOnce(),
    ) -> windows::core::Result<()> {
        let stream = self
            .synth
            .SynthesizeTextToStreamAsync(&HSTRING::from(text))?
            .join()?;
        if stopped() {
            return Ok(());
        }
        let source = MediaSource::CreateFromStream(&stream, &stream.ContentType()?)?;
        ready();
        self.play_source(&source, stopped)
    }

    /// Plays a ready sound file: WAV, MP3, Ogg or FLAC.
    fn play_audio(&self, bytes: &[u8], stopped: impl Fn() -> bool) -> windows::core::Result<()> {
        let stream = InMemoryRandomAccessStream::new()?;
        let writer = DataWriter::CreateDataWriter(&stream)?;
        writer.WriteBytes(bytes)?;
        writer.StoreAsync()?.join()?;
        writer.FlushAsync()?.join()?;
        writer.DetachStream()?;
        stream.Seek(0)?;
        let source = MediaSource::CreateFromStream(&stream, &HSTRING::from(audio_mime(bytes)))?;
        self.play_source(&source, stopped)
    }

    /// Plays to the end. Returns early when `stopped` says the generation changed.
    fn play_source(
        &self,
        source: &MediaSource,
        stopped: impl Fn() -> bool,
    ) -> windows::core::Result<()> {
        // Signals left over from a sentence that was cut short.
        while self.ended.try_recv().is_ok() {}
        self.player.SetSource(source)?;
        self.player.Play()?;
        let started = std::time::Instant::now();
        loop {
            if self.ended.recv_timeout(Duration::from_millis(40)).is_ok() {
                break;
            }
            if stopped() || started.elapsed() > MAX_SENTENCE_PLAY {
                self.player.Pause()?;
                break;
            }
        }
        Ok(())
    }
}

/// Waits for the sound of a sentence. `None` when the sentence was stopped meanwhile.
fn wait_remote(rx: &RemoteAudio, stopped: &impl Fn() -> bool) -> Option<Result<Vec<u8>, String>> {
    let started = std::time::Instant::now();
    loop {
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(result) => return Some(result),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Some(Err("синтез речи оборвался".into()))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stopped() {
                    return None;
                }
                if started.elapsed() > REMOTE_WAIT {
                    return Some(Err("синтез речи не ответил за 45 секунд".into()));
                }
            }
        }
    }
}

fn worker(
    rx: mpsc::Receiver<Cmd>,
    generation: Arc<AtomicU64>,
    observer: Observer,
    prefetch: Arc<Prefetch>,
) {
    // SAFETY: COM is initialised for this thread, which lives as long as the channel.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut engine: Option<Engine> = None;
    let mut settings = (String::new(), 1.0f32);
    while let Ok(cmd) = rx.recv() {
        // The engine starts with the first sentence: a silent assistant never touches audio.
        let cmd = match cmd {
            Cmd::Configure(voice, rate) if engine.is_none() => {
                settings = (voice, rate);
                continue;
            }
            other => other,
        };
        if engine.is_none() {
            match Engine::new() {
                Ok(e) => {
                    if let Err(err) = e.configure(&settings.0, settings.1) {
                        log::warn!("voice settings not applied: {}", err.message());
                    }
                    engine = Some(e);
                }
                Err(err) => {
                    log::warn!("voice is unavailable: {}", err.message());
                    continue;
                }
            }
        }
        let Some(e) = engine.as_ref() else { continue };
        match cmd {
            Cmd::Configure(voice, rate) => {
                if let Err(err) = e.configure(&voice, rate) {
                    log::warn!("voice settings not applied: {}", err.message());
                }
                settings = (voice, rate);
            }
            Cmd::Speak(s) => {
                let gen = s.generation;
                let stopped = || generation.load(Ordering::Relaxed) != gen;
                // Whatever happens to a remote sentence, its place ahead is freed.
                let _free = s.remote.is_some().then(|| FreeOnDrop {
                    prefetch: &prefetch,
                    generation: &generation,
                });
                if stopped() {
                    continue;
                }
                // Reports the time to the first sound, once per answer.
                let ready = || {
                    if s.first {
                        let ms = s.asked.elapsed().as_millis() as u64;
                        log::info!("first sound after {ms} ms");
                        if let Some(report) = observer.lock().clone() {
                            report(ms);
                        }
                    }
                };
                // `None`: no remote synthesizer for this sentence.
                let remote_result = s.remote.as_ref().map(|rx| match wait_remote(rx, &stopped) {
                    Some(Ok(bytes)) => {
                        ready();
                        e.play_audio(&bytes, stopped).map_err(|err| err.message())
                    }
                    Some(Err(why)) => Err(why),
                    // Stopped while waiting for the sound.
                    None => Ok(()),
                });
                if stopped() {
                    continue;
                }
                let result = match remote_result {
                    Some(Ok(())) => Ok(()),
                    Some(Err(why)) => {
                        log::warn!("remote speech failed, using the Windows voice: {why}");
                        e.play(&s.text, stopped, ready)
                    }
                    None => e.play(&s.text, stopped, ready),
                };
                if let Err(err) = result {
                    log::warn!("speech failed: {}", err.message());
                }
            }
        }
    }
}

/// Frees a sentence's place in the prefetch when the worker is done with it.
struct FreeOnDrop<'a> {
    prefetch: &'a Prefetch,
    generation: &'a AtomicU64,
}

impl Drop for FreeOnDrop<'_> {
    fn drop(&mut self) {
        self.prefetch.done(self.generation.load(Ordering::Relaxed));
    }
}

impl Voice for WindowsVoice {
    fn speak(&self, text: &str) {
        let gen = self.generation.load(Ordering::Relaxed);
        // The sound is asked for ahead of playback, but never more than
        // `LOOKAHEAD` sentences ahead: each request is paid.
        let remote = self.remote.lock().clone().map(|synth| {
            let (tx, rx) = mpsc::channel();
            self.prefetch.submit(Job {
                generation: gen,
                text: text.to_string(),
                tx,
                synth,
            });
            rx
        });
        let _ = self.tx.lock().send(Cmd::Speak(Sentence {
            text: text.to_string(),
            generation: gen,
            remote,
            asked: std::time::Instant::now(),
            first: self.fresh.swap(false, Ordering::Relaxed),
        }));
    }

    fn set_observer(&self, observer: Option<FirstSound>) {
        *self.observer.lock() = observer;
    }

    fn set_remote(&self, remote: Option<RemoteSynth>) {
        *self.remote.lock() = remote;
    }

    fn stop(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.prefetch.clear();
        self.fresh.store(true, Ordering::Relaxed);
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        // A short-lived thread: WinRT wants an initialised apartment, and the
        // caller may be an async worker that must not change its own.
        std::thread::spawn(|| {
            // SAFETY: COM is initialised for this new thread only.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            all_voices()
                .map(|v| v.iter().filter_map(info).collect())
                .unwrap_or_else(|e| {
                    log::warn!("cannot list voices: {}", e.message());
                    Vec::new()
                })
        })
        .join()
        .unwrap_or_default()
    }

    fn configure(&self, voice: &str, rate: f32) {
        let _ = self.tx.lock().send(Cmd::Configure(voice.to_string(), rate));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_reply_is_ordered_only_a_few_sentences_ahead() {
        let asked = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = asked.clone();
        let synth: RemoteSynth = Arc::new(move |t: &str| {
            log.lock().push(t.to_string());
            Ok(Vec::new())
        });
        let p = Prefetch::default();
        let mut rxs = Vec::new();
        for i in 0..6 {
            let (tx, rx) = mpsc::channel();
            p.submit(Job {
                generation: 0,
                text: format!("s{i}"),
                tx,
                synth: synth.clone(),
            });
            rxs.push(rx);
        }
        // Two are asked for, four wait.
        for rx in &rxs[..2] {
            rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        }
        assert_eq!(p.waiting.lock().len(), 4);
        // The first is played: the third may be asked for.
        p.done(0);
        rxs[2]
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        // A stop: what waits is never asked for.
        p.clear();
        p.done(1);
        p.done(1);
        std::thread::sleep(Duration::from_millis(100));
        let mut got = asked.lock().clone();
        got.sort();
        assert_eq!(got, ["s0", "s1", "s2"]);
        assert_eq!(p.in_flight.load(Ordering::Relaxed), 0);
    }

    fn v(name: &str, language: &str) -> VoiceInfo {
        VoiceInfo {
            name: name.into(),
            language: language.into(),
        }
    }

    #[test]
    fn named_voice_wins_then_russian_then_default() {
        let voices = vec![
            v("Microsoft David", "en-US"),
            v("Microsoft Irina", "ru-RU"),
            v("Microsoft Pavel", "ru-RU"),
        ];
        assert_eq!(
            pick(&voices, "Microsoft Pavel").unwrap().name,
            "Microsoft Pavel"
        );
        assert_eq!(pick(&voices, "").unwrap().name, "Microsoft Irina");
        assert_eq!(pick(&voices, "Нет такого").unwrap().name, "Microsoft Irina");
        // No Russian voice: the synthesizer keeps its default one.
        assert_eq!(pick(&voices[..1], ""), None);
    }
}
