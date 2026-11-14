//! Sound files as bytes: what kind a file is, and who can turn one kind into another.

use std::time::Duration;

use async_trait::async_trait;

/// The kind of a sound file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundKind {
    /// Ogg with the Opus codec: what a Telegram voice message is.
    OggOpus,
    /// Ogg with another codec, such as Vorbis.
    Ogg,
    Mp3,
    M4a,
    Wav,
    Flac,
    Unknown,
}

/// Bytes at the start of an Ogg file where the codec is named.
const OGG_HEAD: usize = 96;

impl SoundKind {
    /// The kind by the first bytes; the content type of an answer cannot be trusted.
    pub fn of(bytes: &[u8]) -> SoundKind {
        if bytes.starts_with(b"OggS") {
            let head = &bytes[..bytes.len().min(OGG_HEAD)];
            return if head.windows(8).any(|w| w == b"OpusHead") {
                SoundKind::OggOpus
            } else {
                SoundKind::Ogg
            };
        }
        if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE".as_slice()) {
            return SoundKind::Wav;
        }
        if bytes.starts_with(b"fLaC") {
            return SoundKind::Flac;
        }
        if bytes.starts_with(b"ID3") {
            return SoundKind::Mp3;
        }
        if bytes.get(4..8) == Some(b"ftyp".as_slice()) {
            return SoundKind::M4a;
        }
        if mp3_frame(bytes) {
            return SoundKind::Mp3;
        }
        SoundKind::Unknown
    }

    /// The usual extension of such a file.
    pub fn extension(self) -> &'static str {
        match self {
            SoundKind::OggOpus | SoundKind::Ogg => "ogg",
            SoundKind::Mp3 => "mp3",
            SoundKind::M4a => "m4a",
            SoundKind::Wav => "wav",
            SoundKind::Flac => "flac",
            SoundKind::Unknown => "bin",
        }
    }

    /// The content type of such a file.
    pub fn mime(self) -> &'static str {
        match self {
            SoundKind::OggOpus | SoundKind::Ogg => "audio/ogg",
            SoundKind::Mp3 => "audio/mpeg",
            SoundKind::M4a => "audio/mp4",
            SoundKind::Wav => "audio/wav",
            SoundKind::Flac => "audio/flac",
            SoundKind::Unknown => "application/octet-stream",
        }
    }

    /// Telegram shows such a file as a voice message: OGG/Opus, MP3, M4A.
    pub fn voice_message(self) -> bool {
        matches!(self, SoundKind::OggOpus | SoundKind::Mp3 | SoundKind::M4a)
    }
}

/// The header of a real MPEG Layer III frame: the sync bits, layer III, a
/// bitrate and a sample rate that exist. ADTS AAC (layer 00) and raw samples
/// that start with `FF FF` (a sample of -1 at a quiet start) are not one.
fn mp3_frame(b: &[u8]) -> bool {
    b.len() >= 3
        && b[0] == 0xFF
        && b[1] & 0xE0 == 0xE0
        && b[1] & 0x06 == 0x02
        && (b[2] >> 4) != 0
        && (b[2] >> 4) != 0xF
        && (b[2] >> 2) & 3 != 3
}

/// Samples a second in the positions of an Ogg/Opus stream, whatever rate
/// the sound was recorded at.
const OPUS_RATE: u64 = 48_000;
/// Bytes of an Ogg page before the sizes of its segments.
const OGG_PAGE_HEAD: usize = 27;

/// How long an Ogg/Opus sound really plays, read from the positions its
/// pages carry: no decoder is needed. The length a sender names for a voice
/// message is only a claim. `None` for any other kind of sound.
pub fn ogg_opus_length(bytes: &[u8]) -> Option<Duration> {
    if SoundKind::of(bytes) != SoundKind::OggOpus {
        return None;
    }
    let mut at = 0;
    let mut last = 0u64;
    let mut skip = None;
    // A page: `OggS`, version, flags, position (8 bytes), stream, number,
    // checksum, the count of segments, their sizes, the data.
    while let Some(head) = bytes.get(at..at + OGG_PAGE_HEAD) {
        if !head.starts_with(b"OggS") {
            break;
        }
        let data = at + OGG_PAGE_HEAD + head[26] as usize;
        let Some(sizes) = bytes.get(at + OGG_PAGE_HEAD..data) else {
            break;
        };
        if skip.is_none() {
            // The first page holds `OpusHead`; the samples to drop at the
            // start are named at its bytes 10..12.
            let head = bytes.get(data..data + 12);
            skip = Some(head.map_or(0, |h| u64::from(u16::from_le_bytes([h[10], h[11]]))));
        }
        let mut position = [0u8; 8];
        position.copy_from_slice(&head[6..14]);
        let position = u64::from_le_bytes(position);
        // All ones: no packet ends on this page.
        if position != u64::MAX {
            last = last.max(position);
        }
        at = data + sizes.iter().map(|s| usize::from(*s)).sum::<usize>();
    }
    let samples = last.saturating_sub(skip.unwrap_or(0));
    Some(Duration::from_millis(
        samples.saturating_mul(1000) / OPUS_RATE,
    ))
}

