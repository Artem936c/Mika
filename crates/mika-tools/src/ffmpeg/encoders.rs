//! Which encoders the found FFmpeg has, and the H.264 one to use.
//!
//! An LGPL build has no libx264: H.264 then comes from the video card
//! (NVENC, Quick Sync, AMF), from Windows (Media Foundation) or from
//! OpenH264. A hardware encoder is listed even without the card, so it is
//! tried on a second of black first.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use mika_core::tools::ToolError;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use super::locate::file_key;
use super::FfmpegPaths;
use crate::sys::Runner;

/// Longest wait for the list and for one trial.
const TRIAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Encoder names with their kind: `V` video, `A` audio, `S` subtitles.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Encoders(BTreeMap<String, char>);

impl Encoders {
    pub(crate) fn has(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }
}

/// `ffmpeg -hide_banner -encoders`: a legend, a ` ------` line, then
/// ` V....D libx264  libx264 H.264 …`.
pub(crate) fn parse_encoders(text: &str) -> Encoders {
    let mut found = BTreeMap::new();
    let mut listing = false;
    for line in text.lines() {
        if !listing {
            listing = line.trim_start().starts_with("------");
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(flags), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let Some(kind) = flags
            .chars()
            .next()
            .filter(|c| matches!(c, 'V' | 'A' | 'S'))
        {
            found.insert(name.to_string(), kind);
        }
    }
    Encoders(found)
}

/// The H.264 encoder to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum H264 {
    X264,
    Nvenc,
    Qsv,
    Amf,
    /// Media Foundation; `hw` when it took `-hw_encoding 1`.
    Mf {
        hw: bool,
    },
    Openh264,
}

fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|x| x.to_string()).collect()
}

