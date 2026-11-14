//! Sound written to a WAV file for a screen recording: everything the
//! computer plays (WASAPI loopback of the default output device), or the
//! microphone chosen in the settings (decision 0032).
//!
//! The file follows the clock. A loopback stream gives nothing while nothing
//! plays, so the gaps are filled with silence: the sound must not run ahead
//! of the picture it is mixed with later.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use mika_core::sound_capture::{SoundCapture, SoundSource, SoundStats, SoundTake};

use crate::mic::SystemMicrophone;

/// The file may lag behind the clock by this much before silence is added:
/// chunks come in bursts.
const SLACK: Duration = Duration::from_millis(60);
/// Without chunks for this long the file is padded up to the clock.
const PAD_EVERY: Duration = Duration::from_secs(1);
/// How often the writer looks at its stop flag.
const TICK: Duration = Duration::from_millis(100);
/// Longest wait for the device to start.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// Samples under this are silence.
const QUIET: f32 = 1.0e-4;

/// Frames of silence to write before sound that belongs at `elapsed` since
/// the start: 0 while the file is within 60 ms of the clock, else the
/// missing frames.
pub fn silence_before(written_frames: u64, rate: u32, elapsed: Duration) -> u64 {
    let missing = frames_in(rate, elapsed).saturating_sub(written_frames);
    if missing <= frames_in(rate, SLACK) {
        0
    } else {
        missing
    }
}

/// Whole frames that `time` holds at `rate`.
fn frames_in(rate: u32, time: Duration) -> u64 {
    (time.as_nanos() * rate as u128 / 1_000_000_000) as u64
}

/// Header of a PCM 16 bit WAV file with `data_len` bytes of samples.
pub fn wav_header(rate: u32, channels: u16, data_len: u32) -> [u8; 44] {
    let block = channels * 2;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&data_len.saturating_add(36).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * block as u32).to_le_bytes());
    h[32..34].copy_from_slice(&block.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// The first two channels of interleaved sound with more of them; one or two
/// channels stay as they are.
pub fn first_two(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 2 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .flat_map(|frame| [frame[0], frame[1]])
        .collect()
}

/// Sound of this computer for a recording.
pub struct SystemSound {
    mic: Arc<SystemMicrophone>,
}

impl SystemSound {
    /// `mic` is the microphone of the voice input: the same device is recorded.
    pub fn new(mic: Arc<SystemMicrophone>) -> Self {
        Self { mic }
    }
}

/// Starts the loopback of the default output device: interleaved chunks with
/// one or two channels go to `tx`. Returns the stream, the rate and the channels.
fn open_loopback(tx: mpsc::Sender<Vec<f32>>) -> Result<(cpal::Stream, u32, u16), String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("нет устройства вывода звука")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let channels = config.channels() as usize;
    let rate = config.sample_rate();
    let format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

    let on_error = |e| log::warn!("loopback stream error: {e}");
    // An input stream on an output device is its loopback on WASAPI.
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = tx.send(first_two(data, channels));
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let floats: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                let _ = tx.send(first_two(&floats, channels));
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            stream_config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let floats: Vec<f32> = data
                    .iter()
                    .map(|s| (*s as f32 - 32768.0) / 32768.0)
                    .collect();
                let _ = tx.send(first_two(&floats, channels));
            },
            on_error,
            None,
        ),
        other => return Err(format!("формат звука {other:?} не поддержан")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, rate, channels.clamp(1, 2) as u16))
}

/// The file of a take and how much of it is written.
struct WavFile {
    out: BufWriter<File>,
    rate: u32,
    channels: u16,
    /// Frames written.
    frames: u64,
    /// Nothing louder than silence was written.
    silent: bool,
    /// The first write error; nothing is written after it.
    failed: Option<String>,
}

impl WavFile {
    fn create(path: &Path, rate: u32, channels: u16) -> Result<Self, String> {
        let file = File::create(path).map_err(|e| format!("не удалось создать файл звука: {e}"))?;
        let mut out = BufWriter::new(file);
        out.write_all(&wav_header(rate, channels, 0))
            .map_err(|e| format!("не удалось записать звук: {e}"))?;
        Ok(Self {
            out,
            rate,
            channels,
            frames: 0,
            silent: true,
            failed: None,
        })
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.failed.is_some() {
            return;
        }
        if let Err(e) = self.out.write_all(bytes) {
            log::warn!("sound take cannot write: {e}");
            self.failed = Some(format!("не удалось записать звук: {e}"));
        }
    }

    fn silence(&mut self, frames: u64) {
        if frames == 0 {
            return;
        }
        // At most a second of zeros at a time.
        let chunk = vec![0u8; self.rate.max(1) as usize * self.channels as usize * 2];
        let mut left = frames * self.channels as u64 * 2;
        while left > 0 {
            let n = left.min(chunk.len() as u64) as usize;
            self.write(&chunk[..n]);
            left -= n as u64;
        }
        self.frames += frames;
    }

