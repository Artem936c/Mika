//! The sound FFmpeg extracts for recognition, and what its filters print.

use serde_json::Value;

/// Samples per second of the extracted sound: 16-bit mono.
pub(crate) const RATE: u32 = 16_000;

/// The samples of a WAV file: the bytes of its `data` chunk. A size that
/// runs past the end (a file written without seeking) means "to the end".
pub(crate) fn pcm_of(wav: &[u8]) -> Result<&[u8], String> {
    if wav.len() < 12 || &wav[..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err("звук не извлёкся: это не WAV".into());
    }
    let mut at = 12;
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let size =
            u32::from_le_bytes([wav[at + 4], wav[at + 5], wav[at + 6], wav[at + 7]]) as usize;
        let body = at + 8;
        if id == b"data" {
            let end = body.saturating_add(size).min(wav.len());
            return Ok(&wav[body..end]);
        }
        at = body.saturating_add(size).saturating_add(size % 2);
    }
    Err("звук не извлёкся: в WAV нет данных".into())
}

/// Seconds of 16-bit mono samples.
pub(crate) fn pcm_seconds(pcm: &[u8]) -> f64 {
    (pcm.len() / 2) as f64 / RATE as f64
}

/// A new WAV file with the samples from `start` to `end` seconds.
pub(crate) fn slice_wav(pcm: &[u8], start: f64, end: f64) -> Vec<u8> {
    let at = |t: f64| ((t.max(0.0) * RATE as f64).round() as usize * 2).min(pcm.len() & !1);
    let (a, b) = (at(start), at(end));
    let part = &pcm[a..b.max(a)];
    let data_len = part.len() as u32;
    let mut out = Vec::with_capacity(44 + part.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(part);
    out
}

fn number_after(line: &str, key: &str) -> Option<f64> {
    let rest = &line[line.find(key)? + key.len()..];
    let token = rest.split_whitespace().next()?;
    token
        .trim_end_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .ok()
}

/// Silences found by `silencedetect`: start and, unless it lasts to the
/// end, its end.
pub(crate) fn parse_silences(stderr: &str) -> Vec<(f64, Option<f64>)> {
    let mut out: Vec<(f64, Option<f64>)> = Vec::new();
    for line in stderr.lines() {
        if let Some(start) = number_after(line, "silence_start:") {
            out.push((start.max(0.0), None));
        } else if let Some(end) = number_after(line, "silence_end:") {
            match out.last_mut() {
                Some(last) if last.1.is_none() => last.1 = Some(end),
                _ => out.push((0.0, Some(end))),
            }
        }
    }
    out
}

/// What the first `loudnorm` pass measured.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Loudness {
    pub input_i: String,
    pub input_tp: String,
    pub input_lra: String,
    pub input_thresh: String,
    pub target_offset: String,
}

/// The last `{…}` block `loudnorm` printed as JSON.
pub(crate) fn parse_loudnorm(stderr: &str) -> Option<Loudness> {
    let end = stderr.rfind('}')?;
    let start = stderr[..end].rfind('{')?;
    let v: Value = serde_json::from_str(&stderr[start..=end]).ok()?;
    let field = |k: &str| -> Option<String> {
        let s = match &v[k] {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        };
        // A value FFmpeg could not measure, as for silence.
        s.parse::<f64>().ok().filter(|n| n.is_finite())?;
        Some(s)
    };
    Some(Loudness {
        input_i: field("input_i")?,
        input_tp: field("input_tp")?,
        input_lra: field("input_lra")?,
        input_thresh: field("input_thresh")?,
        target_offset: field("target_offset")?,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A WAV of `seconds` of silence as FFmpeg writes it: with a LIST chunk.
    pub(crate) fn wav_of(seconds: f64) -> Vec<u8> {
        let pcm = vec![0u8; (seconds * RATE as f64) as usize * 2];
        let mut wav = slice_wav(&pcm, 0.0, seconds);
        // Put a LIST chunk before data, as FFmpeg does.
        let list = b"LIST\x04\0\0\0INFO";
        wav.splice(36..36, list.iter().copied());
        wav
    }

    #[test]
    fn data_is_found_after_other_chunks_and_sliced() {
        let wav = wav_of(2.0);
        let pcm = pcm_of(&wav).unwrap();
        assert_eq!(pcm.len(), 64_000);
        assert_eq!(pcm_seconds(pcm), 2.0);
        let part = slice_wav(pcm, 0.5, 1.0);
        assert_eq!(part.len(), 44 + 16_000);
        assert_eq!(&part[36..40], b"data");
        assert_eq!(u32::from_le_bytes(part[40..44].try_into().unwrap()), 16_000);
        assert_eq!(
            slice_wav(pcm, 1.5, 9.0).len(),
            44 + 16_000,
            "cut at the end"
        );
        assert!(pcm_of(b"RIFF\0\0\0\0AVI ").is_err());
    }

    #[test]
    fn data_size_past_the_end_means_to_the_end() {
        let mut wav = slice_wav(&[1, 0, 2, 0], 0.0, 1.0);
        wav[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(pcm_of(&wav).unwrap(), &[1, 0, 2, 0]);
    }

    #[test]
    fn silences_pair_starts_with_ends() {
        let log = "[silencedetect @ 000001] silence_start: 0\n\
[silencedetect @ 000001] silence_end: 1.25 | silence_duration: 1.25\n\
size=N/A time=00:00:05.00 bitrate=N/A speed= 100x\n\
[silencedetect @ 000001] silence_start: 4.5\n\
[silencedetect @ 000001] silence_end: 5.1 | silence_duration: 0.6\n\
[silencedetect @ 000001] silence_start: 9.75\n";
        assert_eq!(
            parse_silences(log),
            vec![(0.0, Some(1.25)), (4.5, Some(5.1)), (9.75, None)]
        );
        assert_eq!(parse_silences("nothing"), vec![]);
    }

    #[test]
    fn loudnorm_measurement_is_the_last_json_block() {
        let log = "[Parsed_loudnorm_0 @ 0000] \n{\n\t\"input_i\" : \"-27.61\",\n\t\"input_tp\" : \"-4.47\",\n\
\t\"input_lra\" : \"18.06\",\n\t\"input_thresh\" : \"-39.20\",\n\t\"output_i\" : \"-16.58\",\n\
\t\"target_offset\" : \"0.58\"\n}\n";
        assert_eq!(
            parse_loudnorm(log),
            Some(Loudness {
                input_i: "-27.61".into(),
                input_tp: "-4.47".into(),
                input_lra: "18.06".into(),
                input_thresh: "-39.20".into(),
                target_offset: "0.58".into(),
            })
        );
        let silent = log.replace("-27.61", "-inf");
        assert_eq!(parse_loudnorm(&silent), None);
        assert_eq!(parse_loudnorm("no json"), None);
    }
}
