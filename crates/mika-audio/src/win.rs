//! Windows voice: `SpeechSynthesizer` makes the sound, `MediaPlayer` plays it.
//! Both are built into Windows, so speech works offline and adds no downloads.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use windows::core::{Interface, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Media::Core::MediaSource;
use windows::Media::Playback::MediaPlayer;
use windows::Media::SpeechSynthesis::{SpeechSynthesizer, VoiceInformation};
use windows::Storage::Streams::{
    DataReader, DataWriter, IRandomAccessStream, InMemoryRandomAccessStream,
};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFSample, MFAudioFormat_PCM, MFCreateMFByteStreamOnStreamEx, MFCreateMediaType,
    MFCreateSourceReaderFromByteStream, MFMediaType_Audio, MFShutdown, MFStartup,
    MF_BYTESTREAM_CONTENT_TYPE, MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::lead::{wav, with_lead_in, LEAD_IN_MS};
use crate::lips::{envelope, played_ms, LipCue, LipSink, FRAME_MS};

use crate::speech::{
    defuse, prosody_ssml, with_direction, DirectionSource, FirstSound, RemoteSynth, ToneSource,
    Voice, VoiceInfo,
};

/// One sentence never plays longer than this, whatever the player reports.
const MAX_SENTENCE_PLAY: Duration = Duration::from_secs(90);
/// How long the lips wait for the player to start before they go anyway.
const MAX_START_WAIT: Duration = Duration::from_millis(1500);
/// How often the player is asked whether its sound has started.
const START_POLL: Duration = Duration::from_millis(5);
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
    /// Pitch and pace in percent for the local voice.
    tone: (i8, i8),
}

enum Cmd {
    Speak(Sentence),
    Configure(String, f32),
}

type Observer = Arc<Mutex<Option<FirstSound>>>;
/// Who hears when a phrase starts to sound and when it falls quiet.
type Lips = Arc<Mutex<Option<LipSink>>>;

pub struct WindowsVoice {
    tx: Mutex<mpsc::Sender<Cmd>>,
    /// Grows on every `stop`; sentences of an older generation are dropped.
    generation: Arc<AtomicU64>,
    remote: Mutex<Option<RemoteSynth>>,
    prefetch: Arc<Prefetch>,
    observer: Observer,
    lips: Lips,
    /// True until the first sentence after a stop is queued.
    fresh: AtomicBool,
    /// Sentences queued and not yet said or dropped.
    pending: Arc<AtomicUsize>,
    tone: Mutex<Option<ToneSource>>,
    /// Her mood as a direction for the remote synthesizer (decision 0029).
    direction: Mutex<Option<DirectionSource>>,
}

/// Counts a sentence as done when the worker lets go of it.
struct Said<'a>(&'a AtomicUsize);

impl Drop for Said<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
    }
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
        let lips: Lips = Arc::new(Mutex::new(None));
        let mouth = lips.clone();
        let prefetch = Arc::new(Prefetch::default());
        let ahead = prefetch.clone();
        let pending = Arc::new(AtomicUsize::new(0));
        let queue = pending.clone();
        std::thread::Builder::new()
            .name("voice".into())
            .spawn(move || worker(rx, seen, watching, mouth, ahead, queue))
            .expect("spawn voice thread");
        Self {
            tx: Mutex::new(tx),
            generation,
            remote: Mutex::new(None),
            prefetch,
            observer,
            lips,
            fresh: AtomicBool::new(true),
            pending,
            tone: Mutex::new(None),
            direction: Mutex::new(None),
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

/// The voice with this name, or for an empty name the first one of the
/// language she replies in (`mika_core::i18n::voice_language`).
pub fn pick<'a>(voices: &'a [VoiceInfo], wanted: &str) -> Option<&'a VoiceInfo> {
    pick_for(voices, wanted, &mika_core::i18n::voice_language())
}