    /// A chunk that ended `elapsed` after the start: silence up to its
    /// beginning when the file lags, then the chunk.
    fn chunk(&mut self, samples: &[f32], elapsed: Duration) {
        let channels = self.channels as usize;
        let frames = (samples.len() / channels) as u64;
        if frames == 0 {
            return;
        }
        let length =
            Duration::from_nanos(frames.saturating_mul(1_000_000_000) / self.rate.max(1) as u64);
        let gap = silence_before(self.frames, self.rate, elapsed.saturating_sub(length));
        self.silence(gap);
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in &samples[..frames as usize * channels] {
            if s.abs() > QUIET {
                self.silent = false;
            }
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        self.write(&bytes);
        self.frames += frames;
    }

    /// Pads to `elapsed` and writes the sizes into the header.
    fn finish(mut self, elapsed: Duration) -> Result<SoundStats, String> {
        self.silence(frames_in(self.rate, elapsed).saturating_sub(self.frames));
        if let Some(e) = self.failed {
            return Err(e);
        }
        let data_len =
            u32::try_from(self.frames * self.channels as u64 * 2).unwrap_or(u32::MAX - 36);
        let fail = |e: std::io::Error| format!("не удалось записать звук: {e}");
        self.out.flush().map_err(fail)?;
        self.out.seek(SeekFrom::Start(0)).map_err(fail)?;
        self.out
            .write_all(&wav_header(self.rate, self.channels, data_len))
            .map_err(fail)?;
        self.out.flush().map_err(fail)?;
        Ok(SoundStats {
            seconds: self.frames as f64 / self.rate as f64,
            silent: self.silent,
        })
    }
}

/// The body of the writer thread: opens the device, tells `ready` how that
/// went, writes until `stop` is set.
fn write_take(
    source: SoundSource,
    mic: Arc<SystemMicrophone>,
    path: PathBuf,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
) -> Result<SoundStats, String> {
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    let opened = match source {
        SoundSource::System => open_loopback(tx),
        SoundSource::Microphone => mic
            .open(tx)
            .map(|(stream, rate)| (stream, rate, 1))
            .map_err(|e| e.to_string()),
    };
    let started = opened.and_then(|(stream, rate, channels)| {
        WavFile::create(&path, rate, channels).map(|file| (stream, file))
    });
    let (stream, mut file) = match started {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }
    };
    // The file's time starts here.
    let began = Instant::now();
    let _ = ready.send(Ok(()));
    let mut last = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(TICK) {
            Ok(chunk) => {
                file.chunk(&chunk, began.elapsed());
                last = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // The stream ended by itself: the take goes on in silence.
            Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(TICK),
        }
        if last.elapsed() >= PAD_EVERY {
            let gap = silence_before(file.frames, file.rate, began.elapsed());
            file.silence(gap);
            last = Instant::now();
        }
    }
    let elapsed = began.elapsed();
    drop(stream);
    file.finish(elapsed)
}

/// A running take: its writer thread ends when it is stopped or dropped.
struct Take {
    stop: Arc<AtomicBool>,
    writer: Option<std::thread::JoinHandle<Result<SoundStats, String>>>,
}

impl SoundTake for Take {
    fn stop(mut self: Box<Self>) -> Result<SoundStats, String> {
        self.stop.store(true, Ordering::Relaxed);
        match self.writer.take() {
            Some(writer) => writer
                .join()
                .map_err(|_| "запись звука оборвалась".to_string())?,
            None => Err("запись звука уже остановлена".into()),
        }
    }
}

