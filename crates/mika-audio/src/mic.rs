//! Microphone capture for one spoken phrase: record until the speaker stops,
//! then hand back a 16 kHz mono WAV file for recognition.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Sample rate of the file sent to recognition.
pub const TARGET_RATE: u32 = 16_000;
/// Loudness is measured over frames of this length.
pub const FRAME_MS: u32 = 30;
/// Silence after speech that ends the phrase.
pub const END_SILENCE_MS: u32 = 1100;
/// Recording without any speech is given up after this.
pub const NO_SPEECH_MS: u32 = 7000;
/// Longest phrase.
pub const MAX_PHRASE_MS: u32 = 45_000;
/// Frames used to measure the room noise.
const NOISE_FRAMES: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MicError {
    #[error(
        "микрофон не найден: подключи его или выбери устройство записи в параметрах звука Windows"
    )]
    NoDevice,
    #[error("ничего не услышала")]
    NoSpeech,
    #[error("запись отменена")]
    Cancelled,
    #[error("микрофон недоступен: {0}")]
    Failed(String),
}

/// Lets another thread end a recording.
#[derive(Debug, Default)]
pub struct MicControl {
    /// 0 while recording, 1 finished, 2 cancelled.
    state: AtomicU8,
    /// A key is held down: the pause after speech does not end the recording.
    hold: AtomicBool,
}

impl MicControl {
    /// Stop and use what was said so far.
    pub fn finish(&self) {
        let _ = self
            .state
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// Stop and throw the recording away.
    pub fn cancel(&self) {
        self.state.store(2, Ordering::Relaxed);
    }

    /// While set, the pause after speech does not end the recording: the
    /// speaker holds a key and lets it go when done. Silence from the start
    /// and the length limit still end it.
    pub fn set_hold(&self, on: bool) {
        self.hold.store(on, Ordering::Relaxed);
    }

    pub fn held(&self) -> bool {
        self.hold.load(Ordering::Relaxed)
    }

    fn finished(&self) -> bool {
        self.state.load(Ordering::Relaxed) == 1
    }

    fn cancelled(&self) -> bool {
        self.state.load(Ordering::Relaxed) == 2
    }
}

pub trait Microphone: Send + Sync {
    /// Records one phrase. Blocks until the speaker stops or `control` ends it.
    fn record(&self, control: &MicControl) -> Result<Vec<u8>, MicError>;
    /// True when Windows has a recording device.
    fn available(&self) -> bool;
}

/// Averages interleaved channels into one.
pub fn to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Changes the sample rate. Going down, each output sample is the mean of the
/// input samples it covers, which is enough of a low-pass filter for speech.
pub fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() || from == 0 || to == 0 {
        return samples.to_vec();
    }
    let out_len = (samples.len() as u64 * to as u64 / from as u64) as usize;
    let step = from as f64 / to as f64;
    (0..out_len)
        .map(|i| {
            let start = i as f64 * step;
            if step > 1.0 {
                let a = start as usize;
                let b = (((i + 1) as f64 * step) as usize).clamp(a + 1, samples.len());
                samples[a..b].iter().sum::<f32>() / (b - a) as f32
            } else {
                let a = (start as usize).min(samples.len() - 1);
                let b = (a + 1).min(samples.len() - 1);
                let t = (start - a as f64) as f32;
                samples[a] * (1.0 - t) + samples[b] * t
            }
        })
        .collect()
}

/// A 16-bit mono WAV file.
pub fn encode_wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Root mean square of a frame: its loudness, 0..=1.
pub fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadStep {
    Continue,
    /// The speaker stopped, or the phrase hit the length limit.
    Done,
    /// Nobody spoke.
    NoSpeech,
}

/// Decides when a phrase has ended, from the loudness of 30 ms frames.
/// The threshold follows the noise of the room measured in the first frames.
#[derive(Debug, Default)]
pub struct Vad {
    frames: u32,
    noise_sum: f32,
    loud_run: u32,
    speaking: bool,
    silence_ms: u32,
}

impl Vad {
    fn threshold(&self) -> f32 {
        let measured = self.frames.clamp(1, NOISE_FRAMES) as f32;
        // A user who starts talking at once must not raise the bar out of reach.
        let noise = (self.noise_sum / measured).min(0.02);
        (noise * 3.0).max(0.012)
    }