/// `language` is a prefix of the voice language tag: `ru`, `en`.
pub fn pick_for<'a>(
    voices: &'a [VoiceInfo],
    wanted: &str,
    language: &str,
) -> Option<&'a VoiceInfo> {
    let wanted = wanted.trim();
    if !wanted.is_empty() {
        if let Some(v) = voices.iter().find(|v| v.name == wanted) {
            return Some(v);
        }
    }
    voices
        .iter()
        .find(|v| v.language.to_lowercase().starts_with(language))
}

struct Engine {
    synth: SpeechSynthesizer,
    player: MediaPlayer,
    ended: mpsc::Receiver<()>,
    /// Language of the chosen voice, for SSML.
    language: String,
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
            language: String::new(),
        })
    }

    fn configure(&mut self, voice: &str, rate: f32) -> windows::core::Result<()> {
        let voices = all_voices()?;
        let infos: Vec<VoiceInfo> = voices.iter().filter_map(info).collect();
        if let Some(chosen) = pick(&infos, voice) {
            if let Some(v) = voices
                .iter()
                .find(|v| v.DisplayName().is_ok_and(|n| n == chosen.name.as_str()))
            {
                self.synth.SetVoice(v)?;
                self.language = chosen.language.clone();
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
        tone: (i8, i8),
        stopped: impl Fn() -> bool,
        ready: impl FnOnce(),
        lips: &Lips,
    ) -> windows::core::Result<()> {
        let plain = || {
            self.synth
                .SynthesizeTextToStreamAsync(&HSTRING::from(text))?
                .join()
        };
        let stream = if tone == (0, 0) {
            plain()?
        } else {
            let ssml = prosody_ssml(text, &self.language, tone);
            // A voice that does not take the markup still says the text.
            match self
                .synth
                .SynthesizeSsmlToStreamAsync(&HSTRING::from(ssml))
                .and_then(|op| op.join())
            {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("tone markup not taken: {}", e.message());
                    plain()?
                }
            }
        };
        if stopped() {
            return Ok(());
        }
        // The sound goes the way of a remote one, to get quiet before its
        // first word; one that cannot be read is played as it is.
        match read_all(&stream.cast()?) {
            Ok(bytes) => {
                ready();
                self.play_audio(&bytes, text, stopped, lips)
            }
            Err(e) => {
                log::warn!("voice sound not read: {}", e.message());
                let source = MediaSource::CreateFromStream(&stream, &stream.ContentType()?)?;
                ready();
                // Nothing to measure: the lips follow the letters alone.
                self.play_source(&source, stopped, Some((text, Vec::new())), lips)
            }
        }
    }

    /// Plays a ready sound file: WAV, MP3, Ogg or FLAC. It starts with
    /// quiet, see `lead`.
    fn play_audio(
        &self,
        bytes: &[u8],
        text: &str,
        stopped: impl Fn() -> bool,
        lips: &Lips,
    ) -> windows::core::Result<()> {
        let (bytes, levels) = prepare(bytes);
        let stream = memory_stream(&bytes)?;
        let source = MediaSource::CreateFromStream(&stream, &HSTRING::from(audio_mime(&bytes)))?;
        self.play_source(
            &source,
            stopped,
            Some((text, levels.unwrap_or_default())),
            lips,
        )
    }

    /// Plays to the end. Returns early when `stopped` says the generation changed.
    fn play_source(
        &self,
        source: &MediaSource,
        stopped: impl Fn() -> bool,
        phrase: Option<(&str, Vec<u8>)>,
        lips: &Lips,
    ) -> windows::core::Result<()> {
        // Signals left over from a sentence that was cut short.
        while self.ended.try_recv().is_ok() {}
        self.player.SetSource(source)?;
        let sink = lips.lock().clone();
        self.player.Play()?;
        let started = std::time::Instant::now();
        let mut ended = false;
        // Told once the sound has started, with how far it got, and quiet
        // whatever way it ends: the lips never run ahead of the voice.
        let _quiet = match (sink, phrase) {
            (Some(sink), Some((text, levels))) => {
                let played = loop {
                    if self.ended.recv_timeout(START_POLL).is_ok() {
                        ended = true;
                        break None;
                    }
                    if stopped() {
                        break None;
                    }
                    // A player that cannot tell its place: the lips start now.
                    let Ok(at) = self.player.PlaybackSession().and_then(|s| s.Position()) else {
                        break Some(0);
                    };
                    if at.Duration > 0 || started.elapsed() > MAX_START_WAIT {
                        break Some(played_ms(at.Duration));
                    }
                };
                played.map(|played_ms| {
                    sink(LipCue::Phrase {
                        text: text.to_string(),
                        frame_ms: FRAME_MS,
                        levels,
                        played_ms,
                    });
                    QuietOnDrop(sink)
                })
            }
            _ => None,
        };
        if ended {
            return Ok(());
        }
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

/// `bytes` in a stream of memory, read from its start.
fn memory_stream(bytes: &[u8]) -> windows::core::Result<InMemoryRandomAccessStream> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(bytes)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    Ok(stream)
}

/// The whole of a stream.
fn read_all(stream: &IRandomAccessStream) -> windows::core::Result<Vec<u8>> {
    let size = u32::try_from(stream.Size()?).unwrap_or(u32::MAX);
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    let got = reader.LoadAsync(size)?.join()?;
    let mut bytes = vec![0u8; got as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(bytes)
}

/// A sound that starts with at least `LEAD_IN_MS` of quiet, and its loudness
/// every `FRAME_MS` for the lips (decision 0040). A compressed sound is
/// decoded to WAV for both. A sound that has the quiet already plays as it
/// came; its levels come from the decoded copy, which sounds the same. A
/// sound that cannot be read plays as it is, with no levels.
fn prepare(bytes: &[u8]) -> (Cow<'_, [u8]>, Option<Vec<u8>>) {
    let decoded = if bytes.starts_with(b"RIFF") {
        None
    } else {
        decode(bytes)
            .map_err(|e| log::warn!("sound not decoded: {}", e.message()))
            .ok()
    };
    let readable = decoded.as_deref().unwrap_or(bytes);
    match with_lead_in(readable, LEAD_IN_MS) {
        Some(padded) => {
            let levels = envelope(&padded);
            (Cow::Owned(padded), levels)
        }
        None => (Cow::Borrowed(bytes), envelope(readable)),
    }
}

/// Tells the lips the phrase is over when the player lets go of it.
struct QuietOnDrop(LipSink);

impl Drop for QuietOnDrop {
    fn drop(&mut self) {
        (self.0)(LipCue::Quiet);
    }
}

/// Media Foundation, started for as long as this lives.
struct MediaFoundation;

impl MediaFoundation {
    fn start() -> windows::core::Result<Self> {
        // MF_VERSION and MFSTARTUP_LITE: no network sources are needed.
        // SAFETY: paired with `MFShutdown` in `drop`.
        unsafe { MFStartup(0x0002_0070, 1)? };
        Ok(Self)
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        // SAFETY: `MFStartup` succeeded in `start`.
        let _ = unsafe { MFShutdown() };
    }
}

/// The first audio stream of a source reader.
const FIRST_AUDIO_STREAM: u32 = 0xFFFF_FFFD;
/// The stream of a source reader has no more samples.
const END_OF_STREAM: u32 = 2;

/// A compressed sound, MP3, Ogg or FLAC, as a WAV file of 16-bit samples.
fn decode(bytes: &[u8]) -> windows::core::Result<Vec<u8>> {
    let _mf = MediaFoundation::start()?;
    let stream = memory_stream(bytes)?;
    // SAFETY: plain Media Foundation calls on objects owned here; the
    // locked buffer is read only while it is locked.
    unsafe {
        let byte_stream = MFCreateMFByteStreamOnStreamEx(&stream)?;
        if let Ok(attrs) = byte_stream.cast::<IMFAttributes>() {
            attrs.SetString(
                &MF_BYTESTREAM_CONTENT_TYPE,
                &HSTRING::from(audio_mime(bytes)),
            )?;
        }
        let reader = MFCreateSourceReaderFromByteStream(&byte_stream, None)?;
        let pcm = MFCreateMediaType()?;
        pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        reader.SetCurrentMediaType(FIRST_AUDIO_STREAM, None, &pcm)?;
        let format = reader.GetCurrentMediaType(FIRST_AUDIO_STREAM)?;
        let rate = format.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?;
        let channels = format.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?;
        let bits = format.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE)?;
        let mut samples = Vec::new();
        loop {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(
                FIRST_AUDIO_STREAM,
                0,
                None,
                Some(&mut flags),
                None,
                Some(&mut sample),
            )?;
            if let Some(sample) = sample {
                let buffer = sample.ConvertToContiguousBuffer()?;
                let mut data = std::ptr::null_mut();
                let mut len = 0u32;
                buffer.Lock(&mut data, None, Some(&mut len))?;
                samples.extend_from_slice(std::slice::from_raw_parts(data, len as usize));
                buffer.Unlock()?;
            }
            if flags & END_OF_STREAM != 0 {
                break;
            }
        }
        Ok(wav(&samples, rate, channels as u16, bits as u16))
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
    lips: Lips,
    prefetch: Arc<Prefetch>,
    pending: Arc<AtomicUsize>,
) {
    // SAFETY: COM is initialised for this thread, which lives as long as the channel.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut engine: Option<Engine> = None;
    let mut settings = (String::new(), 1.0f32);
    while let Ok(cmd) = rx.recv() {
        let _said = matches!(cmd, Cmd::Speak(_)).then(|| Said(&pending));
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
                Ok(mut e) => {
                    if let Err(err) = e.configure(&settings.0, settings.1) {
                        log::warn!("voice settings not applied: {}", err.message());
                    }
                    engine = Some(e);
                }
                Err(err) => {
                    log::error!(
                        "the voice of Windows did not start, nothing is said: {}",
                        err.message()
                    );
                    continue;
                }
            }
        }
        let Some(e) = engine.as_mut() else { continue };
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
                        log::info!(
                            "first sound of the answer: ms={ms} engine={}",
                            if s.remote.is_some() {
                                "provider"
                            } else {
                                "windows"
                            }
                        );
                        if let Some(report) = observer.lock().clone() {
                            report(ms);
                        }
                    }
                };
                // `None`: no remote synthesizer for this sentence.
                let remote_result = s.remote.as_ref().map(|rx| match wait_remote(rx, &stopped) {
                    Some(Ok(bytes)) => {
                        ready();
                        e.play_audio(&bytes, &s.text, stopped, &lips)
                            .map_err(|err| err.message())
                    }
                    Some(Err(why)) => Err(why),
                    // Stopped while waiting for the sound.
                    None => Ok(()),
                });
                if stopped() {
                    continue;
                }
                // What is said is the model's text: its size only.
                let chars = s.text.chars().count();
                let began = std::time::Instant::now();
                let result = match remote_result {
                    Some(Ok(())) => Ok("provider"),
                    Some(Err(why)) => {
                        log::warn!(
                            "the voice of the provider failed, the phrase is said by Windows: chars={chars} error={}",
                            mika_core::journal::error_text(&why)
                        );
                        e.play(&s.text, s.tone, stopped, ready, &lips)
                            .map(|()| "windows")
                    }
                    None => e
                        .play(&s.text, s.tone, stopped, ready, &lips)
                        .map(|()| "windows"),
                };
                match result {
                    Ok(engine) => log::debug!(
                        "phrase said: engine={engine} chars={chars} ms={}",
                        began.elapsed().as_millis()
                    ),
                    Err(err) => {
                        log::error!("phrase not said: chars={chars} error={}", err.message())
                    }
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

/// Text that is not from the model, as the remote synthesizer gets it:
/// notifications, reminders, errors and other text of others would otherwise
/// become directions for Fish S2 and Drama 3, which swallow them (decision
/// 0026). The Windows voice gets the text as it is.
fn remote_plain(text: &str) -> String {
    text.chars().filter_map(defuse).collect()
}

/// A sentence as the remote synthesizer gets it: with her mood of this
/// moment as a direction, when it has no voice mark of its own (decision
/// 0029). The Windows voice never gets one.
fn directed(text: &str, direction: Option<&DirectionSource>) -> String {
    let mood = direction.and_then(|mood| mood());
    with_direction(text, mood.as_deref())
}

impl WindowsVoice {
    /// Queues a sentence. A remote synthesizer, if any, is asked for
    /// `remote`; the Windows voice says `local` when there is none or it fails.
    fn queue(&self, remote: impl FnOnce() -> String, local: &str) {
        let gen = self.generation.load(Ordering::Relaxed);
        // The sound is asked for ahead of playback, but never more than
        // `LOOKAHEAD` sentences ahead: each request is paid.
        let remote = self.remote.lock().clone().map(|synth| {
            let (tx, rx) = mpsc::channel();
            // Asked only when a remote synthesizer is set.
            let direction = self.direction.lock().clone();
            self.prefetch.submit(Job {
                generation: gen,
                text: directed(&remote(), direction.as_ref()),
                tx,
                synth,
            });
            rx
        });
        self.pending.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.lock().send(Cmd::Speak(Sentence {
            text: local.to_string(),
            generation: gen,
            remote,
            asked: std::time::Instant::now(),
            first: self.fresh.swap(false, Ordering::Relaxed),
            tone: self.tone.lock().as_ref().map_or((0, 0), |t| t()),
        }));
    }
}

impl Voice for WindowsVoice {
    fn speak(&self, text: &str) {
        self.queue(|| remote_plain(text), text);
    }

    /// Words of the model without voice marks reach the remote synthesizer
    /// as they are, as before decision 0026.
    fn speak_reply(&self, text: &str) {
        self.queue(|| text.to_string(), text);
    }

    /// The remote synthesizer performs the marks; the Windows voice, also
    /// when the remote one fails, says the words without them.
    fn speak_marked(&self, marked: &str, plain: &str) {
        self.queue(|| marked.to_string(), plain);
    }

    fn set_tone(&self, tone: Option<ToneSource>) {
        *self.tone.lock() = tone;
    }

    fn set_direction(&self, direction: Option<DirectionSource>) {
        *self.direction.lock() = direction;
    }

    fn set_observer(&self, observer: Option<FirstSound>) {
        *self.observer.lock() = observer;
    }

    fn set_lips(&self, sink: Option<LipSink>) {
        *self.lips.lock() = sink;
    }

    fn busy(&self) -> bool {
        self.pending.load(Ordering::Relaxed) > 0
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

    /// 16-bit mono at 24 kHz that starts loud.
    fn loud_wav(frames: usize) -> Vec<u8> {
        let mut pcm = Vec::new();
        for i in 0..frames {
            let v: i16 = if i % 2 == 0 { 9_000 } else { -9_000 };
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        wav(&pcm, 24_000, 1, 16)
    }

    /// Silent MPEG-1 Layer III frames, mono, 128 kbit/s at 44.1 kHz.
    fn silent_mp3(frames: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..frames {
            let mut frame = vec![0u8; 417];
            frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0xC0]);
            out.extend_from_slice(&frame);
        }
        out
    }

    #[test]
    fn the_levels_match_the_sound_that_plays() {
        // 200 ms of sound at 24 kHz gets 150 ms of quiet in front: 350 ms, 18 steps.
        let src = loud_wav(4_800);
        let (play, levels) = prepare(&src);
        assert!(matches!(play, Cow::Owned(_)));
        let levels = levels.expect("a WAV has levels");
        assert_eq!(levels.len(), 18);
        assert!(levels[..7].iter().all(|&l| l == 0), "{levels:?}");
        assert!(levels[8..].iter().all(|&l| l > 200), "{levels:?}");
        // Bytes that are no sound play as they are, with no levels.
        let (play, levels) = prepare(b"no sound here");
        assert!(matches!(play, Cow::Borrowed(_)));
        assert_eq!(levels, None);
    }

    #[test]
    fn every_phrase_starts_with_quiet() {
        let src = loud_wav(4_800);
        let out = prepare(&src).0;
        assert!(matches!(out, Cow::Owned(_)));
        // 150 ms of zeros at 24 kHz, then the sound as it was.
        assert_eq!(out.len(), src.len() + 3_600 * 2);
        assert!(out[44..44 + 7_200].iter().all(|&b| b == 0));
        assert_eq!(&out[44 + 7_200..], &src[44..]);
        // Quiet already there: the sound stays as it is.
        let quiet = prepare(&out).0.into_owned();
        assert_eq!(quiet, *out);
        // Bytes that are no sound are played as they are.
        assert!(matches!(prepare(b"no sound here").0, Cow::Borrowed(_)));
    }

    #[test]
    fn a_compressed_sound_is_decoded_for_its_quiet() {
        // SAFETY: COM for this test thread, as the voice worker does.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let decoded = decode(&silent_mp3(40)).expect("MP3 is decoded");
        assert!(decoded.starts_with(b"RIFF"));
        // About a second of mono 16-bit sound at 44.1 kHz.
        let samples = (decoded.len() - 44) / 2;
        assert!((30_000..=50_000).contains(&samples), "{samples} samples");
        assert!(decode(b"no sound here").is_err());
    }

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

    #[test]
    fn text_of_others_reaches_a_remote_voice_without_marks() {
        assert_eq!(
            remote_plain("[Фото] от Ани: <whisper>привет</whisper> [sigh]"),
            "(Фото) от Ани: whisperпривет/whisper (sigh)"
        );
        assert_eq!(remote_plain("Напоминаю: чайник."), "Напоминаю: чайник.");
    }

    #[test]
    fn a_remote_sentence_without_marks_gets_her_mood_as_a_direction() {
        let asked = Arc::new(AtomicUsize::new(0));
        let count = asked.clone();
        let sad: DirectionSource = Arc::new(move || {
            count.fetch_add(1, Ordering::Relaxed);
            Some("sad, quietly".to_string())
        });
        // Words of the model without marks and her own lines.
        assert_eq!(
            directed("Ну вот, всё готово.", Some(&sad)),
            "[sad, quietly] Ну вот, всё готово."
        );
        // Text of others keeps its brackets harmless; the direction is hers.
        assert_eq!(
            directed(&remote_plain("[Фото] от Ани: <b>привет</b>"), Some(&sad)),
            "[sad, quietly] (Фото) от Ани: bпривет/b"
        );
        // A phrase the model marked itself stays as it is.
        assert_eq!(
            directed("[sigh] Ну вот, всё готово.", Some(&sad)),
            "[sigh] Ну вот, всё готово."
        );
        assert_eq!(
            directed("<whisper>Тихо.</whisper>", Some(&sad)),
            "<whisper>Тихо.</whisper>"
        );
        // The mood is asked for each sentence anew.
        assert_eq!(asked.load(Ordering::Relaxed), 4);

        // She is calm, or the switch is off: nothing is added.
        let calm: DirectionSource = Arc::new(|| None);
        assert_eq!(directed("Ну вот.", Some(&calm)), "Ну вот.");
        assert_eq!(directed("Ну вот.", None), "Ну вот.");
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

    #[test]
    fn the_automatic_voice_follows_the_given_language() {
        let voices = vec![
            v("Microsoft David", "en-US"),
            v("Microsoft Irina", "ru-RU"),
            v("Microsoft Pavel", "ru-RU"),
        ];
        assert_eq!(pick_for(&voices, "", "en").unwrap().name, "Microsoft David");
        assert_eq!(pick_for(&voices, "", "ru").unwrap().name, "Microsoft Irina");
        // No English voice: the synthesizer keeps its default one.
        assert_eq!(pick_for(&voices[1..], "", "en"), None);
        // A voice picked by name wins for any language.
        for language in ["ru", "en"] {
            assert_eq!(
                pick_for(&voices, "Microsoft Pavel", language).unwrap().name,
                "Microsoft Pavel"
            );
        }
        assert_eq!(
            pick_for(&voices, "Нет такого", "en").unwrap().name,
            "Microsoft David"
        );
    }
}