impl Drop for Take {
    fn drop(&mut self) {
        // A take nobody stopped still closes its file.
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl SoundCapture for SystemSound {
    fn start(&self, source: SoundSource, wav: &Path) -> Result<Box<dyn SoundTake>, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (mic, path, flag) = (self.mic.clone(), wav.to_path_buf(), stop.clone());
        // The stream lives and dies on the writer's own thread.
        let writer = std::thread::Builder::new()
            .name("mika-sound-take".into())
            .spawn(move || write_take(source, mic, path, flag, ready_tx))
            .map_err(|e| format!("не удалось начать запись звука: {e}"))?;
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => Ok(Box::new(Take {
                stop,
                writer: Some(writer),
            })),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err("устройство звука не ответило".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_fills_only_a_real_gap() {
        let ms = Duration::from_millis;
        // On time, and 50 ms behind: bursts of chunks, nothing is added.
        assert_eq!(silence_before(48_000, 48_000, ms(1000)), 0);
        assert_eq!(silence_before(48_000, 48_000, ms(1050)), 0);
        assert_eq!(silence_before(48_000, 48_000, ms(1060)), 0);
        // Two seconds without sound: the whole gap is filled.
        assert_eq!(silence_before(48_000, 48_000, ms(3000)), 96_000);
        assert_eq!(silence_before(0, 44_100, ms(500)), 22_050);
        // The file ahead of the clock gets nothing.
        assert_eq!(silence_before(96_000, 48_000, ms(1000)), 0);
        assert_eq!(silence_before(0, 48_000, Duration::ZERO), 0);
    }

    #[test]
    fn header_describes_16_bit_pcm() {
        let h = wav_header(48_000, 2, 192_000);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 192_036);
        assert_eq!(&h[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(h[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(h[20..22].try_into().unwrap()), 1, "PCM");
        assert_eq!(u16::from_le_bytes(h[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(h[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u32::from_le_bytes(h[28..32].try_into().unwrap()), 192_000);
        assert_eq!(u16::from_le_bytes(h[32..34].try_into().unwrap()), 4);
        assert_eq!(u16::from_le_bytes(h[34..36].try_into().unwrap()), 16);
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 192_000);
        let mono = wav_header(16_000, 1, 0);
        assert_eq!(u32::from_le_bytes(mono[28..32].try_into().unwrap()), 32_000);
        assert_eq!(u16::from_le_bytes(mono[32..34].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(mono[4..8].try_into().unwrap()), 36);
    }

    #[test]
    fn surround_sound_keeps_its_first_two_channels() {
        let six = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.2];
        assert_eq!(first_two(&six, 6), vec![0.1, 0.2, 0.7, 0.8]);
        assert_eq!(
            first_two(&[0.1, 0.2, 0.3, 0.4], 2),
            vec![0.1, 0.2, 0.3, 0.4]
        );
        assert_eq!(first_two(&[0.1, 0.2], 1), vec![0.1, 0.2]);
        // A cut frame at the end is dropped.
        assert_eq!(first_two(&[0.1, 0.2, 0.3, 0.4], 3), vec![0.1, 0.2]);
        assert!(first_two(&[], 6).is_empty());
    }

    fn data_len(path: &Path) -> (u32, u64) {
        let bytes = std::fs::read(path).unwrap();
        (
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            bytes.len() as u64 - 44,
        )
    }

    #[test]
    fn a_file_follows_the_clock_and_ends_with_true_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("take.wav");
        let mut f = WavFile::create(&path, 1000, 2).unwrap();
        // 100 ms of sound that ended at 100 ms: nothing to fill.
        f.chunk(&[0.5; 200], Duration::from_millis(100));
        assert_eq!(f.frames, 100);
        // The next 100 ms came at one second: 800 ms of silence first.
        f.chunk(&[0.0; 200], Duration::from_millis(1000));
        assert_eq!(f.frames, 1000);
        // A chunk with half a frame at its end keeps whole frames only.
        f.chunk(&[0.0; 3], Duration::from_millis(1001));
        assert_eq!(f.frames, 1001);
        let stats = f.finish(Duration::from_millis(1500)).unwrap();
        assert_eq!(stats.seconds, 1.5, "padded to the stop moment");
        assert!(!stats.silent);
        let (told, real) = data_len(&path);
        assert_eq!((told, real), (6000, 6000));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..44], &wav_header(1000, 2, 6000));
        assert_eq!(
            i16::from_le_bytes(bytes[44..46].try_into().unwrap()),
            i16::MAX / 2
        );

        let quiet = dir.path().join("quiet.wav");
        let mut f = WavFile::create(&quiet, 1000, 1).unwrap();
        f.chunk(&[0.00001; 10], Duration::from_millis(10));
        let stats = f.finish(Duration::from_millis(10)).unwrap();
        assert!(stats.silent, "noise under the floor is silence");
        assert_eq!(data_len(&quiet), (20, 20));
    }

    /// Three seconds of what the computer plays, to a temporary file.
    /// `cargo test -p mika-audio live_loopback_take -- --ignored --nocapture`
    #[test]
    #[ignore = "needs a sound output device"]
    fn live_loopback_take() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loopback.wav");
        let sound = SystemSound::new(Arc::new(SystemMicrophone::default()));
        let take = match sound.start(SoundSource::System, &path) {
            Ok(t) => t,
            Err(e) => {
                println!("no loopback here: {e}");
                return;
            }
        };
        std::thread::sleep(Duration::from_secs(3));
        let stats = take.stop().unwrap();
        println!("{stats:?}");
        assert!((2.9..3.6).contains(&stats.seconds), "{stats:?}");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        let rate = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let channels = u16::from_le_bytes(bytes[22..24].try_into().unwrap());
        let (told, real) = data_len(&path);
        assert_eq!(told as u64, real);
        let seconds = real as f64 / (rate as f64 * channels as f64 * 2.0);
        assert!((seconds - stats.seconds).abs() < 0.01, "{seconds}");
    }
}
