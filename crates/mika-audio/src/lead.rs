//! Quiet at the start of a phrase. Windows starts each new sound a moment
//! late and the first milliseconds of it are lost. A voice that begins right
//! with its first word loses part of that word, so every phrase starts with
//! a little quiet. A sound that already has it gets nothing more.

use std::ops::Range;

/// Quiet a phrase starts with, at least, milliseconds.
pub const LEAD_IN_MS: u32 = 150;

/// A sample below this share of full scale counts as quiet: −40 dBFS.
const QUIET: f64 = 0.01;

/// What a WAV file holds, as far as the quiet at its start goes.
#[derive(Debug, PartialEq)]
pub(crate) struct Wav {
    pub(crate) rate: u32,
    pub(crate) bits: u16,
    float: bool,
    /// Bytes of one frame: one sample of every channel.
    pub(crate) block: usize,
    /// Where the size of the data chunk is written.
    size_at: usize,
    /// The samples, within the file as it came.
    pub(crate) data: Range<usize>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The samples of a WAV file of plain PCM or 32-bit float. `None` for any
/// other file.
pub(crate) fn parse(b: &[u8]) -> Option<Wav> {
    if b.get(..4)? != b"RIFF" || b.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut format: Option<(usize, u32, u16, bool)> = None;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let size = u32_at(b, at + 4)? as usize;
        let body = at + 8;
        if id == b"fmt " {
            let mut tag = u16_at(b, body)?;
            let channels = u16_at(b, body + 2)? as usize;
            let rate = u32_at(b, body + 4)?;
            let bits = u16_at(b, body + 14)?;
            if tag == 0xFFFE {
                // WAVE_FORMAT_EXTENSIBLE: the real tag opens the subformat.
                tag = u16_at(b, body + 24)?;
            }
            let float = match (tag, bits) {
                (1, 8 | 16 | 24 | 32) => false,
                (3, 32) => true,
                _ => return None,
            };
            if channels == 0 || rate == 0 {
                return None;
            }
            format = Some((channels, rate, bits, float));
        } else if id == b"data" {
            let (channels, rate, bits, float) = format?;
            let block = channels * usize::from(bits / 8);
            // Sound sent as a stream may name a size it does not have.
            let len = size.min(b.len() - body);
            let len = len - len % block;
            return Some(Wav {
                rate,
                bits,
                float,
                block,
                size_at: at + 4,
                data: body..body + len,
            });
        }
        at = body.checked_add(size)?.checked_add(size % 2)?;
    }
    None
}

impl Wav {
    /// Loudness of one sample, a share of full scale.
    pub(crate) fn level(&self, s: &[u8]) -> f64 {
        match (self.bits, self.float) {
            (8, _) => (f64::from(s[0]) - 128.0).abs() / 128.0,
            (16, _) => f64::from(i16::from_le_bytes([s[0], s[1]])).abs() / 32768.0,
            (24, _) => {
                let v = i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8;
                f64::from(v).abs() / 8_388_608.0
            }
            (_, true) => f64::from(f32::from_le_bytes([s[0], s[1], s[2], s[3]])).abs(),
            _ => f64::from(i32::from_le_bytes([s[0], s[1], s[2], s[3]])).abs() / 2_147_483_648.0,
        }
    }

    /// Frames of quiet before the first sound.
    fn quiet_frames(&self, b: &[u8]) -> usize {
        let width = usize::from(self.bits / 8);
        b[self.data.clone()]
            .chunks_exact(self.block)
            .take_while(|frame| frame.chunks_exact(width).all(|s| self.level(s) < QUIET))
            .count()
    }
}

/// `wav` with quiet added before its first sound, so that it starts with at
/// least `lead_ms` of it. `None` when it already does or is not a WAV file of
/// plain samples.
pub fn with_lead_in(wav: &[u8], lead_ms: u32) -> Option<Vec<u8>> {
    let w = parse(wav)?;
    let want = (u64::from(w.rate) * u64::from(lead_ms) / 1000) as usize;
    let missing = want.saturating_sub(w.quiet_frames(wav));
    if missing == 0 {
        return None;
    }
    let silence = if w.bits == 8 { 0x80 } else { 0 };
    let added = missing * w.block;
    let mut out = Vec::with_capacity(wav.len() + added);
    out.extend_from_slice(&wav[..w.data.start]);
    out.resize(out.len() + added, silence);
    out.extend_from_slice(&wav[w.data.clone()]);
    let data_len = u32::try_from(w.data.len() + added).ok()?;
    out[w.size_at..w.size_at + 4].copy_from_slice(&data_len.to_le_bytes());
    let riff_len = u32::try_from(out.len() - 8).ok()?;
    out[4..8].copy_from_slice(&riff_len.to_le_bytes());
    Some(out)
}