impl H264 {
    pub(crate) fn name(self) -> &'static str {
        match self {
            H264::X264 => "libx264",
            H264::Nvenc => "h264_nvenc",
            H264::Qsv => "h264_qsv",
            H264::Amf => "h264_amf",
            H264::Mf { .. } => "h264_mf",
            H264::Openh264 => "libopenh264",
        }
    }

    pub(crate) fn pix_fmt(self) -> &'static str {
        match self {
            H264::Qsv | H264::Mf { .. } => "nv12",
            _ => "yuv420p",
        }
    }

    /// Only libx264 knows two passes.
    pub(crate) fn two_pass(self) -> bool {
        self == H264::X264
    }

    fn head(self) -> Vec<String> {
        let mut v = s(&["-c:v", self.name()]);
        if self == (H264::Mf { hw: true }) {
            v.extend(s(&["-hw_encoding", "1"]));
        }
        v
    }

    fn tail(self, mut v: Vec<String>) -> Vec<String> {
        v.extend(s(&["-pix_fmt", self.pix_fmt()]));
        v
    }

    /// Constant quality: `strong` squeezes harder. Encoders without a quality
    /// mode get a bit rate from the picture size and frame rate.
    pub(crate) fn quality_args(self, strong: bool, w: u32, h: u32, fps: f64) -> Vec<String> {
        let q = if strong { "28" } else { "23" };
        let mut v = self.head();
        match self {
            H264::X264 => v.extend(s(&["-preset", "medium", "-crf", q])),
            H264::Nvenc => v.extend(s(&["-preset", "p5", "-rc", "vbr", "-cq", q, "-b:v", "0"])),
            H264::Qsv => v.extend(s(&["-global_quality", q])),
            H264::Amf => v.extend(s(&["-rc", "cqp", "-qp_i", q, "-qp_p", q])),
            H264::Mf { .. } | H264::Openh264 => {
                let bpp = if strong { 0.05 } else { 0.08 };
                let floor = if strong { 200.0 } else { 300.0 };
                let fps = if fps > 0.0 { fps.min(60.0) } else { 30.0 };
                let kbps = (w.max(2) as f64 * h.max(2) as f64 * fps * bpp / 1000.0).max(floor);
                let kbps = kbps.round() as u64;
                v.extend([
                    "-b:v".into(),
                    format!("{kbps}k"),
                    "-maxrate".into(),
                    format!("{}k", kbps * 3 / 2),
                ]);
            }
        }
        self.tail(v)
    }

    /// Recording on the fly: fast, no B-frames, a key frame every two
    /// seconds, so a piece cut at its end stays whole. Encoders without a
    /// quality mode get a bit rate from the picture size and frame rate.
    pub(crate) fn live_args(self, w: u32, h: u32, fps: u32) -> Vec<String> {
        let gop = (2 * fps.max(1)).to_string();
        let mut v = self.head();
        match self {
            H264::X264 => v.extend(s(&[
                "-preset",
                "veryfast",
                "-tune",
                "zerolatency",
                "-crf",
                "23",
            ])),
            H264::Nvenc => v.extend(s(&[
                "-preset", "p4", "-tune", "ll", "-rc", "vbr", "-cq", "23", "-b:v", "0",
            ])),
            H264::Qsv => v.extend(s(&["-preset", "veryfast", "-global_quality", "23"])),
            H264::Amf => v.extend(s(&[
                "-usage",
                "lowlatency",
                "-rc",
                "cqp",
                "-qp_i",
                "23",
                "-qp_p",
                "25",
            ])),
            H264::Mf { .. } | H264::Openh264 => {
                let kbps = (w.max(2) as f64 * h.max(2) as f64 * fps.max(1) as f64 * 0.07 / 1000.0)
                    .clamp(1500.0, 12000.0)
                    .round() as u64;
                v.extend([
                    "-b:v".into(),
                    format!("{kbps}k"),
                    "-maxrate".into(),
                    format!("{}k", kbps * 3 / 2),
                ]);
            }
        }
        v.extend(["-g".into(), gop]);
        if !matches!(self, H264::Mf { .. } | H264::Openh264) {
            v.extend(s(&["-bf", "0"]));
        }
        self.tail(v)
    }

    /// An average bit rate in kbit/s, for a size to reach.
    pub(crate) fn bitrate_args(self, kbps: u64) -> Vec<String> {
        let b = format!("{kbps}k");
        let max = format!("{}k", kbps * 3 / 2);
        let buf = format!("{}k", kbps * 2);
        let mut v = self.head();
        match self {
            H264::X264 => v.extend(["-preset".into(), "medium".into(), "-b:v".into(), b]),
            H264::Nvenc => v.extend([
                "-preset".into(),
                "p5".into(),
                "-rc".into(),
                "vbr".into(),
                "-multipass".into(),
                "fullres".into(),
                "-b:v".into(),
                b,
                "-maxrate".into(),
                max,
                "-bufsize".into(),
                buf,
            ]),
            H264::Qsv | H264::Mf { .. } | H264::Openh264 => v.extend([
                "-b:v".into(),
                b,
                "-maxrate".into(),
                max,
                "-bufsize".into(),
                buf,
            ]),
            H264::Amf => v.extend([
                "-rc".into(),
                "vbr_peak".into(),
                "-b:v".into(),
                b,
                "-maxrate".into(),
                max,
            ]),
        }
        self.tail(v)
    }
}

/// VP9 and Opus for WebM.
pub(crate) fn webm_args() -> Vec<String> {
    s(&[
        "-c:v",
        "libvpx-vp9",
        "-crf",
        "32",
        "-b:v",
        "0",
        "-row-mt",
        "1",
        "-deadline",
        "good",
        "-cpu-used",
        "4",
        "-c:a",
        "libopus",
        "-b:a",
        "128k",
    ])
}

/// Argument list of a trial: five frames of black into nothing.
pub(crate) fn trial_args(encoder: &str, pix_fmt: &str, hw: bool) -> Vec<String> {
    let mut v = s(&[
        "-hide_banner",
        "-nostdin",
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        "color=black:s=320x240:r=30:d=1",
        "-frames:v",
        "5",
        "-pix_fmt",
        pix_fmt,
        "-c:v",
        encoder,
    ]);
    if hw {
        v.extend(s(&["-hw_encoding", "1"]));
    }
    v.extend(s(&["-f", "null", "-"]));
    v
}

type Key = (String, u64, std::time::SystemTime);

#[derive(Default)]
struct Known {
    list: Option<Encoders>,
    h264: Option<H264>,
}

