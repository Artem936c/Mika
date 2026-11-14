//! What is inside a media file, by ffprobe.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use mika_core::tools::ToolError;
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::args::DEMUXERS;
use super::locate::file_key;
use super::FfmpegPaths;
use crate::sys::{RunOutput, Runner};

/// Longest wait for ffprobe.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Containers that only point at other files or addresses.
const PLAYLISTS: &[&str] = &[
    "hls",
    "applehttp",
    "dash",
    "concat",
    "ffconcat",
    "sdp",
    "tee",
];

#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    /// ffprobe's names of the container: `mov,mp4,m4a,3gp,3g2,mj2`.
    pub format_name: String,
    pub duration_s: Option<f64>,
    pub size: u64,
    /// Bits per second of the whole file.
    pub bit_rate: Option<u64>,
    pub video: Option<VideoStream>,
    pub audio: Option<AudioStream>,
    pub subtitle_count: usize,
}

impl MediaInfo {
    pub fn duration(&self) -> f64 {
        self.duration_s.unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoStream {
    pub codec: String,
    /// Size as it is shown, the rotation applied.
    pub width: u32,
    pub height: u32,
    pub fps: Option<f64>,
    /// Degrees from the file, 0..360.
    pub rotation: i32,
    pub pix_fmt: String,
    /// Encoder settings that must match for a join without encoding.
    pub profile: String,
    pub level: Option<i64>,
    pub has_b_frames: Option<u32>,
    pub codec_tag: String,
    pub extradata_size: Option<u64>,
}

impl VideoStream {
    /// The shorter side as shown: 720 for 1280×720 and for 720×1280.
    pub fn short_side(&self) -> u32 {
        self.width.min(self.height)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioStream {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub bit_rate: Option<u64>,
    /// `LC`, `HE-AAC`: must match for a join without encoding.
    pub profile: String,
    pub sample_fmt: String,
}

/// A number that ffprobe may write as a string.
fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n: &f64| n.is_finite())
}

/// `30000/1001` or `25/1`; `0/0` is none.
fn rate(v: &Value) -> Option<f64> {
    let s = v.as_str()?;
    let (a, b) = s.split_once('/').unwrap_or((s, "1"));
    let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
    (a > 0.0 && b > 0.0).then_some(a / b)
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// ffprobe's `-print_format json -show_format -show_streams` answer.
pub fn parse_probe(json: &str) -> Result<MediaInfo, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "FFmpeg не прочитал файл".to_string())?;
    let format = &v["format"];
    let format_name = text(&format["format_name"]);
    if format_name
        .split(',')
        .any(|f| PLAYLISTS.contains(&f.trim()))
    {
        return Err("это плейлист, а не медиафайл".into());
    }
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    if format_name.is_empty() && streams.is_empty() {
        return Err("FFmpeg не прочитал файл".into());
    }
    let kind = |s: &Value, k: &str| s["codec_type"].as_str() == Some(k);
    let video = streams
        .iter()
        .find(|s| kind(s, "video") && number(&s["disposition"]["attached_pic"]) != Some(1.0))
        .map(video_stream);
    let audio = streams
        .iter()
        .find(|s| kind(s, "audio"))
        .map(|s| AudioStream {
            codec: text(&s["codec_name"]),
            sample_rate: number(&s["sample_rate"]).unwrap_or(0.0) as u32,
            channels: number(&s["channels"]).unwrap_or(0.0) as u32,
            bit_rate: number(&s["bit_rate"]).map(|b| b as u64),
            profile: text(&s["profile"]),
            sample_fmt: text(&s["sample_fmt"]),
        });
    let subtitle_count = streams.iter().filter(|s| kind(s, "subtitle")).count();
    let duration_s = number(&format["duration"])
        .or_else(|| {
            streams
                .iter()
                .filter_map(|s| number(&s["duration"]))
                .reduce(f64::max)
        })
        .filter(|d| *d > 0.0);
    Ok(MediaInfo {
        format_name,
        duration_s,
        size: number(&format["size"]).unwrap_or(0.0) as u64,
        bit_rate: number(&format["bit_rate"]).map(|b| b as u64),
        video,
        audio,
        subtitle_count,
    })
}

fn video_stream(s: &Value) -> VideoStream {
    let rotation = s["side_data_list"]
        .as_array()
        .and_then(|list| list.iter().find_map(|d| number(&d["rotation"])))
        .or_else(|| number(&s["tags"]["rotate"]))
        .unwrap_or(0.0)
        .round() as i32;
    let rotation = rotation.rem_euclid(360);
    let (w, h) = (
        number(&s["width"]).unwrap_or(0.0) as u32,
        number(&s["height"]).unwrap_or(0.0) as u32,
    );
    let (width, height) = if rotation == 90 || rotation == 270 {
        (h, w)
    } else {
        (w, h)
    };
    VideoStream {
        codec: text(&s["codec_name"]),
        width,
        height,
        fps: rate(&s["avg_frame_rate"]).or_else(|| rate(&s["r_frame_rate"])),
        rotation,
        pix_fmt: text(&s["pix_fmt"]),
        profile: text(&s["profile"]),
        level: number(&s["level"]).map(|l| l as i64),
        has_b_frames: number(&s["has_b_frames"]).map(|b| b as u32),
        codec_tag: text(&s["codec_tag_string"]),
        extradata_size: number(&s["extradata_size"]).map(|n| n as u64),
    }
}

/// ffprobe's arguments for one file.
pub(crate) fn probe_args(input: &Path) -> Vec<String> {
    probe_args_as(DEMUXERS, input, true)
}

fn probe_args_as(formats: &str, input: &Path, streams: bool) -> Vec<String> {
    let mut v: Vec<String> = [
        "-v",
        "error",
        "-hide_banner",
        "-format_whitelist",
        formats,
        "-protocol_whitelist",
        "file",
        "-print_format",
        "json",
        "-show_format",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if streams {
        v.push("-show_streams".into());
    }
    v.extend(["-i".to_string(), format!("file:{}", input.display())]);
    v
}

/// Demuxers of the subtitle files that are burnt in.
const SUBTITLE_FORMATS: &[&str] = &["srt", "webvtt", "ass"];

/// Checks that a subtitle file is read as subtitles. The `subtitles` filter
/// guesses the format by itself and without the protocol whitelist, so a
/// file that FFmpeg takes for a playlist must not reach it.
pub(crate) async fn check_subtitles(
    runner: &dyn Runner,
    paths: &FfmpegPaths,
    file: &Path,
    cancel: &CancellationToken,
) -> Result<(), ToolError> {
    let argv = probe_args_as(&SUBTITLE_FORMATS.join(","), file, false);
    let out = runner
        .run(
            &paths.ffprobe.to_string_lossy(),
            &argv,
            PROBE_TIMEOUT,
            cancel.clone(),
        )
        .await
        .map_err(|e| {
            if e == "остановлено" {
                ToolError::Cancelled
            } else {
                ToolError::Failed(e)
            }
        })?;
    let format = serde_json::from_str::<Value>(&out.stdout)
        .ok()
        .map(|v| text(&v["format"]["format_name"]))
        .unwrap_or_default();
    if out.code != Some(0) || !SUBTITLE_FORMATS.contains(&format.as_str()) {
        return Err(ToolError::BadArguments(
            "файл субтитров не читается как .srt, .vtt или .ass".into(),
        ));
    }
    Ok(())
}

type Key = (String, u64, std::time::SystemTime);

/// The last files looked at, by path, size and time.
static CACHE: Mutex<VecDeque<(Key, MediaInfo)>> = Mutex::new(VecDeque::new());
const CACHE_SIZE: usize = 64;

fn cached(input: &Path) -> (Option<Key>, Option<MediaInfo>) {
    let key = file_key(input);
    let hit = key.as_ref().and_then(|k| {
        CACHE
            .lock()
            .iter()
            .find(|(c, _)| c == k)
            .map(|(_, i)| i.clone())
    });
    (key, hit)
}

fn remember(key: Option<Key>, info: &MediaInfo) {
    if let Some(k) = key {
        let mut c = CACHE.lock();
        c.retain(|(old, _)| *old != k);
        c.push_back((k, info.clone()));
        while c.len() > CACHE_SIZE {
            c.pop_front();
        }
    }
}

fn answer(out: RunOutput, input: &Path) -> Result<MediaInfo, ToolError> {
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if out.timed_out {
        return Err(ToolError::Failed(format!(
            "FFmpeg слишком долго читал «{name}»"
        )));
    }
    if out.code != Some(0) {
        return Err(ToolError::BadArguments(format!(
            "«{name}» не читается как видео или звук: файл другого вида или повреждён"
        )));
    }
    let mut info =
        parse_probe(&out.stdout).map_err(|e| ToolError::BadArguments(format!("«{name}»: {e}")))?;
    if info.size == 0 {
        info.size = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    }
    Ok(info)
}

/// For the risk check, which cannot wait asynchronously.
pub(crate) fn probe_blocking(
    runner: &dyn Runner,
    paths: &FfmpegPaths,
    input: &Path,
) -> Result<MediaInfo, ToolError> {
    let (key, hit) = cached(input);
    if let Some(info) = hit {
        return Ok(info);
    }
    let out = runner
        .run_blocking(
            &paths.ffprobe.to_string_lossy(),
            &probe_args(input),
            PROBE_TIMEOUT,
        )
        .map_err(ToolError::Failed)?;
    let info = answer(out, input)?;
    remember(key, &info);
    Ok(info)
}

pub(crate) async fn probe(
    runner: &dyn Runner,
    paths: &FfmpegPaths,
    input: &Path,
    cancel: &CancellationToken,
) -> Result<MediaInfo, ToolError> {
    let (key, hit) = cached(input);
    if let Some(info) = hit {
        return Ok(info);
    }
    let out = runner
        .run(
            &paths.ffprobe.to_string_lossy(),
            &probe_args(input),
            PROBE_TIMEOUT,
            cancel.clone(),
        )
        .await
        .map_err(|e| {
            if e == "остановлено" {
                ToolError::Cancelled
            } else {
                ToolError::Failed(e)
            }
        })?;
    let info = answer(out, input)?;
    remember(key, &info);
    Ok(info)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// ffprobe's answer for a file with the given streams.
    pub(crate) fn probe_json(
        format: &str,
        duration: f64,
        size: u64,
        video: Option<(&str, u32, u32)>,
        audio: Option<&str>,
    ) -> String {
        let mut streams = Vec::new();
        if let Some((codec, w, h)) = video {
            streams.push(serde_json::json!({
                "codec_type": "video", "codec_name": codec, "width": w, "height": h,
                "avg_frame_rate": "30/1", "r_frame_rate": "30/1", "pix_fmt": "yuv420p",
                "disposition": {"attached_pic": 0}
            }));
        }
        if let Some(codec) = audio {
            streams.push(serde_json::json!({
                "codec_type": "audio", "codec_name": codec, "sample_rate": "48000",
                "channels": 2, "bit_rate": "128000"
            }));
        }
        serde_json::json!({
            "streams": streams,
            "format": {
                "format_name": format, "duration": format!("{duration:.6}"),
                "size": size.to_string(), "bit_rate": "2000000"
            }
        })
        .to_string()
    }

    #[test]
    fn rotation_swaps_the_shown_size_and_cover_pictures_are_skipped() {
        let json = r#"{
            "streams": [
                {"codec_type":"video","codec_name":"mjpeg","width":600,"height":600,
                 "disposition":{"attached_pic":1}},
                {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,
                 "r_frame_rate":"30000/1001","avg_frame_rate":"0/0","pix_fmt":"yuv420p",
                 "side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}],
                 "disposition":{"attached_pic":0}},
                {"codec_type":"audio","codec_name":"aac","sample_rate":"44100","channels":1},
                {"codec_type":"subtitle","codec_name":"mov_text"}
            ],
            "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"75.200000",
                       "size":"52428800","bit_rate":"5577000"}
        }"#;
        let info = parse_probe(json).unwrap();
        let v = info.video.unwrap();
        assert_eq!((v.codec.as_str(), v.width, v.height), ("h264", 1080, 1920));
        assert_eq!(v.rotation, 270);
        assert!((v.fps.unwrap() - 29.97).abs() < 0.01);
        assert_eq!(v.short_side(), 1080);
        let a = info.audio.unwrap();
        assert_eq!((a.sample_rate, a.channels, a.bit_rate), (44100, 1, None));
        assert_eq!(info.subtitle_count, 1);
        assert_eq!(info.duration_s, Some(75.2));
        assert_eq!(info.size, 52_428_800);
    }

