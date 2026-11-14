//! `av_convert` (another format, sound out of a video) and `video_compress`.

use std::path::Path;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{
    arg_str, arg_str_opt, progress_handle, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::args::{self, fits, s, AUDIO_EXTS, VIDEO_EXTS};
use super::encoders::{encoders, pick_h264, webm_args};
use super::job::{copy_timeout, encode_timeout, run_ffmpeg};
use super::output::{ext_of, resolve_output, stem, PartFile, WorkDir};
use super::{
    about, bad, done_text, files_card, need_audio, need_video, not_too_long, quoted, size_text,
    AvEnv, MediaInfo, MAX_ENCODE_S,
};
use crate::util::schema;

const MB: f64 = 1024.0 * 1024.0;

/// What happens to one kind of stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Track {
    Copy,
    Encode,
}

/// Video and sound of a conversion; `None` leaves that kind out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ConvertPlan {
    pub video: Option<Track>,
    pub audio: Option<Track>,
}

impl ConvertPlan {
    /// Something is encoded: the conversion takes as long as the file is.
    pub(crate) fn encodes(&self) -> bool {
        self.video == Some(Track::Encode) || self.audio == Some(Track::Encode)
    }
}

/// Copy what fits the new format, encode the rest.
pub(crate) fn plan_convert(
    input_ext: &str,
    info: &MediaInfo,
    format: &str,
) -> Result<ConvertPlan, ToolError> {
    let track = |codec: &str, video: bool| {
        if fits(format, codec, video) {
            Track::Copy
        } else {
            Track::Encode
        }
    };
    let plan = if VIDEO_EXTS.contains(&format) {
        let v = info.video.as_ref().ok_or_else(|| {
            bad("в файле нет видео: его можно перевести только в звуковой формат (mp3, m4a, wav, flac, ogg, opus)")
        })?;
        ConvertPlan {
            video: Some(track(&v.codec, true)),
            audio: info.audio.as_ref().map(|a| track(&a.codec, false)),
        }
    } else {
        let a = need_audio(info)?;
        ConvertPlan {
            video: None,
            audio: Some(track(&a.codec, false)),
        }
    };
    let only_copies = !plan.encodes() && (plan.video.is_some() || info.video.is_none());
    if input_ext == format && only_copies {
        return Err(bad("файл уже в этом формате"));
    }
    Ok(plan)
}

pub struct AvConvertTool(pub AvEnv);

struct ConvertJob {
    input: std::path::PathBuf,
    info: MediaInfo,
    format: String,
    plan: ConvertPlan,
    output: std::path::PathBuf,
}

impl AvConvertTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<ConvertJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let format = convert_format(args)?;
        let info = info_of(&input)?;
        let plan = plan_convert(&ext_of(&input), &info, &format)?;
        if plan.encodes() {
            not_too_long(&info, MAX_ENCODE_S)?;
        }
        let output = resolve_output(
            &input,
            arg_str_opt(args, "output"),
            &format!("{}.{format}", stem(&input)),
            &format,
            std::slice::from_ref(&input),
            &self.0.user_profile,
        )?;
        Ok(ConvertJob {
            input,
            info,
            format,
            plan,
            output,
        })
    }
}

#[async_trait]
impl Tool for AvConvertTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "av_convert",
            "Перевести видео или звук в другой формат (webm в mp4, wav в mp3) или извлечь звук из видео (указать звуковой формат). Исходный файл не меняется, результат — новый файл рядом.",
            schema(
                &[
                    ("path", "string", "Полный путь к файлу"),
                    (
                        "format",
                        "string",
                        "one of: mp4, mkv, mov, webm, mp3, m4a, wav, flac, ogg, opus",
                    ),
                    (
                        "output",
                        "string",
                        "Имя или путь нового файла; без него — рядом с исходным",
                    ),
                ],
                &["path", "format"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        let what = if job.plan.video.is_none() && job.info.video.is_some() {
            format!(
                "Извлечь звук из {} в {}",
                about(&job.input, &job.info),
                job.format.to_uppercase()
            )
        } else {
            format!(
                "Перевести {} в {}",
                about(&job.input, &job.info),
                job.format.to_uppercase()
            )
        };
        let summary = format!("{what} → {}", quoted(&job.output));
        Ok(self.0.assess_io(summary, &[&job.input], &job.output))
    }

    async fn run(
        &self,
        args: Value,
        _approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        self.exec(args, cancel).await
    }
}