/// What was learnt of each FFmpeg, by its path, size and time.
static KNOWN: Mutex<Option<HashMap<Key, Known>>> = Mutex::new(None);

fn with_known<T>(key: &Key, f: impl FnOnce(&mut Known) -> T) -> T {
    let mut all = KNOWN.lock();
    f(all
        .get_or_insert_with(HashMap::new)
        .entry(key.clone())
        .or_default())
}

fn key_of(paths: &FfmpegPaths) -> Key {
    file_key(&paths.ffmpeg).unwrap_or_else(|| {
        (
            paths.ffmpeg.to_string_lossy().into_owned(),
            0,
            std::time::SystemTime::UNIX_EPOCH,
        )
    })
}

fn stopped(e: String) -> ToolError {
    if e == "остановлено" {
        ToolError::Cancelled
    } else {
        ToolError::Failed(e)
    }
}

/// The encoders of this FFmpeg.
pub(crate) async fn encoders(
    runner: &dyn Runner,
    paths: &FfmpegPaths,
    cancel: &CancellationToken,
) -> Result<Encoders, ToolError> {
    let key = key_of(paths);
    if let Some(list) = with_known(&key, |k| k.list.clone()) {
        return Ok(list);
    }
    let out = runner
        .run(
            &paths.ffmpeg.to_string_lossy(),
            &s(&["-hide_banner", "-encoders"]),
            TRIAL_TIMEOUT,
            cancel.clone(),
        )
        .await
        .map_err(stopped)?;
    let list = parse_encoders(&out.stdout);
    if list.0.is_empty() {
        return Err(ToolError::Failed(
            "FFmpeg не перечислил свои кодировщики".into(),
        ));
    }
    with_known(&key, |k| k.list = Some(list.clone()));
    Ok(list)
}

/// True when a trial encode with `encoder` ends well.
async fn usable(
    runner: &dyn Runner,
    paths: &FfmpegPaths,
    encoder: &str,
    pix_fmt: &str,
    hw: bool,
    cancel: &CancellationToken,
) -> Result<bool, ToolError> {
    let out = runner
        .run(
            &paths.ffmpeg.to_string_lossy(),
            &trial_args(encoder, pix_fmt, hw),
            TRIAL_TIMEOUT,
            cancel.clone(),
        )
        .await
        .map_err(stopped)?;
    Ok(!out.timed_out && out.code == Some(0))
}

/// The first H.264 encoder that works: libx264, the video cards, Windows,
/// OpenH264.
pub(crate) async fn pick_h264(
    runner: &dyn Runner,
    paths: &FfmpegPaths,
    cancel: &CancellationToken,
) -> Result<H264, ToolError> {
    let key = key_of(paths);
    if let Some(h) = with_known(&key, |k| k.h264) {
        return Ok(h);
    }
    let list = encoders(runner, paths, cancel).await?;
    let mut chosen = None;
    if list.has("libx264") {
        chosen = Some(H264::X264);
    }
    for h in [H264::Nvenc, H264::Qsv, H264::Amf] {
        if chosen.is_none()
            && list.has(h.name())
            && usable(runner, paths, h.name(), h.pix_fmt(), false, cancel).await?
        {
            chosen = Some(h);
        }
    }
    if chosen.is_none() && list.has("h264_mf") {
        for hw in [true, false] {
            if chosen.is_none() && usable(runner, paths, "h264_mf", "nv12", hw, cancel).await? {
                chosen = Some(H264::Mf { hw });
            }
        }
    }
    if chosen.is_none() && list.has("libopenh264") {
        chosen = Some(H264::Openh264);
    }
    let h = chosen
        .ok_or_else(|| ToolError::Failed("в этой сборке FFmpeg нет кодировщика H.264".into()))?;
    with_known(&key, |k| k.h264 = Some(h));
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const LGPL_LIST: &str = "Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 ------
 V....D h264_amf             AMD AMF H.264 Encoder (codec h264)
 V....D h264_mf              H264 via MediaFoundation (codec h264)
 V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)
 V....D h264_qsv             H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (Intel Quick Sync Video acceleration) (codec h264)
 V....D libopenh264          OpenH264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)
 V....D libvpx-vp9           libvpx VP9 (codec vp9)
 A....D aac                  AAC (Advanced Audio Coding)
 A....D libmp3lame           libmp3lame MP3 (MPEG audio layer 3) (codec mp3)
 A....D libopus              libopus Opus (codec opus)
 S..... srt                  SubRip subtitle