    #[test]
    fn rotate_tag_is_read_when_side_data_is_absent() {
        let json = r#"{"streams":[{"codec_type":"video","codec_name":"h264","width":640,
            "height":480,"tags":{"rotate":"90"}}],"format":{"format_name":"mov"}}"#;
        let v = parse_probe(json).unwrap().video.unwrap();
        assert_eq!((v.width, v.height, v.rotation), (480, 640, 90));
    }

    #[test]
    fn playlists_and_garbage_are_refused() {
        let hls = r#"{"streams":[],"format":{"format_name":"hls"}}"#;
        assert_eq!(parse_probe(hls), Err("это плейлист, а не медиафайл".into()));
        let concat = r#"{"streams":[],"format":{"format_name":"concat"}}"#;
        assert!(parse_probe(concat).is_err());
        assert!(parse_probe("not json").is_err());
        assert!(parse_probe("{}").is_err());
    }

    #[test]
    fn probe_argv_reads_only_a_local_file() {
        let args = probe_args(Path::new("C:\\v\\a b.mp4"));
        assert_eq!(args.last().unwrap(), "file:C:\\v\\a b.mp4");
        assert!(args
            .windows(2)
            .any(|w| w == ["-protocol_whitelist", "file"]));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "-format_whitelist" && w[1] == DEMUXERS));
    }

    #[test]
    fn join_settings_are_read() {
        let json = r#"{"streams":[
            {"codec_type":"video","codec_name":"h264","width":1920,"height":1080,
             "profile":"High","level":40,"has_b_frames":2,"codec_tag_string":"avc1",
             "extradata_size":"45","pix_fmt":"yuv420p"},
            {"codec_type":"audio","codec_name":"aac","profile":"HE-AAC","sample_fmt":"fltp",
             "sample_rate":"48000","channels":2}
        ],"format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"10"}}"#;
        let info = parse_probe(json).unwrap();
        let v = info.video.unwrap();
        assert_eq!(
            (
                v.profile.as_str(),
                v.level,
                v.has_b_frames,
                v.codec_tag.as_str(),
                v.extradata_size
            ),
            ("High", Some(40), Some(2), "avc1", Some(45))
        );
        let a = info.audio.unwrap();
        assert_eq!(
            (a.profile.as_str(), a.sample_fmt.as_str()),
            ("HE-AAC", "fltp")
        );
    }

    #[tokio::test]
    async fn subtitles_must_be_read_as_subtitles() {
        let (env, f) = crate::testing::env();
        let paths = FfmpegPaths {
            ffmpeg: "ffmpeg.exe".into(),
            ffprobe: "ffprobe.exe".into(),
            source: super::super::FfmpegSource::Path,
        };
        let file = Path::new("C:\\T\\mika_subs.srt");
        let cancel = CancellationToken::new();
        let answer = |format: &str, code: i32| {
            f.run_outputs.lock().push_back(RunOutput {
                code: Some(code),
                stdout: format!("{{\"format\":{{\"format_name\":\"{format}\"}}}}"),
                ..Default::default()
            });
        };
        answer("srt", 0);
        check_subtitles(env.runner.as_ref(), &paths, file, &cancel)
            .await
            .unwrap();
        let argv = f.runs.lock().last().unwrap().1.join(" ");
        assert!(
            argv.contains("-format_whitelist srt,webvtt,ass -protocol_whitelist file"),
            "{argv}"
        );
        answer("dash", 0);
        assert!(check_subtitles(env.runner.as_ref(), &paths, file, &cancel)
            .await
            .is_err());
        answer("", 1);
        assert!(check_subtitles(env.runner.as_ref(), &paths, file, &cancel)
            .await
            .is_err());
    }
}