impl AvConvertTool {
    /// The work of `run`, for a caller that holds the approval itself: one
    /// approval of a folder covers every file on its card.
    pub(crate) async fn exec(
        &self,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let progress = progress_handle();
        let paths = self.0.ffmpeg()?;
        let info = {
            let input = self.0.input(&args, "path")?;
            self.0.probe(&paths, &input, &cancel).await?
        };
        let job = self.prepare(&args, |_| Ok(info.clone()))?;
        let runner = self.0.runner.as_ref();
        let list = if job.plan.encodes() {
            Some(encoders(runner, &paths, &cancel).await?)
        } else {
            None
        };
        let has = |name: &str| list.as_ref().is_some_and(|l| l.has(name));
        let webm = job.format == "webm";
        let video = match (job.plan.video, &job.info.video) {
            (Some(Track::Copy), Some(v)) => {
                let mut a = s(&["-c:v", "copy"]);
                if v.codec == "hevc" && matches!(job.format.as_str(), "mp4" | "mov") {
                    a.extend(s(&["-tag:v", "hvc1"]));
                }
                Some(a)
            }
            (Some(Track::Encode), Some(_)) if webm => {
                if !has("libvpx-vp9") {
                    return Err(ToolError::Failed(
                        "в этой сборке FFmpeg нет кодировщика VP9".into(),
                    ));
                }
                let mut w = webm_args();
                w.truncate(w.len() - 4);
                Some(w)
            }
            (Some(Track::Encode), Some(v)) => {
                let h = pick_h264(runner, &paths, &cancel).await?;
                Some(h.quality_args(false, v.width, v.height, v.fps.unwrap_or(30.0)))
            }
            _ => None,
        };
        let audio = match job.plan.audio {
            Some(Track::Copy) => Some(s(&["-c:a", "copy"])),
            Some(Track::Encode) => {
                Some(args::audio_codec(&job.format, has).map_err(ToolError::Failed)?)
            }
            None => None,
        };
        let part = PartFile::new(&job.output);
        let argv = args::convert_args(
            &job.input,
            video.as_deref(),
            audio.as_deref(),
            &job.format,
            part.path(),
        );
        let duration = job.info.duration();
        let timeout = if job.plan.encodes() {
            encode_timeout(duration)
        } else {
            copy_timeout(duration)
        };
        run_ffmpeg(
            "av_convert",
            runner,
            &paths,
            &argv,
            None,
            duration,
            (0, 100),
            &progress,
            &cancel,
            timeout,
        )
        .await?;
        let (done, out_info) = self.0.finish(&paths, part, &job.output, &cancel).await?;
        let text = done_text(&done, out_info.as_ref(), Some(job.info.size));
        let card = files_card(&[(
            done,
            out_info.and_then(|i| i.duration_s).or(job.info.duration_s),
        )]);
        Ok(ToolOutput::new(text).with_data(card))
    }
}

/// How to compress.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CompressPlan {
    /// The shorter side after scaling; `None` keeps the size.
    pub height: Option<u32>,
    /// Average video bit rate for a size to reach; `None` is quality mode.
    pub video_kbps: Option<u64>,
    pub audio_kbps: u64,
    pub strong: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Compress {
    /// The file is already smaller than asked: nothing to do.
    Already {
        mb: u64,
    },
    Plan(CompressPlan),
}

const HEIGHTS: &[u32] = &[2160, 1440, 1080, 720, 480, 360];