    pub fn heard_speech(&self) -> bool {
        self.speaking
    }

    /// Feeds the loudness of the next frame.
    pub fn push(&mut self, level: f32) -> VadStep {
        if self.frames < NOISE_FRAMES {
            self.noise_sum += level;
        }
        self.frames += 1;
        let elapsed = self.frames * FRAME_MS;
        if level > self.threshold() {
            self.loud_run += 1;
            self.silence_ms = 0;
            // Two loud frames in a row: a click or a key press is one.
            if self.loud_run >= 2 {
                self.speaking = true;
            }
        } else {
            self.loud_run = 0;
            if self.speaking {
                self.silence_ms += FRAME_MS;
            }
        }
        if self.speaking && self.silence_ms >= END_SILENCE_MS {
            VadStep::Done
        } else if elapsed >= MAX_PHRASE_MS {
            if self.speaking {
                VadStep::Done
            } else {
                VadStep::NoSpeech
            }
        } else if !self.speaking && elapsed >= NO_SPEECH_MS {
            VadStep::NoSpeech
        } else {
            VadStep::Continue
        }
    }
}

/// Whether the recording ends at this step. While a key is held (`held`) the
/// pause after speech does not end it; the length limit and silence from the
/// start do. `elapsed_ms` is the sound measured so far.
pub fn ends_phrase(step: VadStep, held: bool, elapsed_ms: u32) -> bool {
    match step {
        VadStep::Continue => false,
        VadStep::NoSpeech => true,
        VadStep::Done => !held || elapsed_ms >= MAX_PHRASE_MS,
    }
}

/// A recording device of Windows: the one named in the settings, or the default.
#[derive(Default)]
pub struct SystemMicrophone {
    /// Name from the settings; empty means the default device.
    wanted: parking_lot::RwLock<String>,
}

/// Index of the device to record from: the wanted name when it is still there.
pub fn pick_device(names: &[String], wanted: &str) -> Option<usize> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return None;
    }
    names.iter().position(|n| n == wanted)
}

fn device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_string())
}

impl SystemMicrophone {
    pub fn new(wanted: &str) -> Self {
        Self {
            wanted: parking_lot::RwLock::new(wanted.to_string()),
        }
    }

    pub fn set_device(&self, wanted: &str) {
        *self.wanted.write() = wanted.trim().to_string();
    }

    /// Names of the recording devices Windows has now.
    pub fn devices() -> Vec<String> {
        cpal::default_host()
            .input_devices()
            .map(|list| list.filter_map(|d| device_name(&d)).collect())
            .unwrap_or_default()
    }

    fn device(&self) -> Option<cpal::Device> {
        let host = cpal::default_host();
        let wanted = self.wanted.read().clone();
        if !wanted.is_empty() {
            if let Ok(list) = host.input_devices() {
                let all: Vec<cpal::Device> = list.collect();
                let names: Vec<String> = all
                    .iter()
                    .map(|d| device_name(d).unwrap_or_default())
                    .collect();
                if let Some(i) = pick_device(&names, &wanted) {
                    return all.into_iter().nth(i);
                }
                log::warn!("recording device from the settings is gone, using the default");
            }
        }
        host.default_input_device()
    }
}

fn failed(e: impl std::fmt::Display) -> MicError {
    MicError::Failed(e.to_string())
}

impl SystemMicrophone {
    /// Starts the device and sends mono chunks at its own rate to `tx`.
    /// Sound flows while the returned stream lives.
    pub fn open(&self, tx: mpsc::Sender<Vec<f32>>) -> Result<(cpal::Stream, u32), MicError> {
        let device = self.device().ok_or(MicError::NoDevice)?;
        let config = device.default_input_config().map_err(failed)?;
        let channels = config.channels() as usize;
        let rate = config.sample_rate();
        let format = config.sample_format();
        let stream_config: cpal::StreamConfig = config.into();

        // A broken stream may call this again and again: once is enough.
        let told = std::sync::atomic::AtomicBool::new(false);
        let on_error = move |e| {
            if !told.swap(true, std::sync::atomic::Ordering::Relaxed) {
                log::warn!("microphone stream error: {e}");
            }
        };
        let stream = match format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let _ = tx.send(to_mono(data, channels));
                },
                on_error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                stream_config,
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let floats: Vec<f32> =
                        data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                    let _ = tx.send(to_mono(&floats, channels));
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
                    let _ = tx.send(to_mono(&floats, channels));
                },
                on_error,
                None,
            ),
            other => {
                return Err(MicError::Failed(format!(
                    "формат звука {other:?} не поддержан"
                )))
            }
        }
        .map_err(failed)?;
        stream.play().map_err(failed)?;
        Ok((stream, rate))
    }
}