/// A WAV file of plain integer samples.
pub fn wav(pcm: &[u8], rate: u32, channels: u16, bits: u16) -> Vec<u8> {
    let block = channels.saturating_mul(bits / 8);
    let data_len = u32::try_from(pcm.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&data_len.saturating_add(36).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&rate.saturating_mul(u32::from(block)).to_le_bytes());
    out.extend_from_slice(&block.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16-bit mono samples at 24 kHz: `quiet` frames of zero, then `loud`
    /// frames of a loud tone.
    fn speech(quiet: usize, loud: usize) -> Vec<u8> {
        let mut pcm = vec![0u8; quiet * 2];
        for i in 0..loud {
            let v: i16 = if i % 2 == 0 { 12_000 } else { -12_000 };
            pcm.extend_from_slice(&v.to_le_bytes());
        }
        wav(&pcm, 24_000, 1, 16)
    }

    fn data_of(w: &[u8]) -> &[u8] {
        let p = parse(w).unwrap();
        &w[p.data]
    }

    #[test]
    fn a_sound_that_starts_with_its_word_gets_quiet_before_it() {
        let src = speech(0, 2400);
        let out = with_lead_in(&src, 150).unwrap();
        let p = parse(&out).unwrap();
        // 150 ms at 24 kHz, two bytes a frame.
        assert_eq!(p.data.len(), src.len() - 44 + 3600 * 2);
        assert_eq!(p.quiet_frames(&out), 3600);
        assert_eq!(&data_of(&out)[7200..], data_of(&src));
        assert_eq!(u32_at(&out, 4).unwrap() as usize, out.len() - 8);
    }

    #[test]
    fn quiet_already_there_counts() {
        assert_eq!(with_lead_in(&speech(4800, 100), 150), None);
        let out = with_lead_in(&speech(1200, 100), 150).unwrap();
        assert_eq!(parse(&out).unwrap().quiet_frames(&out), 3600);
        // A whisper of noise is quiet too.
        let mut pcm = Vec::new();
        for _ in 0..3600 {
            pcm.extend_from_slice(&100i16.to_le_bytes());
        }
        pcm.extend_from_slice(&20_000i16.to_le_bytes());
        assert_eq!(with_lead_in(&wav(&pcm, 24_000, 1, 16), 150), None);
    }

    #[test]
    fn a_stream_with_a_size_it_does_not_have_is_mended() {
        let mut src = speech(0, 100);
        src[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        src[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let out = with_lead_in(&src, 150).unwrap();
        assert_eq!(u32_at(&out, 40).unwrap() as usize, out.len() - 44);
        assert_eq!(u32_at(&out, 4).unwrap() as usize, out.len() - 8);
    }

    #[test]
    fn other_chunks_and_formats_are_handled() {
        // A LIST chunk of odd size before the data, stereo float.
        let mut f = Vec::new();
        f.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        f.extend_from_slice(&16u32.to_le_bytes());
        f.extend_from_slice(&3u16.to_le_bytes());
        f.extend_from_slice(&2u16.to_le_bytes());
        f.extend_from_slice(&1000u32.to_le_bytes());
        f.extend_from_slice(&8000u32.to_le_bytes());
        f.extend_from_slice(&8u16.to_le_bytes());
        f.extend_from_slice(&32u16.to_le_bytes());
        f.extend_from_slice(b"LIST");
        f.extend_from_slice(&3u32.to_le_bytes());
        f.extend_from_slice(b"abc\0");
        f.extend_from_slice(b"data");
        f.extend_from_slice(&8u32.to_le_bytes());
        f.extend_from_slice(&0.5f32.to_le_bytes());
        f.extend_from_slice(&0.0f32.to_le_bytes());
        let out = with_lead_in(&f, 150).unwrap();
        let p = parse(&out).unwrap();
        assert_eq!((p.block, p.rate, p.float), (8, 1000, true));
        assert_eq!(p.data.len(), 8 + 150 * 8);
        // 8-bit sound is quiet at 128.
        let out = with_lead_in(&wav(&[200, 128], 1000, 1, 8), 10).unwrap();
        assert_eq!(
            data_of(&out),
            [128, 128, 128, 128, 128, 128, 128, 128, 128, 128, 200, 128]
        );
        // Not a WAV file, or samples that are not plain.
        assert_eq!(with_lead_in(b"ID3\x04 mp3", 150), None);
        let mut adpcm = speech(0, 10);
        adpcm[20..22].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(with_lead_in(&adpcm, 150), None);
    }
}