";

    #[test]
    fn encoder_list_is_read_after_the_legend() {
        let e = parse_encoders(LGPL_LIST);
        assert!(e.has("h264_mf") && e.has("libopus") && e.has("srt"));
        assert!(!e.has("libx264"));
        assert!(!e.has("V....."), "legend lines are not encoders");
        assert_eq!(e.0.get("aac"), Some(&'A'));
    }

    #[test]
    fn quality_and_bitrate_arguments_per_encoder() {
        assert_eq!(
            H264::X264.quality_args(false, 1280, 720, 30.0).join(" "),
            "-c:v libx264 -preset medium -crf 23 -pix_fmt yuv420p"
        );
        assert_eq!(
            H264::Nvenc.quality_args(true, 1280, 720, 30.0).join(" "),
            "-c:v h264_nvenc -preset p5 -rc vbr -cq 28 -b:v 0 -pix_fmt yuv420p"
        );
        assert_eq!(
            H264::Qsv.quality_args(false, 1280, 720, 30.0).join(" "),
            "-c:v h264_qsv -global_quality 23 -pix_fmt nv12"
        );
        assert_eq!(
            H264::Amf.quality_args(false, 1280, 720, 30.0).join(" "),
            "-c:v h264_amf -rc cqp -qp_i 23 -qp_p 23 -pix_fmt yuv420p"
        );
        // 1280×720×30×0.08 = 2212 kbit/s.
        assert_eq!(
            H264::Mf { hw: true }
                .quality_args(false, 1280, 720, 30.0)
                .join(" "),
            "-c:v h264_mf -hw_encoding 1 -b:v 2212k -maxrate 3318k -pix_fmt nv12"
        );
        assert_eq!(
            H264::Openh264.quality_args(true, 160, 120, 10.0).join(" "),
            "-c:v libopenh264 -b:v 200k -maxrate 300k -pix_fmt yuv420p",
            "a floor for tiny pictures"
        );
        assert_eq!(
            H264::Nvenc.bitrate_args(1000).join(" "),
            "-c:v h264_nvenc -preset p5 -rc vbr -multipass fullres -b:v 1000k -maxrate 1500k -bufsize 2000k -pix_fmt yuv420p"
        );
        assert_eq!(
            H264::Amf.bitrate_args(800).join(" "),
            "-c:v h264_amf -rc vbr_peak -b:v 800k -maxrate 1200k -pix_fmt yuv420p"
        );
        assert!(H264::X264.two_pass() && !H264::Nvenc.two_pass());
    }

    #[test]
    fn live_arguments_per_encoder() {
        assert_eq!(
            H264::X264.live_args(1920, 1080, 30).join(" "),
            "-c:v libx264 -preset veryfast -tune zerolatency -crf 23 -g 60 -bf 0 -pix_fmt yuv420p"
        );
        assert_eq!(
            H264::Nvenc.live_args(1920, 1080, 30).join(" "),
            "-c:v h264_nvenc -preset p4 -tune ll -rc vbr -cq 23 -b:v 0 -g 60 -bf 0 -pix_fmt yuv420p"
        );
        assert_eq!(
            H264::Qsv.live_args(1920, 1080, 30).join(" "),
            "-c:v h264_qsv -preset veryfast -global_quality 23 -g 60 -bf 0 -pix_fmt nv12"
        );
        assert_eq!(
            H264::Amf.live_args(1920, 1080, 30).join(" "),
            "-c:v h264_amf -usage lowlatency -rc cqp -qp_i 23 -qp_p 25 -g 60 -bf 0 -pix_fmt yuv420p"
        );
        // 1920×1080×30×0.07 = 4354,56 kbit/s.
        assert_eq!(
            H264::Mf { hw: true }.live_args(1920, 1080, 30).join(" "),
            "-c:v h264_mf -hw_encoding 1 -b:v 4355k -maxrate 6532k -g 60 -pix_fmt nv12"
        );
        assert_eq!(
            H264::Openh264.live_args(640, 360, 30).join(" "),
            "-c:v libopenh264 -b:v 1500k -maxrate 2250k -g 60 -pix_fmt yuv420p",
            "a floor for a small picture"
        );
        assert_eq!(
            H264::Mf { hw: false }.live_args(3840, 2160, 30).join(" "),
            "-c:v h264_mf -b:v 12000k -maxrate 18000k -g 60 -pix_fmt nv12",
            "a ceiling for 4K"
        );
    }

    #[test]
    fn trial_encodes_five_black_frames() {
        assert_eq!(
            trial_args("h264_mf", "nv12", true).join(" "),
            "-hide_banner -nostdin -v error -f lavfi -i color=black:s=320x240:r=30:d=1 \
-frames:v 5 -pix_fmt nv12 -c:v h264_mf -hw_encoding 1 -f null -"
        );
    }

    fn fake_ffmpeg() -> (tempfile::TempDir, FfmpegPaths) {
        let d = tempfile::tempdir().unwrap();
        let ffmpeg = d.path().join("ffmpeg.exe");
        let ffprobe = d.path().join("ffprobe.exe");
        std::fs::write(&ffmpeg, b"").unwrap();
        std::fs::write(&ffprobe, b"").unwrap();
        (
            d,
            FfmpegPaths {
                ffmpeg,
                ffprobe,
                source: super::super::FfmpegSource::Path,
            },
        )
    }

    fn out(code: i32, stdout: &str) -> crate::sys::RunOutput {
        crate::sys::RunOutput {
            code: Some(code),
            stdout: stdout.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_first_working_encoder_is_chosen_and_remembered() {
        let (env, f) = crate::testing::env();
        let (_d, paths) = fake_ffmpeg();
        // NVENC fails, Quick Sync works.
        f.run_outputs
            .lock()
            .extend([out(0, LGPL_LIST), out(1, ""), out(0, "")]);
        let cancel = CancellationToken::new();
        let h = pick_h264(env.runner.as_ref(), &paths, &cancel)
            .await
            .unwrap();
        assert_eq!(h, H264::Qsv);
        let runs = f.runs.lock().clone();
        assert_eq!(runs.len(), 3);
        assert!(runs[1].1.contains(&"h264_nvenc".to_string()));
        assert!(runs[2].1.contains(&"h264_qsv".to_string()));
        assert_eq!(
            pick_h264(env.runner.as_ref(), &paths, &cancel).await,
            Ok(H264::Qsv)
        );
        assert_eq!(f.runs.lock().len(), 3, "the choice is remembered");
    }

    #[tokio::test]
    async fn media_foundation_is_tried_with_and_without_hardware() {
        let (env, f) = crate::testing::env();
        let (_d, paths) = fake_ffmpeg();
        let list = LGPL_LIST
            .lines()
            .filter(|l| !l.contains("nvenc") && !l.contains("qsv") && !l.contains("amf"))
            .collect::<Vec<_>>()
            .join("\n");
        f.run_outputs
            .lock()
            .extend([out(0, &list), out(1, ""), out(0, "")]);
        let h = pick_h264(env.runner.as_ref(), &paths, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(h, H264::Mf { hw: false });
    }

    #[tokio::test]
    async fn libx264_needs_no_trial_and_nothing_usable_is_an_error() {
        let (env, f) = crate::testing::env();
        let (_d, paths) = fake_ffmpeg();
        f.run_outputs
            .lock()
            .push_back(out(0, " ------\n V....D libx264  x264\n"));
        let cancel = CancellationToken::new();
        assert_eq!(
            pick_h264(env.runner.as_ref(), &paths, &cancel).await,
            Ok(H264::X264)
        );
        assert_eq!(f.runs.lock().len(), 1);

        let (_d2, other) = fake_ffmpeg();
        f.run_outputs
            .lock()
            .extend([out(0, " ------\n V....D h264_nvenc  n\n"), out(1, "")]);
        assert_eq!(
            pick_h264(env.runner.as_ref(), &other, &cancel).await,
            Err(ToolError::Failed(
                "в этой сборке FFmpeg нет кодировщика H.264".into()
            ))
        );
    }
}