/// The bit rates and the size that bring the file to `target_mb`.
pub(crate) fn plan_compress(
    info: &MediaInfo,
    target_mb: Option<f64>,
    quality: Option<bool>,
    height: Option<u32>,
) -> Result<Compress, ToolError> {
    let v = need_video(info)?;
    let short = v.short_side();
    let scale = height.filter(|h| short > *h);
    if let (Some(h), None, None, None) = (height, scale, target_mb, quality) {
        return Err(bad(format!("разрешение уже не больше {h}p")));
    }
    let has_audio = info.audio.is_some();
    let Some(mb) = target_mb else {
        let strong = quality.unwrap_or(false);
        return Ok(Compress::Plan(CompressPlan {
            height: scale,
            video_kbps: None,
            audio_kbps: if strong { 96 } else { 128 },
            strong,
        }));
    };
    let target = mb * MB;
    if target >= info.size as f64 {
        return Ok(Compress::Already {
            mb: (info.size as f64 / MB).ceil() as u64,
        });
    }
    let duration = info.duration();
    if duration <= 0.0 {
        return Err(bad("не удалось узнать длительность видео"));
    }
    let total = |audio: f64| target * 8.0 / 1000.0 / duration * 0.97 - audio;
    let mut audio = if has_audio { 128.0 } else { 0.0 };
    let mut video = total(audio);
    let mut side = scale.unwrap_or(short);
    for (limit, to) in [(400.0, 720), (250.0, 480), (150.0, 360)] {
        if video < limit && side > to {
            side = to;
        }
    }
    if video < 150.0 && has_audio {
        audio = 64.0;
        video = total(audio);
    }
    if video < 80.0 {
        let least = ((80.0 + audio) * duration * 1000.0 / 8.0 / 0.97 / MB).ceil();
        return Err(bad(format!(
            "до {} МБ не сжать: при такой длине нужно хотя бы {least} МБ",
            super::args::number(mb)
        )));
    }
    Ok(Compress::Plan(CompressPlan {
        height: (side < short).then_some(side),
        video_kbps: Some(video.floor() as u64),
        audio_kbps: audio as u64,
        strong: false,
    }))
}

/// Picture size after scaling the shorter side to `side`, even numbers.
fn scaled(w: u32, h: u32, side: Option<u32>) -> (u32, u32) {
    let Some(side) = side else {
        return (w, h);
    };
    let even = |x: f64| ((x / 2.0).round() as u32 * 2).max(2);
    if w > h {
        (even(w as f64 * side as f64 / h.max(1) as f64), side)
    } else {
        (side, even(h as f64 * side as f64 / w.max(1) as f64))
    }
}

pub struct VideoCompressTool(pub AvEnv);

struct CompressArgs {
    target_mb: Option<f64>,
    quality: Option<bool>,
    height: Option<u32>,
}

fn compress_args(args: &Value) -> Result<CompressArgs, ToolError> {
    let target_mb = match args.get("target_mb") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let n = v
                .as_f64()
                .or_else(|| {
                    v.as_str()
                        .and_then(|s| s.trim().replace(',', ".").parse().ok())
                })
                .ok_or_else(|| bad("target_mb — число мегабайт"))?;
            if !(1.0..=4000.0).contains(&n) {
                return Err(bad("target_mb — от 1 до 4000 МБ"));
            }
            Some(n)
        }
    };
    let quality = match arg_str_opt(args, "quality").map(str::trim) {
        None | Some("") => None,
        Some("normal") => Some(false),
        Some("strong") => Some(true),
        Some(q) => return Err(bad(format!("quality — normal или strong, а не «{q}»"))),
    };
    let height = match args.get("height") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let h = v
                .as_u64()
                .or_else(|| v.as_f64().map(|f| f as u64))
                .or_else(|| {
                    v.as_str()
                        .and_then(|s| s.trim().trim_end_matches('p').parse().ok())
                })
                .ok_or_else(|| bad("height — число строк: 2160, 1440, 1080, 720, 480 или 360"))?;
            let h = u32::try_from(h).unwrap_or(0);
            if !HEIGHTS.contains(&h) {
                return Err(bad("height — 2160, 1440, 1080, 720, 480 или 360"));
            }
            Some(h)
        }
    };
    if target_mb.is_none() && quality.is_none() && height.is_none() {
        return Err(bad("укажи target_mb, quality или height"));
    }
    if target_mb.is_some() && quality.is_some() {
        return Err(bad("target_mb и quality вместе не задаются: выбери одно"));
    }
    Ok(CompressArgs {
        target_mb,
        quality,
        height,
    })
}