impl Microphone for SystemMicrophone {
    fn available(&self) -> bool {
        self.device().is_some()
    }

    fn record(&self, control: &MicControl) -> Result<Vec<u8>, MicError> {
        let (tx, rx) = mpsc::channel::<Vec<f32>>();
        let (stream, rate) = self.open(tx).inspect_err(|e| {
            log::warn!(
                "microphone did not open: outcome={} error={e}",
                outcome_name(e)
            );
        })?;
        let opened = std::time::Instant::now();
        log::info!("microphone opened: rate={rate}");

        let frame_len = (rate as u64 * FRAME_MS as u64 / 1000).max(1) as usize;
        let mut samples: Vec<f32> = Vec::new();
        let mut measured = 0usize;
        let mut frames = 0u32;
        let mut vad = Vad::default();
        let outcome = loop {
            if control.cancelled() {
                break Err(MicError::Cancelled);
            }
            if control.finished() {
                break if vad.heard_speech() {
                    Ok(())
                } else {
                    Err(MicError::NoSpeech)
                };
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(chunk) => samples.extend(chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(MicError::Failed("запись оборвалась".into()))
                }
            }
            let mut end = VadStep::Continue;
            while samples.len() - measured >= frame_len {
                let step = vad.push(rms(&samples[measured..measured + frame_len]));
                measured += frame_len;
                frames += 1;
                // A pause under a held key is not the end: keep measuring.
                if ends_phrase(step, control.held(), frames * FRAME_MS) {
                    end = step;
                    break;
                }
            }
            match end {
                VadStep::Continue => {}
                VadStep::Done => break Ok(()),
                VadStep::NoSpeech => break Err(MicError::NoSpeech),
            }
        };
        drop(stream);
        // What was said is not kept here, only how long the microphone was open.
        log::info!(
            "microphone closed: ms={} sound_s={:.1} outcome={}",
            opened.elapsed().as_millis(),
            samples.len() as f64 / f64::from(rate.max(1)),
            outcome.as_ref().map_or_else(outcome_name, |()| "phrase")
        );
        if let Err(e @ MicError::Failed(_)) = &outcome {
            log::warn!("recording broke off: {e}");
        }
        outcome?;
        Ok(encode_wav(
            &resample(&samples, rate, TARGET_RATE),
            TARGET_RATE,
        ))
    }
}