/// Turns one kind of sound into another, such as FFmpeg.
#[async_trait]
pub trait SoundConverter: Send + Sync {
    /// A converter is there now.
    fn ready(&self) -> bool;
    /// Any known sound as WAV, 16 kHz, mono, 16 bit: what the microphone sends to recognition.
    async fn to_wav16k(&self, sound: &[u8]) -> Result<Vec<u8>, String>;
    /// Any known sound as OGG/Opus, mono: a voice message.
    async fn to_ogg_opus(&self, sound: &[u8]) -> Result<Vec<u8>, String>;
}

/// No converter: never ready, both conversions fail with "нет FFmpeg".
pub struct NoConverter;

#[async_trait]
impl SoundConverter for NoConverter {
    fn ready(&self) -> bool {
        false
    }

    async fn to_wav16k(&self, _sound: &[u8]) -> Result<Vec<u8>, String> {
        Err("нет FFmpeg".into())
    }

    async fn to_ogg_opus(&self, _sound: &[u8]) -> Result<Vec<u8>, String> {
        Err("нет FFmpeg".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first page of an Ogg file whose codec header starts with `codec`.
    fn ogg(codec: &[u8]) -> Vec<u8> {
        let mut page = b"OggS\0\x02".to_vec();
        page.extend_from_slice(&[0; 20]);
        page.extend_from_slice(&[1, codec.len() as u8]);
        page.extend_from_slice(codec);
        page.extend_from_slice(&[0; 64]);
        page
    }

    #[test]
    fn the_kind_is_told_by_the_first_bytes() {
        assert_eq!(SoundKind::of(&ogg(b"OpusHead\x01\x01")), SoundKind::OggOpus);
        assert_eq!(SoundKind::of(&ogg(b"\x01vorbis\0\0")), SoundKind::Ogg);
        assert_eq!(SoundKind::of(b"OggS"), SoundKind::Ogg);
        assert_eq!(SoundKind::of(b"RIFF\x24\0\0\0WAVEfmt "), SoundKind::Wav);
        assert_eq!(SoundKind::of(b"RIFF\x24\0\0\0AVI LIST"), SoundKind::Unknown);
        assert_eq!(SoundKind::of(b"RIFF\x24\0"), SoundKind::Unknown);
        assert_eq!(SoundKind::of(b"fLaC\0\0\0\x22"), SoundKind::Flac);
        assert_eq!(SoundKind::of(b"ID3\x04\0\0\0\0"), SoundKind::Mp3);
        assert_eq!(SoundKind::of(&[0xFF, 0xFB, 0x90, 0x00]), SoundKind::Mp3);
        assert_eq!(SoundKind::of(&[0xFF, 0xF3, 0x48, 0xC4]), SoundKind::Mp3);
        assert_eq!(
            SoundKind::of(&[0x00, 0x00, 0x00, 0x20, 0x66, 0x74, 0x79, 0x70]),
            SoundKind::M4a
        );
    }

    #[test]
    fn aac_frames_and_raw_samples_are_not_taken_for_mp3() {
        // ADTS AAC: the same sync bits, layer 00.
        assert_eq!(SoundKind::of(&[0xFF, 0xF1, 0x50, 0x80]), SoundKind::Unknown);
        // Raw 16-bit samples that start with -1, as a quiet start does.
        assert_eq!(SoundKind::of(&[0xFF, 0xFF, 0x01, 0x00]), SoundKind::Unknown);
        // A free bitrate, a bad bitrate and a reserved sample rate.
        assert_eq!(SoundKind::of(&[0xFF, 0xFB, 0x00, 0x00]), SoundKind::Unknown);
        assert_eq!(SoundKind::of(&[0xFF, 0xFB, 0xF0, 0x00]), SoundKind::Unknown);
        assert_eq!(SoundKind::of(&[0xFF, 0xFB, 0x9C, 0x00]), SoundKind::Unknown);
        assert_eq!(SoundKind::of(&[0x12, 0x00, 0x34, 0x00]), SoundKind::Unknown);
    }

    #[test]
    fn short_input_is_unknown_without_a_panic() {
        assert_eq!(SoundKind::of(b""), SoundKind::Unknown);
        assert_eq!(SoundKind::of(&[0xFF]), SoundKind::Unknown);
        assert_eq!(SoundKind::of(&[0xFF, 0xFB]), SoundKind::Unknown);
        assert_eq!(SoundKind::of(b"RI"), SoundKind::Unknown);
        assert_eq!(SoundKind::of(b"\0\0\0\x20ft"), SoundKind::Unknown);
    }

    #[test]
    fn each_kind_has_its_extension_and_content_type() {
        let all = [
            (SoundKind::OggOpus, "ogg", "audio/ogg", true),
            (SoundKind::Ogg, "ogg", "audio/ogg", false),
            (SoundKind::Mp3, "mp3", "audio/mpeg", true),
            (SoundKind::M4a, "m4a", "audio/mp4", true),
            (SoundKind::Wav, "wav", "audio/wav", false),
            (SoundKind::Flac, "flac", "audio/flac", false),
            (SoundKind::Unknown, "bin", "application/octet-stream", false),
        ];
        for (kind, ext, mime, voice) in all {
            assert_eq!(kind.extension(), ext, "{kind:?}");
            assert_eq!(kind.mime(), mime, "{kind:?}");
            assert_eq!(kind.voice_message(), voice, "{kind:?}");
        }
    }

    /// One Ogg page that ends at `position` and carries `data`.
    fn page(position: u64, data: &[u8]) -> Vec<u8> {
        let mut page = b"OggS\0\0".to_vec();
        page.extend_from_slice(&position.to_le_bytes());
        page.extend_from_slice(&[0; 12]);
        let sizes: Vec<u8> = data.chunks(255).map(|c| c.len() as u8).collect();
        page.push(sizes.len() as u8);
        page.extend_from_slice(&sizes);
        page.extend_from_slice(data);
        page
    }

    /// An Ogg/Opus stream of `seconds` with 312 samples to drop at the start.
    fn opus(seconds: u64) -> Vec<u8> {
        let mut head = b"OpusHead\x01\x01".to_vec();
        head.extend_from_slice(&312u16.to_le_bytes());
        head.extend_from_slice(&[0x80, 0xBB, 0, 0, 0, 0, 0]);
        let mut file = page(0, &head);
        file.extend(page(0, b"OpusTags\0\0\0\0\0\0\0\0"));
        file.extend(page(48_000 * seconds / 2, &[7; 600]));
        file.extend(page(48_000 * seconds + 312, &[7; 300]));
        file
    }

    #[test]
    fn the_length_of_a_voice_message_is_read_from_its_pages() {
        assert_eq!(ogg_opus_length(&opus(3)), Some(Duration::from_secs(3)));
        assert_eq!(
            ogg_opus_length(&opus(7200)),
            Some(Duration::from_secs(7200))
        );
        // A page on which no packet ends carries no position.
        let mut open = opus(5);
        open.extend(page(u64::MAX, &[7; 40]));
        assert_eq!(ogg_opus_length(&open), Some(Duration::from_secs(5)));
        // Headers alone play for no time.
        let mut head = b"OpusHead\x01\x01".to_vec();
        head.extend_from_slice(&[0; 9]);
        assert_eq!(ogg_opus_length(&page(0, &head)), Some(Duration::ZERO));
        // A position that no sound can have is a very long sound, not a panic.
        let mut forged = opus(1);
        forged.extend(page(u64::MAX - 1, &[7; 4]));
        assert!(ogg_opus_length(&forged).unwrap() > Duration::from_secs(1_000_000));
    }

    #[test]
    fn the_length_of_a_cut_or_foreign_file_is_read_without_a_panic() {
        let whole = opus(4);
        for cut in 0..whole.len() {
            let _ = ogg_opus_length(&whole[..cut]);
        }
        // The last page lost its end: its position still counts.
        assert_eq!(
            ogg_opus_length(&whole[..whole.len() - 100]),
            Some(Duration::from_secs(4))
        );
        // Bytes after the pages that are no page end the reading.
        let mut tail = opus(2);
        tail.extend_from_slice(b"not a page at all, but long enough to be one");
        assert_eq!(ogg_opus_length(&tail), Some(Duration::from_secs(2)));
        // Only Ogg with Opus counts its positions at 48 kHz.
        assert_eq!(ogg_opus_length(&ogg(b"\x01vorbis\0\0")), None);
        assert_eq!(ogg_opus_length(b"RIFF\x24\0\0\0WAVEfmt "), None);
        assert_eq!(ogg_opus_length(b""), None);
        assert_eq!(ogg_opus_length(b"OggS-voice-message"), None);
    }

    #[tokio::test]
    async fn no_converter_is_never_ready_and_says_why() {
        let none = NoConverter;
        assert!(!none.ready());
        assert_eq!(none.to_wav16k(b"OggS").await, Err("нет FFmpeg".into()));
        assert_eq!(none.to_ogg_opus(b"RIFF").await, Err("нет FFmpeg".into()));
    }
}