/// Checks the compression arguments without a file: a folder call is
/// refused at once, not file by file.
pub(crate) fn check_compress_args(args: &Value) -> Result<(), ToolError> {
    compress_args(args).map(|_| ())
}

/// The formats `av_convert` writes, lower case.
pub(crate) fn convert_format(args: &Value) -> Result<String, ToolError> {
    let format = arg_str(args, "format")?
        .trim()
        .trim_start_matches('.')
        .to_lowercase();
    if !VIDEO_EXTS.contains(&format.as_str()) && !AUDIO_EXTS.contains(&format.as_str()) {
        return Err(bad(format!(
            "формат «{format}» не поддержан: mp4, mkv, mov, webm, mp3, m4a, wav, flac, ogg, opus"
        )));
    }
    Ok(format)
}

struct CompressJob {
    input: std::path::PathBuf,
    info: MediaInfo,
    plan: Compress,
    target_mb: Option<f64>,
    output: std::path::PathBuf,
}

impl VideoCompressTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<CompressJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let a = compress_args(args)?;
        let info = info_of(&input)?;
        need_video(&info)?;
        not_too_long(&info, MAX_ENCODE_S)?;
        let plan = plan_compress(&info, a.target_mb, a.quality, a.height)?;
        let squeezed = a.target_mb.is_some() || a.quality.is_some();
        let mark = match (squeezed, a.height) {
            (true, Some(h)) => format!("сжато, {h}p"),
            (false, Some(h)) => format!("{h}p"),
            _ => "сжато".to_string(),
        };
        let output = resolve_output(
            &input,
            arg_str_opt(args, "output"),
            &format!("{} ({mark}).mp4", stem(&input)),
            "mp4",
            std::slice::from_ref(&input),
            &self.0.user_profile,
        )?;
        Ok(CompressJob {
            input,
            info,
            plan,
            target_mb: a.target_mb,
            output,
        })
    }
}

#[async_trait]
impl Tool for VideoCompressTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "video_compress",
            "Сжать видео: до размера в мегабайтах (target_mb), по качеству (quality) или уменьшив разрешение (height — число строк короткой стороны: 2160, 1440, 1080, 720, 480 или 360). Результат — новый файл mp4 (H.264 и AAC) рядом с исходным.",
            schema(
                &[
                    ("path", "string", "Полный путь к видео"),
                    ("target_mb", "number", "Нужный размер файла в МБ, от 1 до 4000"),
                    ("quality", "string", "one of: normal, strong"),
                    (
                        "height",
                        "integer",
                        "Короткая сторона кадра после сжатия: 2160, 1440, 1080, 720, 480 или 360",
                    ),
                    (
                        "output",
                        "string",
                        "Имя или путь нового файла; без него — рядом с исходным",
                    ),
                ],
                &["path"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        let what = about(&job.input, &job.info);
        let summary = match (&job.plan, job.target_mb) {
            // Its own scope: «Всегда» here must not cover a call that writes.
            (Compress::Already { mb }, _) => {
                return Ok(Assessment::new(
                    RiskLevel::Low,
                    format!("Проверить размер {what}: он уже меньше {mb} МБ"),
                    job.input.display().to_string(),
                    "ничего не записывается",
                )
                .with_scope(format!(
                    "check:{}",
                    job.input.display().to_string().to_lowercase()
                )));
            }
            (Compress::Plan(_), Some(mb)) => {
                format!("Сжать {what} до {} МБ", super::args::number(mb))
            }
            (Compress::Plan(p), None)
                if p.video_kbps.is_none() && args.get("quality").is_none() =>
            {
                format!("Уменьшить {what} до {}p", p.height.unwrap_or(0))
            }
            (Compress::Plan(p), None) if p.strong => format!("Сильно сжать {what}"),
            (Compress::Plan(_), None) => format!("Сжать {what}"),
        };
        let summary = format!("{summary} → {}", quoted(&job.output));
        Ok(self.0.assess_io(summary, &[&job.input], &job.output))
    }

    async fn run(
        &self,
        args: Value,
        _approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        self.exec(args, cancel).await
    }
}