/// How a recording ended without a phrase, as the journal names it.
fn outcome_name(error: &MicError) -> &'static str {
    match error {
        MicError::NoDevice => "no_device",
        MicError::NoSpeech => "no_speech",
        MicError::Cancelled => "cancelled",
        MicError::Failed(_) => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(levels: impl IntoIterator<Item = f32>) -> (VadStep, u32, bool) {
        let mut vad = Vad::default();
        let mut frames = 0;
        for level in levels {
            frames += 1;
            let step = vad.push(level);
            if step != VadStep::Continue {
                return (step, frames * FRAME_MS, vad.heard_speech());
            }
        }
        (VadStep::Continue, frames * FRAME_MS, vad.heard_speech())
    }

    fn quiet(ms: u32) -> impl Iterator<Item = f32> {
        std::iter::repeat_n(0.002, (ms / FRAME_MS) as usize)
    }

    fn loud(ms: u32) -> impl Iterator<Item = f32> {
        std::iter::repeat_n(0.2, (ms / FRAME_MS) as usize)
    }

    #[test]
    fn phrase_ends_after_a_pause() {
        let (step, at, heard) = run(quiet(600).chain(loud(1500)).chain(quiet(3000)));
        assert_eq!(step, VadStep::Done);
        assert!(heard);
        // Speech ended at 2100 ms; the pause that closes it is 1100 ms.
        assert!((3180..=3240).contains(&at), "{at}");
    }

    #[test]
    fn a_short_pause_inside_the_phrase_does_not_end_it() {
        let (step, _, _) = run(quiet(300)
            .chain(loud(600))
            .chain(quiet(700))
            .chain(loud(600)));
        assert_eq!(step, VadStep::Continue);
    }

    #[test]
    fn silence_gives_up_after_seven_seconds() {
        let (step, at, heard) = run(quiet(20_000));
        assert_eq!(step, VadStep::NoSpeech);
        assert!(!heard);
        // The limit falls inside a frame: the decision comes at the end of that frame.
        assert!(
            (NO_SPEECH_MS..NO_SPEECH_MS + FRAME_MS).contains(&at),
            "{at}"
        );
    }

    #[test]
    fn a_single_click_is_not_speech() {
        let mut levels: Vec<f32> = quiet(900).collect();
        levels[15] = 0.5;
        let (step, _, heard) = run(levels.into_iter().chain(quiet(7000)));
        assert_eq!(step, VadStep::NoSpeech);
        assert!(!heard);
    }

    #[test]
    fn speech_from_the_first_frame_is_still_heard() {
        let (step, _, heard) = run(loud(1200).chain(quiet(2000)));
        assert_eq!(step, VadStep::Done);
        assert!(heard);
    }

    #[test]
    fn endless_speech_is_cut_at_the_limit() {
        let (step, at, _) = run(loud(60_000));
        assert_eq!(step, VadStep::Done);
        assert_eq!(at, MAX_PHRASE_MS);
    }

    #[test]
    fn noisy_room_raises_the_threshold() {
        // Steady hum at 0.015: below three times itself, so it is not speech.
        let hum = std::iter::repeat_n(0.015f32, 300);
        let (step, _, heard) = run(hum);
        assert_eq!(step, VadStep::NoSpeech);
        assert!(!heard);
    }

    #[test]
    fn mono_and_resample() {
        assert_eq!(to_mono(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(to_mono(&[0.25, 0.75], 1), vec![0.25, 0.75]);
        // 48 kHz to 16 kHz: every three samples become their mean.
        let down = resample(&[0.0, 0.3, 0.6, 1.0, 1.0, 1.0], 48_000, 16_000);
        assert_eq!(down.len(), 2);
        assert!((down[0] - 0.3).abs() < 1e-6 && (down[1] - 1.0).abs() < 1e-6);
        // 8 kHz to 16 kHz: twice as many, values in between.
        let up = resample(&[0.0, 1.0], 8_000, 16_000);
        assert_eq!(up.len(), 4);
        assert!((up[1] - 0.5).abs() < 1e-6);
        assert_eq!(resample(&[0.1, 0.2], 16_000, 16_000), vec![0.1, 0.2]);
        assert!(resample(&[], 48_000, 16_000).is_empty());
    }

    #[test]
    fn wav_header_and_samples() {
        let wav = encode_wav(&[0.0, 1.0, -1.0, 2.0], TARGET_RATE);
        assert_eq!(wav.len(), 44 + 8);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(
            i16::from_le_bytes(wav[46..48].try_into().unwrap()),
            i16::MAX
        );
        assert_eq!(
            i16::from_le_bytes(wav[48..50].try_into().unwrap()),
            -i16::MAX
        );
        // Out-of-range samples are clipped, not wrapped.
        assert_eq!(
            i16::from_le_bytes(wav[50..52].try_into().unwrap()),
            i16::MAX
        );
    }

    #[test]
    fn control_finish_does_not_override_cancel() {
        let c = MicControl::default();
        assert!(!c.finished() && !c.cancelled());
        c.cancel();
        c.finish();
        assert!(c.cancelled() && !c.finished());
        let c = MicControl::default();
        c.finish();
        assert!(c.finished());
    }

    #[test]
    fn hold_does_not_disturb_finish_and_cancel() {
        let c = MicControl::default();
        assert!(!c.held());
        c.set_hold(true);
        assert!(c.held() && !c.finished() && !c.cancelled());
        c.finish();
        assert!(c.finished() && c.held());
        c.set_hold(false);
        assert!(c.finished() && !c.held());
        let c = MicControl::default();
        c.set_hold(true);
        c.cancel();
        c.set_hold(false);
        assert!(c.cancelled() && !c.finished());
    }

    #[test]
    fn a_held_key_keeps_the_recording_through_a_pause() {
        // Not held: as before.
        assert!(!ends_phrase(VadStep::Continue, false, 3000));
        assert!(ends_phrase(VadStep::Done, false, 3000));
        assert!(ends_phrase(VadStep::NoSpeech, false, NO_SPEECH_MS));
        // Held: the pause is not the end, the limit and silence are.
        assert!(!ends_phrase(VadStep::Continue, true, 3000));
        assert!(!ends_phrase(VadStep::Done, true, 3000));
        assert!(!ends_phrase(VadStep::Done, true, MAX_PHRASE_MS - FRAME_MS));
        assert!(ends_phrase(VadStep::Done, true, MAX_PHRASE_MS));
        assert!(ends_phrase(VadStep::NoSpeech, true, NO_SPEECH_MS));
        assert!(ends_phrase(VadStep::NoSpeech, true, MAX_PHRASE_MS));
    }

    /// The frame loop of `record` without a device: when the phrase ends if
    /// the key is held for the first `held_ms` of the recording.
    fn record_ends(levels: impl IntoIterator<Item = f32>, held_ms: u32) -> (VadStep, u32) {
        let mut vad = Vad::default();
        let mut frames = 0;
        for level in levels {
            let step = vad.push(level);
            frames += 1;
            let elapsed = frames * FRAME_MS;
            if ends_phrase(step, elapsed <= held_ms, elapsed) {
                return (step, elapsed);
            }
        }
        (VadStep::Continue, frames * FRAME_MS)
    }

    #[test]
    fn speech_after_a_long_pause_is_recorded_while_the_key_is_held() {
        // Speech, three seconds of thinking, speech again, silence.
        let levels = || {
            quiet(300)
                .chain(loud(900))
                .chain(quiet(3000))
                .chain(loud(900))
                .chain(quiet(5000))
        };
        // Without a hold the first pause ends the phrase: 1200 + 1100 ms.
        let (step, at) = record_ends(levels(), 0);
        assert_eq!(step, VadStep::Done);
        assert!((2280..=2340).contains(&at), "{at}");
        // Held past the second speech: the pause after it ends the phrase, 5100 + 1100 ms.
        let (step, at) = record_ends(levels(), 5400);
        assert_eq!(step, VadStep::Done);
        assert!((6180..=6240).contains(&at), "{at}");
        // Let go in the middle of a pause that is already long: the phrase ends at once.
        let (step, at) = record_ends(levels(), 3000);
        assert_eq!(step, VadStep::Done);
        assert_eq!(at, 3030);
        // Held for ever: the limit ends it.
        let (step, at) = record_ends(loud(900).chain(quiet(60_000)), u32::MAX);
        assert_eq!(step, VadStep::Done);
        assert_eq!(at, MAX_PHRASE_MS);
        // Held and silent from the start: given up as usual.
        let (step, at) = record_ends(quiet(20_000), u32::MAX);
        assert_eq!(step, VadStep::NoSpeech);
        assert!(
            (NO_SPEECH_MS..NO_SPEECH_MS + FRAME_MS).contains(&at),
            "{at}"
        );
    }

    #[test]
    fn wanted_device_is_used_only_while_it_exists() {
        let names = vec!["Микрофон (USB)".to_string(), "Гарнитура".to_string()];
        assert_eq!(pick_device(&names, " Гарнитура "), Some(1));
        assert_eq!(pick_device(&names, "Старый микрофон"), None);
        assert_eq!(pick_device(&names, ""), None);
    }

    #[test]
    fn rms_of_silence_and_full_scale() {
        assert_eq!(rms(&[]), 0.0);
        assert_eq!(rms(&[0.0; 8]), 0.0);
        assert!((rms(&[1.0, -1.0, 1.0, -1.0]) - 1.0).abs() < 1e-6);
    }
}
