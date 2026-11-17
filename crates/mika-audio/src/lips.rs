//! What the lips of the avatar need from a phrase: how loud it is from one
//! moment to the next (decision 0040). The voice tells the window when a
//! phrase starts to sound and when it falls quiet; the window opens the mouth
//! by this envelope and shapes it by the letters of the phrase.

use std::sync::Arc;

use crate::lead::parse;

/// One step of the envelope, milliseconds.
pub const FRAME_MS: u32 = 20;

/// What the voice tells about the sound it plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LipCue {
    /// A phrase sounds now. `levels` is its loudness every `frame_ms`,
    /// 0..=255 of its loudest moment; empty when the sound could not be read.
    /// `played_ms` is how far the sound already got when this was told: the
    /// lips start there and keep up with the sound.
    Phrase {
        text: String,
        frame_ms: u32,
        levels: Vec<u8>,
        played_ms: u32,
    },
    /// The phrase is over or was cut short.
    Quiet,
}

/// Milliseconds in a span of the player, given in steps of 100 ns; 0 for a
/// span below zero.
pub fn played_ms(ticks: i64) -> u32 {
    u32::try_from(ticks.max(0) / 10_000).unwrap_or(u32::MAX)
}

/// Who hears of the phrases the voice plays.
pub type LipSink = Arc<dyn Fn(LipCue) + Send + Sync>;

/// Loudness of a WAV file every `FRAME_MS`: the root mean square of each step
/// over all channels, scaled so that the loudest step is 255. A quiet file
/// gives zeros. `None` for a file that is not WAV of plain samples.
pub fn envelope(wav: &[u8]) -> Option<Vec<u8>> {
    let w = parse(wav)?;
    let width = usize::from(w.bits / 8);
    let frames_per_step = ((u64::from(w.rate) * u64::from(FRAME_MS)) / 1000).max(1) as usize;
    let data = &wav[w.data.clone()];
    let step_bytes = frames_per_step * w.block;
    let rms: Vec<f64> = data
        .chunks(step_bytes)
        .map(|step| {
            let mut sum = 0.0;
            let mut n = 0usize;
            for sample in step.chunks_exact(width) {
                let v = w.level(sample);
                sum += v * v;
                n += 1;
            }
            if n == 0 {
                0.0
            } else {
                (sum / n as f64).sqrt()
            }
        })
        .collect();
    let peak = rms.iter().cloned().fold(0.0_f64, f64::max);
    // Below −60 dBFS the whole phrase is quiet: no mouth at all.
    if peak < 0.001 {
        return Some(vec![0; rms.len()]);
    }
    Some(
        rms.iter()
            .map(|v| ((v / peak) * 255.0).round().clamp(0.0, 255.0) as u8)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lead::wav;

    fn tone16(ms: u32, rate: u32, amp: f64) -> Vec<u8> {
        let n = (rate * ms / 1000) as usize;
        let mut pcm = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f64 / f64::from(rate);
            let v = (amp * 32767.0 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()) as i16;
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        pcm
    }

    #[test]
    fn played_ticks_become_milliseconds() {
        assert_eq!(played_ms(0), 0);
        assert_eq!(played_ms(1_000_000), 100);
        assert_eq!(played_ms(9_999), 0);
        assert_eq!(played_ms(-5), 0);
        assert_eq!(played_ms(i64::MAX), u32::MAX);
    }

    #[test]
    fn silence_gives_zero_levels() {
        let file = wav(&vec![0u8; 16000 * 2], 16000, 1, 16);
        let levels = envelope(&file).unwrap();
        assert_eq!(levels.len(), 50);
        assert!(levels.iter().all(|&l| l == 0));
    }

    #[test]
    fn tone_gives_peak_255_and_frames_by_20ms() {
        let file = wav(&tone16(1000, 16000, 0.5), 16000, 1, 16);
        let levels = envelope(&file).unwrap();
        assert_eq!(levels.len(), 50);
        assert_eq!(levels.iter().copied().max(), Some(255));
        assert!(levels.iter().all(|&l| l > 240));
    }

    #[test]
    fn half_loud_frame_is_about_half() {
        let mut pcm = tone16(200, 16000, 0.8);
        pcm.extend(tone16(200, 16000, 0.4));
        let levels = envelope(&wav(&pcm, 16000, 1, 16)).unwrap();
        assert_eq!(levels.len(), 20);
        let late = levels[15];
        assert!((115..=140).contains(&late), "{late}");
    }

    #[test]
    fn stereo_and_8bit_are_read() {
        let mono = tone16(400, 8000, 0.5);
        let stereo: Vec<u8> = mono
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|s| [s[0], s[1], s[0], s[1]])
            .collect();
        let levels = envelope(&wav(&stereo, 8000, 2, 16)).unwrap();
        assert_eq!(levels.len(), 20);
        assert!(levels.iter().all(|&l| l > 240));

        let eight: Vec<u8> = mono
            .as_chunks::<2>()
            .0
            .iter()
            .map(|s| ((i16::from_le_bytes(*s) >> 8) as i32 + 128) as u8)
            .collect();
        let levels = envelope(&wav(&eight, 8000, 1, 8)).unwrap();
        assert_eq!(levels.len(), 20);
        assert!(levels.iter().all(|&l| l > 230));
    }

    #[test]
    fn not_wav_gives_none() {
        assert_eq!(envelope(b"ID3 not a wav at all"), None);
        assert_eq!(envelope(&[]), None);
    }

    #[test]
    fn empty_data_gives_empty_levels() {
        assert_eq!(envelope(&wav(&[], 16000, 1, 16)), Some(vec![]));
    }
}