impl VideoCompressTool {
    /// The work of `run`, for a caller that holds the approval itself.
    pub(crate) async fn exec(
        &self,
        args: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let progress = progress_handle();
        let paths = self.0.ffmpeg()?;
        let info = {
            let input = self.0.input(&args, "path")?;
            self.0.probe(&paths, &input, &cancel).await?
        };
        let job = self.prepare(&args, |_| Ok(info.clone()))?;
        let plan = match &job.plan {
            Compress::Already { mb } => {
                return Ok(ToolOutput::new(format!(
                    "Файл уже меньше {mb} МБ ({}), сжимать не нужно.",
                    size_text(job.info.size)
                )));
            }
            Compress::Plan(p) => p.clone(),
        };
        let runner = self.0.runner.as_ref();
        let h264 = pick_h264(runner, &paths, &cancel).await?;
        let v = need_video(&job.info)?;
        let (w, h) = scaled(v.width, v.height, plan.height);
        let filter = plan.height.map(args::scale_short);
        let audio = job.info.audio.as_ref().map(|_| {
            vec![
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                format!("{}k", plan.audio_kbps),
            ]
        });
        let duration = job.info.duration();
        let timeout = encode_timeout(duration);
        let work = WorkDir::new()?;
        let encode = |video: Vec<String>, pass: Option<(u8, &Path)>, out: &Path| {
            args::compress_args(
                &job.input,
                filter.as_deref(),
                &video,
                audio.as_deref(),
                pass,
                out,
            )
        };

        let mut part = PartFile::new(&job.output);
        match plan.video_kbps {
            None => {
                let video = h264.quality_args(plan.strong, w, h, v.fps.unwrap_or(30.0));
                let argv = encode(video, None, part.path());
                run_ffmpeg(
                    "av_convert",
                    runner,
                    &paths,
                    &argv,
                    None,
                    duration,
                    (0, 100),
                    &progress,
                    &cancel,
                    timeout,
                )
                .await?;
            }
            Some(kbps) if h264.two_pass() => {
                let log = work.path().join("mika2p");
                let first = encode(h264.bitrate_args(kbps), Some((1, &log)), part.path());
                run_ffmpeg(
                    "av_convert",
                    runner,
                    &paths,
                    &first,
                    Some(work.path()),
                    duration,
                    (0, 50),
                    &progress,
                    &cancel,
                    timeout,
                )
                .await?;
                let second = encode(h264.bitrate_args(kbps), Some((2, &log)), part.path());
                run_ffmpeg(
                    "av_convert",
                    runner,
                    &paths,
                    &second,
                    Some(work.path()),
                    duration,
                    (50, 100),
                    &progress,
                    &cancel,
                    timeout,
                )
                .await?;
            }
            Some(kbps) => {
                let argv = encode(h264.bitrate_args(kbps), None, part.path());
                run_ffmpeg(
                    "av_convert",
                    runner,
                    &paths,
                    &argv,
                    None,
                    duration,
                    (0, 100),
                    &progress,
                    &cancel,
                    timeout,
                )
                .await?;
                let target = job.target_mb.unwrap_or(0.0) * MB;
                let got = std::fs::metadata(part.path()).map(|m| m.len()).unwrap_or(0) as f64;
                if got > target * 1.05 && got > 0.0 {
                    // One more try at a bit rate corrected by the miss.
                    let again = ((kbps as f64) * target / got * 0.95).max(80.0) as u64;
                    let retry = PartFile::new(&job.output);
                    let argv = encode(h264.bitrate_args(again), None, retry.path());
                    run_ffmpeg(
                        "av_convert",
                        runner,
                        &paths,
                        &argv,
                        None,
                        duration,
                        (0, 100),
                        &progress,
                        &cancel,
                        timeout,
                    )
                    .await?;
                    part = retry;
                }
            }
        }
        let (done, out_info) = self.0.finish(&paths, part, &job.output, &cancel).await?;
        let mut text = done_text(&done, out_info.as_ref(), Some(job.info.size));
        if let Some(mb) = job.target_mb {
            let got = std::fs::metadata(&done).map(|m| m.len()).unwrap_or(0) as f64;
            if got > mb * MB * 1.05 {
                text.push_str(&format!(
                    " До {} МБ точно не вышло.",
                    super::args::number(mb)
                ));
            }
        }
        let card = files_card(&[(
            done,
            out_info.and_then(|i| i.duration_s).or(job.info.duration_s),
        )]);
        Ok(ToolOutput::new(text).with_data(card))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{av, probe_json, X264_LIST};
    use super::*;
    use crate::testing::run;
    use serde_json::json;

    fn info(json: &str) -> MediaInfo {
        super::super::probe::parse_probe(json).unwrap()
    }

    #[test]
    fn conversion_copies_what_fits_and_encodes_the_rest() {
        let webm = info(&probe_json(
            "matroska,webm",
            31.0,
            2_000_000,
            Some(("vp9", 1280, 720)),
            Some("opus"),
        ));
        assert_eq!(
            plan_convert("webm", &webm, "mp4").unwrap(),
            ConvertPlan {
                video: Some(Track::Encode),
                audio: Some(Track::Copy)
            }
        );
        assert_eq!(
            plan_convert("webm", &webm, "mkv").unwrap(),
            ConvertPlan {
                video: Some(Track::Copy),
                audio: Some(Track::Copy)
            }
        );
        assert_eq!(
            plan_convert("webm", &webm, "mp3").unwrap(),
            ConvertPlan {
                video: None,
                audio: Some(Track::Encode)
            },
            "sound out of a video"
        );
        assert!(matches!(
            plan_convert("webm", &webm, "webm"),
            Err(ToolError::BadArguments(m)) if m == "файл уже в этом формате"
        ));
        let wav = info(&probe_json("wav", 10.0, 1_000_000, None, Some("pcm_s16le")));
        assert!(
            matches!(plan_convert("wav", &wav, "mp4"), Err(ToolError::BadArguments(m)) if m.contains("нет видео"))
        );
        assert!(plan_convert("wav", &wav, "wav").is_err());
        assert_eq!(
            plan_convert("wav", &wav, "flac").unwrap().audio,
            Some(Track::Encode)
        );
        let mp4 = info(&probe_json(
            "mov,mp4",
            10.0,
            1_000_000,
            Some(("mpeg2video", 640, 480)),
            Some("aac"),
        ));
        assert_eq!(
            plan_convert("mp4", &mp4, "mp4").unwrap().video,
            Some(Track::Encode),
            "the same format with a codec that does not fit is encoded"
        );
        // WAV holds little-endian PCM only; AIFF's big-endian is encoded.
        let aiff = info(&probe_json(
            "aiff",
            10.0,
            1_000_000,
            None,
            Some("pcm_s16be"),
        ));
        assert_eq!(
            plan_convert("aiff", &aiff, "wav").unwrap().audio,
            Some(Track::Encode)
        );
        // AV1 goes into MP4, not into MOV.
        let av1 = info(&probe_json(
            "matroska,webm",
            10.0,
            1_000_000,
            Some(("av1", 1280, 720)),
            Some("opus"),
        ));
        assert_eq!(
            plan_convert("webm", &av1, "mov").unwrap().video,
            Some(Track::Encode)
        );
        assert_eq!(
            plan_convert("webm", &av1, "mp4").unwrap().video,
            Some(Track::Copy)
        );
    }

    fn mb(x: f64) -> u64 {
        (x * MB) as u64
    }

    #[test]
    fn target_size_lowers_resolution_when_the_rate_is_low() {
        // 12:31 of 1080p, 245 МБ.
        let big = info(&probe_json(
            "mov,mp4",
            751.0,
            mb(245.0),
            Some(("h264", 1920, 1080)),
            Some("aac"),
        ));
        // 50 МБ: 50×1048576×8/1000/751×0.97 − 128 ≈ 413 kbit/s: the size stays.
        let Compress::Plan(p) = plan_compress(&big, Some(50.0), None, None).unwrap() else {
            panic!()
        };
        assert_eq!(
            (p.height, p.video_kbps, p.audio_kbps),
            (None, Some(413), 128)
        );
        // 40 МБ: ≈ 305 kbit/s, under 400: 720p.
        let Compress::Plan(p) = plan_compress(&big, Some(40.0), None, None).unwrap() else {
            panic!()
        };
        assert_eq!(p.height, Some(720));
        // 20 МБ: ≈ 88 kbit/s, under 150: 360p and 64 kbit/s of sound.
        let Compress::Plan(p) = plan_compress(&big, Some(20.0), None, None).unwrap() else {
            panic!()
        };
        assert_eq!(
            (p.height, p.audio_kbps, p.video_kbps),
            (Some(360), 64, Some(152))
        );
        // 10 МБ is too little for 12 minutes.
        assert!(matches!(
            plan_compress(&big, Some(10.0), None, None),
            Err(ToolError::BadArguments(m)) if m.contains("нужно хотя бы 14 МБ")
        ));
        assert_eq!(
            plan_compress(&big, Some(300.0), None, None).unwrap(),
            Compress::Already { mb: 245 }
        );
    }

    #[test]
    fn height_alone_must_change_something() {
        let small = info(&probe_json(
            "mov,mp4",
            60.0,
            mb(20.0),
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        assert!(matches!(
            plan_compress(&small, None, None, Some(720)),
            Err(ToolError::BadArguments(m)) if m == "разрешение уже не больше 720p"
        ));
        let Compress::Plan(p) = plan_compress(&small, None, Some(true), Some(720)).unwrap() else {
            panic!()
        };
        assert_eq!((p.height, p.strong, p.audio_kbps), (None, true, 96));
        let portrait = info(&probe_json(
            "mov,mp4",
            60.0,
            mb(20.0),
            Some(("h264", 1080, 1920)),
            None,
        ));
        let Compress::Plan(p) = plan_compress(&portrait, None, None, Some(720)).unwrap() else {
            panic!()
        };
        assert_eq!(p.height, Some(720));
        assert_eq!(scaled(1080, 1920, Some(720)), (720, 1280));
        assert_eq!(scaled(1920, 1080, Some(480)), (854, 480));
    }

    #[test]
    fn compress_arguments_are_checked() {
        let ok = |v: Value| compress_args(&v).is_ok();
        assert!(ok(json!({"target_mb": 50})));
        assert!(ok(json!({"target_mb": "49,5"})));
        assert!(ok(json!({"height": "720p"})));
        assert!(!ok(json!({})));
        assert!(!ok(json!({"target_mb": 50, "quality": "strong"})));
        assert!(!ok(json!({"target_mb": 0.5})));
        assert!(!ok(json!({"height": 800})));
        assert!(!ok(json!({"quality": "best"})));
    }

    #[tokio::test]
    async fn compress_to_a_size_runs_two_passes_of_libx264() {
        let t = av();
        let input = t.file("x.mp4");
        t.answer(&probe_json(
            "mov,mp4",
            751.0,
            mb(245.0),
            Some(("h264", 1920, 1080)),
            Some("aac"),
        ));
        t.answer(X264_LIST);
        t.answer("");
        t.answer("out_time_us=375500000\nprogress=end");
        t.answer(&probe_json(
            "mov,mp4",
            751.0,
            mb(48.7),
            Some(("h264", 1920, 1080)),
            Some("aac"),
        ));
        let tool = VideoCompressTool(t.av());
        let args = json!({ "path": input.display().to_string(), "target_mb": 50 });
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert_eq!(
            a.summary,
            "Сжать «x.mp4» (12:31, 245 МБ) до 50 МБ → «x (сжато).mp4»"
        );
        let folder = t.dir.path().display().to_string().to_lowercase();
        assert_eq!(a.scope, format!("{folder} -> {folder}"));
        let out = run(&tool, args).await.unwrap();
        let result = t.dir.path().join("x (сжато).mp4");
        assert!(result.is_file());
        assert!(
            out.text
                .starts_with(&format!("Готово: {}", result.display())),
            "{}",
            out.text
        );
        assert!(out.text.contains("(было 245 МБ)"), "{}", out.text);
        assert_eq!(out.data.as_ref().unwrap()["kind"], "av_files");
        assert_eq!(out.data.unwrap()["files"][0]["duration_s"], 751.0);
        let runs = t.ffmpeg_runs();
        assert_eq!(runs.len(), 3, "the list and two passes");
        assert!(runs[1].join(" ").contains("-pass 1"));
        assert!(runs[1].join(" ").ends_with("-f null NUL"));
        assert!(runs[2].join(" ").contains("-pass 2"));
        assert!(t.av().outputs.contains(&result));
        assert_eq!(
            std::fs::read(&input).unwrap(),
            b"media",
            "the original is untouched"
        );
    }

    #[tokio::test]
    async fn already_small_file_is_not_encoded() {
        let t = av();
        let input = t.file("x.mp4");
        t.answer(&probe_json(
            "mov,mp4",
            60.0,
            mb(10.0),
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        let tool = VideoCompressTool(t.av());
        let args = json!({ "path": input.display().to_string(), "target_mb": 50 });
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert!(
            a.scope.starts_with("check:") && a.scope.ends_with("x.mp4"),
            "a rule from this card must not match the folders of a write: {}",
            a.scope
        );
        let out = run(&tool, args).await.unwrap();
        assert!(
            out.text.starts_with("Файл уже меньше 10 МБ"),
            "{}",
            out.text
        );
        assert!(t.ffmpeg_runs().is_empty());
    }

    #[tokio::test]
    async fn convert_copies_into_a_new_container() {
        let t = av();
        let input = t.file("clip.mkv");
        t.answer(&probe_json(
            "matroska,webm",
            31.0,
            3_000_000,
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        let tool = AvConvertTool(t.av());
        let args = json!({ "path": input.display().to_string(), "format": "mp4" });
        let a = tool.assess(&args).unwrap();
        assert_eq!(
            a.summary,
            "Перевести «clip.mkv» (0:31, 2,9 МБ) в MP4 → «clip.mp4»"
        );
        let out = run(&tool, args).await.unwrap();
        assert!(out.text.contains("clip.mp4"));
        let runs = t.ffmpeg_runs();
        assert_eq!(runs.len(), 1, "no encoder list for a copy");
        let l = runs[0].join(" ");
        assert!(
            l.contains("-c:v copy -c:a copy -sn -dn -movflags +faststart -n file:"),
            "{l}"
        );
        assert!(l.contains("clip.mika-part.mp4"), "{l}");
    }

    #[tokio::test]
    async fn output_under_program_files_is_high_risk() {
        let t = av();
        let input = t.file("a.wav");
        t.answer(&probe_json("wav", 10.0, 1_000_000, None, Some("pcm_s16le")));
        let tool = AvConvertTool(t.av());
        let args = json!({
            "path": input.display().to_string(),
            "format": "mp3",
            "output": "C:\\Program Files\\a.mp3"
        });
        assert_eq!(tool.assess(&args).unwrap().level, RiskLevel::High);
    }
}
