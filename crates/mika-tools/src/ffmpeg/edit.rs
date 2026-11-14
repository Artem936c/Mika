//! Cutting and joining: `av_cut`, `av_join`, `video_gif`, `video_rotate`,
//! `av_speed`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{
    arg_str_opt, progress_handle, spec, Assessment, Tool, ToolError, ToolOutput,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::args::{self, audio_ext, number, s, video_ext, JoinInput, COPY_EXTS, VIDEO_EXTS};
use super::encoders::{encoders, pick_h264};
use super::job::{copy_timeout, encode_timeout, run_ffmpeg};
use super::output::{ext_of, resolve_output, stem, PartFile, WorkDir};
use super::time::{name_time, parse_time, show_time};
use super::{
    aac, about, bad, done_text, files_card, need_video, not_too_long, quoted, video_codec,
    video_sound, AvEnv, FfmpegPaths, MediaInfo, MAX_ENCODE_S,
};
use crate::util::{arg_bool, schema};

const OUTPUT_DESC: &str = "Имя или путь нового файла; без него — рядом с исходным";

/// A number argument written as a number or as text: `2`, `"0,5"`, `"x2"`.
fn arg_number(args: &Value, key: &str) -> Result<Option<f64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_f64()),
        Some(Value::String(t)) if t.trim().is_empty() => Ok(None),
        Some(Value::String(t)) => t
            .trim()
            .trim_start_matches(['x', 'х', '×'])
            .replace(',', ".")
            .parse()
            .map(Some)
            .map_err(|_| bad(format!("`{key}` — число, а не «{t}»"))),
        Some(_) => Err(bad(format!("`{key}` — число"))),
    }
}

/// A whole number within `range`, `default` when absent.
fn arg_whole(args: &Value, key: &str, default: u32, range: (u32, u32)) -> Result<u32, ToolError> {
    let Some(n) = arg_number(args, key)? else {
        return Ok(default);
    };
    let n = n.round();
    if n < range.0 as f64 || n > range.1 as f64 {
        return Err(bad(format!("`{key}` — от {} до {}", range.0, range.1)));
    }
    Ok(n as u32)
}

fn time_arg(args: &Value, key: &str) -> Result<Option<f64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_f64()
            .filter(|t| *t >= 0.0)
            .map(Some)
            .ok_or_else(|| bad(format!("`{key}` не может быть меньше нуля"))),
        Some(Value::String(t)) if t.trim().is_empty() => Ok(None),
        Some(Value::String(t)) => parse_time(t).map(Some).map_err(bad),
        Some(_) => Err(bad(format!("`{key}` — время"))),
    }
}

fn known_length(info: &MediaInfo) -> Result<f64, ToolError> {
    info.duration_s
        .filter(|d| *d > 0.0)
        .ok_or_else(|| bad("не удалось узнать длительность файла"))
}

/// Start and length of a piece. `end` and `duration` exclude each other.
pub(crate) fn span(
    total: f64,
    start: Option<f64>,
    end: Option<f64>,
    duration: Option<f64>,
) -> Result<(f64, f64), ToolError> {
    let start = start.unwrap_or(0.0);
    if start >= total {
        return Err(bad(format!(
            "начало {} не раньше конца файла ({})",
            show_time(start),
            show_time(total)
        )));
    }
    let length = match (end, duration) {
        (Some(_), Some(_)) => return Err(bad("end и duration вместе не задаются: выбери одно")),
        (Some(e), None) => {
            if e <= start {
                return Err(bad("конец должен быть позже начала"));
            }
            if e > total + 0.5 {
                return Err(bad(format!(
                    "конец {} позже конца файла ({})",
                    show_time(e),
                    show_time(total)
                )));
            }
            e.min(total) - start
        }
        (None, Some(d)) if d <= 0.0 => return Err(bad("длительность должна быть больше нуля")),
        (None, Some(d)) => d.min(total - start),
        (None, None) => total - start,
    };
    Ok((start, length))
}

// ---------------------------------------------------------------- av_cut

pub struct AvCutTool(pub AvEnv);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CutMode {
    /// Sound only: streams copied.
    Sound,
    /// Video copied: the edges snap to keyframes.
    Fast,
    Encode,
}

struct CutJob {
    input: PathBuf,
    info: MediaInfo,
    start: f64,
    length: f64,
    mode: CutMode,
    ext: String,
    output: PathBuf,
}

impl AvCutTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<CutJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let info = info_of(&input)?;
        let total = known_length(&info)?;
        let (start, length) = span(
            total,
            time_arg(args, "start")?,
            time_arg(args, "end")?,
            time_arg(args, "duration")?,
        )?;
        if start <= 0.05 && start + length >= total - 0.05 {
            return Err(bad(
                "так вырежется весь файл: укажи start, end или duration",
            ));
        }
        let input_ext = ext_of(&input);
        let (mode, ext) = match (&info.video, &info.audio) {
            (Some(_), _) if arg_bool(args, "fast") && VIDEO_EXTS.contains(&input_ext.as_str()) => {
                (CutMode::Fast, input_ext.clone())
            }
            (Some(_), _) => (CutMode::Encode, video_ext(&input_ext).to_string()),
            // APE, DSF and the like can be read but not written: encoded.
            (None, Some(_)) if COPY_EXTS.contains(&input_ext.as_str()) => {
                (CutMode::Sound, input_ext.clone())
            }
            (None, Some(_)) => (CutMode::Encode, audio_ext(&input_ext).to_string()),
            (None, None) => return Err(bad("в файле нет ни видео, ни звука")),
        };
        if mode == CutMode::Encode {
            not_too_long(&info, MAX_ENCODE_S)?;
        }
        let name = format!(
            "{} (фрагмент {}-{}).{ext}",
            stem(&input),
            name_time(start),
            name_time(start + length)
        );
        let output = resolve_output(
            &input,
            arg_str_opt(args, "output"),
            &name,
            &ext,
            std::slice::from_ref(&input),
            &self.0.user_profile,
        )?;
        Ok(CutJob {
            input,
            info,
            start,
            length,
            mode,
            ext,
            output,
        })
    }
}

#[async_trait]
impl Tool for AvCutTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "av_cut",
            "Вырезать кусок видео или звука по времени в новый файл. Время — секунды (90) или м:сс (1:30), ч:мм:сс. Задай end или duration, не оба.",
            schema(
                &[
                    ("path", "string", "Полный путь к файлу"),
                    ("start", "string", "Начало куска, по умолчанию 0"),
                    ("end", "string", "Конец куска"),
                    ("duration", "string", "Длина куска"),
                    (
                        "fast",
                        "boolean",
                        "true — без перекодирования, быстро, но границы сдвигаются к ключевым кадрам",
                    ),
                    ("output", "string", OUTPUT_DESC),
                ],
                &["path"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        let summary = format!(
            "Вырезать {}–{} из {} → {}",
            show_time(job.start),
            show_time(job.start + job.length),
            about(&job.input, &job.info),
            quoted(&job.output)
        );
        Ok(self.0.assess_io(summary, &[&job.input], &job.output))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let progress = progress_handle();
        let paths = self.0.ffmpeg()?;
        let info = probe_arg(&self.0, &paths, &args, &cancel).await?;
        let job = self.prepare(&args, |_| Ok(info.clone()))?;
        let runner = self.0.runner.as_ref();
        let (codec, timeout) = match job.mode {
            CutMode::Sound => (s(&["-c", "copy"]), copy_timeout(job.length)),
            CutMode::Fast => (
                s(&["-c", "copy", "-avoid_negative_ts", "make_zero"]),
                copy_timeout(job.length),
            ),
            CutMode::Encode => {
                let codec = match &job.info.video {
                    Some(v) => {
                        let h = pick_h264(runner, &paths, &cancel).await?;
                        video_codec(&job.ext, h, v, job.info.audio.as_ref().map(|_| aac()))
                    }
                    None => {
                        let list = encoders(runner, &paths, &cancel).await?;
                        args::audio_codec(&job.ext, |e| list.has(e)).map_err(ToolError::Failed)?
                    }
                };
                (codec, encode_timeout(job.length))
            }
        };
        let part = PartFile::new(&job.output);
        let argv = args::cut_args(
            &job.input,
            job.start,
            job.length,
            &codec,
            &job.ext,
            part.path(),
        );
        run_ffmpeg(
            runner,
            &paths,
            &argv,
            None,
            job.length,
            (0, 100),
            &progress,
            &cancel,
            timeout,
        )
        .await?;
        let (done, out_info) = self.0.finish(&paths, part, &job.output, &cancel).await?;
        let mut text = done_text(&done, out_info.as_ref(), None);
        if job.mode == CutMode::Fast {
            text.push_str(" Вырезано без перекодирования: границы взяты по ближайшим ключевым кадрам и могут сдвинуться на пару секунд.");
        }
        let card = files_card(&[(
            done,
            out_info.and_then(|i| i.duration_s).or(Some(job.length)),
        )]);
        Ok(ToolOutput::new(text).with_data(card))
    }
}

/// The input of `path` and what is in it, from the cache of the risk check.
async fn probe_arg(
    av: &AvEnv,
    paths: &FfmpegPaths,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<MediaInfo, ToolError> {
    let input = av.input(args, "path")?;
    av.probe(paths, &input, cancel).await
}

// ---------------------------------------------------------------- av_join

pub struct AvJoinTool(pub AvEnv);

const JOIN_MAX: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JoinMode {
    /// Equal streams: the concat demuxer copies them.
    List,
    /// Different video: scaled to the first one and encoded.
    Video,
    /// Different sound: resampled and encoded.
    Sound,
}

/// Streams equal enough to be joined without encoding. The result keeps the
/// first file's codec settings, so profile, level, frame order and the like
/// must match too; when in doubt the files are encoded.
fn same_streams(a: &MediaInfo, b: &MediaInfo) -> bool {
    let video = match (&a.video, &b.video) {
        (Some(x), Some(y)) => {
            x.codec == y.codec
                && x.width == y.width
                && x.height == y.height
                && x.rotation == y.rotation
                && x.pix_fmt == y.pix_fmt
                && x.profile == y.profile
                && x.level == y.level
                && x.has_b_frames == y.has_b_frames
                && x.codec_tag == y.codec_tag
                && x.extradata_size == y.extradata_size
                && match (x.fps, y.fps) {
                    (Some(f), Some(g)) => (f - g).abs() < 0.01,
                    (None, None) => true,
                    _ => false,
                }
        }
        (None, None) => true,
        _ => false,
    };
    let audio = match (&a.audio, &b.audio) {
        (Some(x), Some(y)) => {
            x.codec == y.codec
                && x.sample_rate == y.sample_rate
                && x.channels == y.channels
                && x.profile == y.profile
                && x.sample_fmt == y.sample_fmt
        }
        (None, None) => true,
        _ => false,
    };
    video && audio
}

fn join_mode(inputs: &[(PathBuf, MediaInfo)]) -> Result<JoinMode, ToolError> {
    let videos = inputs.iter().filter(|(_, i)| i.video.is_some()).count();
    if videos != 0 && videos != inputs.len() {
        return Err(bad("нельзя склеить видео со звуковыми файлами"));
    }
    if videos == 0 && inputs.iter().any(|(_, i)| i.audio.is_none()) {
        return Err(bad("в одном из файлов нет ни видео, ни звука"));
    }
    let (first, info) = &inputs[0];
    let same = inputs
        .iter()
        .all(|(p, i)| ext_of(p) == ext_of(first) && same_streams(info, i));
    // The copy goes back into the same container, which FFmpeg must write.
    Ok(if same && COPY_EXTS.contains(&ext_of(first).as_str()) {
        JoinMode::List
    } else if videos > 0 {
        JoinMode::Video
    } else {
        JoinMode::Sound
    })
}

struct JoinJob {
    inputs: Vec<(PathBuf, MediaInfo)>,
    mode: JoinMode,
    ext: String,
    total: f64,
    output: PathBuf,
}

impl AvJoinTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<JoinJob, ToolError> {
        let list = args
            .get("paths")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("нужен список `paths`"))?;
        if list.len() < 2 || list.len() > JOIN_MAX {
            return Err(bad(format!("склеить можно от 2 до {JOIN_MAX} файлов")));
        }
        let mut inputs = Vec::new();
        for item in list {
            let raw = item.as_str().ok_or_else(|| bad("`paths` — список путей"))?;
            let path = self.0.input_raw(raw)?;
            let info = info_of(&path)?;
            inputs.push((path, info));
        }
        let total: f64 = inputs.iter().map(|(_, i)| i.duration()).sum();
        if total > MAX_ENCODE_S {
            return Err(bad("вместе файлы длиннее 4 ч: такое склеивать не берусь"));
        }
        let mode = join_mode(&inputs)?;
        let first = &inputs[0].0;
        let ext = match mode {
            JoinMode::List => ext_of(first),
            JoinMode::Video => "mp4".to_string(),
            JoinMode::Sound => audio_ext(&ext_of(first)).to_string(),
        };
        let paths: Vec<PathBuf> = inputs.iter().map(|(p, _)| p.clone()).collect();
        let output = resolve_output(
            first,
            arg_str_opt(args, "output"),
            &format!("{} (склеено).{ext}", stem(first)),
            &ext,
            &paths,
            &self.0.user_profile,
        )?;
        Ok(JoinJob {
            inputs,
            mode,
            ext,
            total,
            output,
        })
    }
}

#[async_trait]
impl Tool for AvJoinTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "av_join",
            "Склеить несколько видео или несколько звуковых файлов по порядку в один новый файл.",
            json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Полные пути файлов в порядке склейки, от 2 до 20"
                    },
                    "output": { "type": "string", "description": OUTPUT_DESC }
                },
                "required": ["paths"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        let names: Vec<String> = job.inputs.iter().map(|(p, _)| quoted(p)).collect();
        let summary = format!(
            "Склеить {} ({}) → {}",
            names.join(", "),
            show_time(job.total),
            quoted(&job.output)
        );
        let inputs: Vec<&Path> = job.inputs.iter().map(|(p, _)| p.as_path()).collect();
        Ok(self.0.assess_io(summary, &inputs, &job.output))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let progress = progress_handle();
        let paths = self.0.ffmpeg()?;
        let mut seen = Vec::new();
        if let Some(list) = args.get("paths").and_then(Value::as_array) {
            for raw in list.iter().filter_map(Value::as_str) {
                if let Ok(p) = self.0.input_raw(raw) {
                    let info = self.0.probe(&paths, &p, &cancel).await?;
                    seen.push((p, info));
                }
            }
        }
        let job = self.prepare(&args, |p| {
            seen.iter()
                .find(|(q, _)| q == p)
                .map(|(_, i)| i.clone())
                .ok_or_else(|| bad(format!("нет файла {}", p.display())))
        })?;
        let runner = self.0.runner.as_ref();
        let work = WorkDir::new()?;
        let part = PartFile::new(&job.output);
        let (argv, timeout) = match job.mode {
            JoinMode::List => {
                let list = work.path().join("list.txt");
                let text: Vec<String> = job
                    .inputs
                    .iter()
                    .map(|(p, _)| args::concat_line(p))
                    .collect();
                std::fs::write(&list, text.join("\n") + "\n").map_err(|e| {
                    ToolError::Failed(format!("не удалось записать список файлов: {e}"))
                })?;
                (
                    args::join_list_args(&list, &job.ext, part.path()),
                    copy_timeout(job.total),
                )
            }
            JoinMode::Video => {
                let h = pick_h264(runner, &paths, &cancel).await?;
                let v = need_video(&job.inputs[0].1)?;
                let (w, hh) = ((v.width & !1).max(2), (v.height & !1).max(2));
                let audio_out = job.inputs.iter().any(|(_, i)| i.audio.is_some());
                let mut codec = h.quality_args(false, w, hh, 30.0);
                codec.extend(if audio_out { aac() } else { s(&["-an"]) });
                let inputs: Vec<JoinInput> = job
                    .inputs
                    .iter()
                    .map(|(p, i)| JoinInput {
                        path: p,
                        has_audio: i.audio.is_some(),
                        duration: i.duration(),
                    })
                    .collect();
                (
                    args::join_filter_args(
                        &inputs,
                        Some((w, hh)),
                        audio_out,
                        &codec,
                        &job.ext,
                        part.path(),
                    ),
                    encode_timeout(job.total),
                )
            }
            JoinMode::Sound => {
                let list = encoders(runner, &paths, &cancel).await?;
                let codec =
                    args::audio_codec(&job.ext, |e| list.has(e)).map_err(ToolError::Failed)?;
                let inputs: Vec<JoinInput> = job
                    .inputs
                    .iter()
                    .map(|(p, i)| JoinInput {
                        path: p,
                        has_audio: true,
                        duration: i.duration(),
                    })
                    .collect();
                (
                    args::join_filter_args(&inputs, None, true, &codec, &job.ext, part.path()),
                    encode_timeout(job.total),
                )
            }
        };
        run_ffmpeg(
            runner,
            &paths,
            &argv,
            None,
            job.total,
            (0, 100),
            &progress,
            &cancel,
            timeout,
        )
        .await?;
        let (done, out_info) = self.0.finish(&paths, part, &job.output, &cancel).await?;
        let text = done_text(&done, out_info.as_ref(), None);
        let card = files_card(&[(
            done,
            out_info.and_then(|i| i.duration_s).or(Some(job.total)),
        )]);
        Ok(ToolOutput::new(text).with_data(card))
    }
}

// ---------------------------------------------------------------- video_gif

pub struct VideoGifTool(pub AvEnv);

struct GifJob {
    input: PathBuf,
    info: MediaInfo,
    start: f64,
    length: f64,
    width: u32,
    fps: u32,
    output: PathBuf,
}

const GIF_MAX_S: f64 = 30.0;

impl VideoGifTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<GifJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let info = info_of(&input)?;
        let v = need_video(&info)?;
        let total = known_length(&info)?;
        let asked = time_arg(args, "duration")?.unwrap_or(5.0);
        if asked > GIF_MAX_S {
            return Err(bad("GIF — не длиннее 30 секунд"));
        }
        let (start, length) = span(total, time_arg(args, "start")?, None, Some(asked))?;
        let width = arg_whole(args, "width", 480, (120, 1280))?.min(v.width.max(2));
        let fps = arg_whole(args, "fps", 12, (5, 30))?;
        let output = resolve_output(
            &input,
            arg_str_opt(args, "output"),
            &format!("{}.gif", stem(&input)),
            "gif",
            std::slice::from_ref(&input),
            &self.0.user_profile,
        )?;
        Ok(GifJob {
            input,
            info,
            start,
            length,
            width,
            fps,
            output,
        })
    }
}

#[async_trait]
impl Tool for VideoGifTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "video_gif",
            "Сделать GIF из куска видео: до 30 секунд.",
            schema(
                &[
                    ("path", "string", "Полный путь к видео"),
                    (
                        "start",
                        "string",
                        "Начало: секунды или м:сс, по умолчанию 0",
                    ),
                    (
                        "duration",
                        "string",
                        "Длина в секундах, по умолчанию 5, не больше 30",
                    ),
                    (
                        "width",
                        "integer",
                        "Ширина в пикселях: от 120 до 1280, по умолчанию 480",
                    ),
                    (
                        "fps",
                        "integer",
                        "Кадров в секунду: от 5 до 30, по умолчанию 12",
                    ),
                    ("output", "string", OUTPUT_DESC),
                ],
                &["path"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        let summary = format!(
            "Сделать GIF {}–{} из {} → {}",
            show_time(job.start),
            show_time(job.start + job.length),
            about(&job.input, &job.info),
            quoted(&job.output)
        );
        Ok(self.0.assess_io(summary, &[&job.input], &job.output))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let progress = progress_handle();
        let paths = self.0.ffmpeg()?;
        let info = probe_arg(&self.0, &paths, &args, &cancel).await?;
        let job = self.prepare(&args, |_| Ok(info.clone()))?;
        let part = PartFile::new(&job.output);
        let argv = args::gif_args(
            &job.input,
            job.start,
            job.length,
            job.width,
            job.fps,
            part.path(),
        );
        run_ffmpeg(
            self.0.runner.as_ref(),
            &paths,
            &argv,
            None,
            job.length,
            (0, 100),
            &progress,
            &cancel,
            encode_timeout(job.length),
        )
        .await?;
        let (done, out_info) = self.0.finish(&paths, part, &job.output, &cancel).await?;
        let text = done_text(&done, out_info.as_ref(), None);
        let card = files_card(&[(done, Some(job.length))]);
        Ok(ToolOutput::new(text).with_data(card))
    }
}

// ---------------------------------------------------------------- video_rotate

pub struct VideoRotateTool(pub AvEnv);

struct RotateJob {
    input: PathBuf,
    info: MediaInfo,
    angle: u32,
    ext: String,
    output: PathBuf,
}

impl VideoRotateTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<RotateJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let angle = match arg_number(args, "angle")?.map(|a| a.round() as i64) {
            Some(90) => 90,
            Some(180) => 180,
            Some(270) | Some(-90) => 270,
            _ => return Err(bad("angle — 90, 180 или 270 градусов по часовой стрелке")),
        };
        let info = info_of(&input)?;
        need_video(&info)?;
        not_too_long(&info, MAX_ENCODE_S)?;
        let ext = video_ext(&ext_of(&input)).to_string();
        let output = resolve_output(
            &input,
            arg_str_opt(args, "output"),
            &format!("{} (повёрнуто).{ext}", stem(&input)),
            &ext,
            std::slice::from_ref(&input),
            &self.0.user_profile,
        )?;
        Ok(RotateJob {
            input,
            info,
            angle,
            ext,
            output,
        })
    }
}

#[async_trait]
impl Tool for VideoRotateTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "video_rotate",
            "Повернуть видео по часовой стрелке на 90, 180 или 270 градусов (270 — это 90 против часовой).",
            schema(
                &[
                    ("path", "string", "Полный путь к видео"),
                    ("angle", "integer", "Угол по часовой стрелке: 90, 180 или 270"),
                    ("output", "string", OUTPUT_DESC),
                ],
                &["path", "angle"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        let summary = format!(
            "Повернуть {} на {}° по часовой → {}",
            about(&job.input, &job.info),
            job.angle,
            quoted(&job.output)
        );
        Ok(self.0.assess_io(summary, &[&job.input], &job.output))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let progress = progress_handle();
        let paths = self.0.ffmpeg()?;
        let info = probe_arg(&self.0, &paths, &args, &cancel).await?;
        let job = self.prepare(&args, |_| Ok(info.clone()))?;
        let runner = self.0.runner.as_ref();
        let h = pick_h264(runner, &paths, &cancel).await?;
        let v = need_video(&job.info)?;
        let codec = video_codec(&job.ext, h, v, video_sound(&job.ext, &job.info));
        let part = PartFile::new(&job.output);
        let argv = args::rotate_args(&job.input, job.angle, &codec, &job.ext, part.path());
        let duration = job.info.duration();
        run_ffmpeg(
            runner,
            &paths,
            &argv,
            None,
            duration,
            (0, 100),
            &progress,
            &cancel,
            encode_timeout(duration),
        )
        .await?;
        let (done, out_info) = self.0.finish(&paths, part, &job.output, &cancel).await?;
        let text = done_text(&done, out_info.as_ref(), None);
        let card = files_card(&[(
            done,
            out_info.and_then(|i| i.duration_s).or(job.info.duration_s),
        )]);
        Ok(ToolOutput::new(text).with_data(card))
    }
}

// ---------------------------------------------------------------- av_speed

pub struct AvSpeedTool(pub AvEnv);

struct SpeedJob {
    input: PathBuf,
    info: MediaInfo,
    factor: f64,
    ext: String,
    output: PathBuf,
}

impl AvSpeedTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<SpeedJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let factor = arg_number(args, "factor")?
            .ok_or_else(|| bad("нужен `factor`: во сколько раз ускорить"))?;
        if !(0.25..=4.0).contains(&factor) || (factor - 1.0).abs() < 0.001 {
            return Err(bad(
                "factor — от 0.25 до 4 и не 1: 2 — вдвое быстрее, 0.5 — вдвое медленнее",
            ));
        }
        let info = info_of(&input)?;
        known_length(&info)?;
        not_too_long(&info, MAX_ENCODE_S)?;
        let ext = match (&info.video, &info.audio) {
            (Some(_), _) => video_ext(&ext_of(&input)).to_string(),
            (None, Some(_)) => audio_ext(&ext_of(&input)).to_string(),
            (None, None) => return Err(bad("в файле нет ни видео, ни звука")),
        };
        let output = resolve_output(
            &input,
            arg_str_opt(args, "output"),
            &format!("{} (x{}).{ext}", stem(&input), number(factor)),
            &ext,
            std::slice::from_ref(&input),
            &self.0.user_profile,
        )?;
        Ok(SpeedJob {
            input,
            info,
            factor,
            ext,
            output,
        })
    }
}

#[async_trait]
impl Tool for AvSpeedTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "av_speed",
            "Ускорить или замедлить видео или звук; голос не меняет высоту.",
            schema(
                &[
                    ("path", "string", "Полный путь к файлу"),
                    (
                        "factor",
                        "number",
                        "Во сколько раз быстрее: 2 — вдвое быстрее, 0.5 — вдвое медленнее; от 0.25 до 4",
                    ),
                    ("output", "string", OUTPUT_DESC),
                ],
                &["path", "factor"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        let how = if job.factor > 1.0 {
            "Ускорить"
        } else {
            "Замедлить"
        };
        let summary = format!(
            "{how} {} в {} раза → {}",
            about(&job.input, &job.info),
            number(if job.factor > 1.0 {
                job.factor
            } else {
                1.0 / job.factor
            }),
            quoted(&job.output)
        );
        Ok(self.0.assess_io(summary, &[&job.input], &job.output))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let progress = progress_handle();
        let paths = self.0.ffmpeg()?;
        let info = probe_arg(&self.0, &paths, &args, &cancel).await?;
        let job = self.prepare(&args, |_| Ok(info.clone()))?;
        let runner = self.0.runner.as_ref();
        let has_audio = job.info.audio.is_some();
        let codec = match &job.info.video {
            Some(v) => {
                let h = pick_h264(runner, &paths, &cancel).await?;
                video_codec(&job.ext, h, v, has_audio.then(aac))
            }
            None => {
                let list = encoders(runner, &paths, &cancel).await?;
                args::audio_codec(&job.ext, |e| list.has(e)).map_err(ToolError::Failed)?
            }
        };
        let part = PartFile::new(&job.output);
        let argv = args::speed_args(
            &job.input,
            job.factor,
            job.info.video.is_some(),
            has_audio,
            &codec,
            &job.ext,
            part.path(),
        );
        let input_len = job.info.duration();
        let out_len = input_len / job.factor;
        run_ffmpeg(
            runner,
            &paths,
            &argv,
            None,
            out_len,
            (0, 100),
            &progress,
            &cancel,
            encode_timeout(input_len.max(out_len)),
        )
        .await?;
        let (done, out_info) = self.0.finish(&paths, part, &job.output, &cancel).await?;
        let text = done_text(&done, out_info.as_ref(), None);
        let card = files_card(&[(done, out_info.and_then(|i| i.duration_s).or(Some(out_len)))]);
        Ok(ToolOutput::new(text).with_data(card))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{av, probe_json, X264_LIST};
    use super::*;
    use crate::testing::run;
    use mika_core::tools::RiskLevel;

    #[test]
    fn spans_are_checked_against_the_length() {
        assert_eq!(
            span(120.0, Some(90.0), Some(100.0), None).unwrap(),
            (90.0, 10.0)
        );
        assert_eq!(
            span(120.0, Some(90.0), None, Some(60.0)).unwrap(),
            (90.0, 30.0)
        );
        assert_eq!(span(120.0, None, None, None).unwrap(), (0.0, 120.0));
        assert_eq!(
            span(120.0, Some(10.0), Some(120.3), None).unwrap(),
            (10.0, 110.0)
        );
        assert!(span(120.0, Some(130.0), None, None).is_err());
        assert!(span(120.0, Some(10.0), Some(5.0), None).is_err());
        assert!(span(120.0, Some(10.0), Some(200.0), None).is_err());
        assert!(span(120.0, None, Some(10.0), Some(5.0)).is_err());
        assert!(span(120.0, None, None, Some(0.0)).is_err());
    }

    #[tokio::test]
    async fn cut_of_sound_copies_with_a_time_in_the_name() {
        let t = av();
        let input = t.file("песня.mp3");
        t.answer(&probe_json("mp3", 200.0, 4_000_000, None, Some("mp3")));
        let tool = AvCutTool(t.av());
        let args = json!({ "path": input.display().to_string(), "start": "1:30", "end": "2:00" });
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert!(
            a.summary.starts_with("Вырезать 1:30–2:00 из «песня.mp3»"),
            "{}",
            a.summary
        );
        let out = run(&tool, args).await.unwrap();
        let result = t.dir.path().join("песня (фрагмент 01.30-02.00).mp3");
        assert!(result.is_file(), "{}", out.text);
        let runs = t.ffmpeg_runs();
        assert_eq!(runs.len(), 1);
        let l = runs[0].join(" ");
        assert!(
            l.contains("-ss 90.000 -t 30.000 -protocol_whitelist file -i file:"),
            "{l}"
        );
        assert!(l.contains("-c copy -n file:"), "{l}");
        assert_eq!(out.data.unwrap()["files"][0]["duration_s"], 30.0);
    }

    #[tokio::test]
    async fn cut_of_video_encodes_unless_fast() {
        let t = av();
        let input = t.file("v.mp4");
        t.answer(&probe_json(
            "mov,mp4",
            100.0,
            4_000_000,
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        t.answer(X264_LIST);
        let tool = AvCutTool(t.av());
        let args = json!({ "path": input.display().to_string(), "duration": "10" });
        run(&tool, args).await.unwrap();
        let l = t.ffmpeg_runs()[1].join(" ");
        assert!(
            l.contains("-t 10.000 -protocol_whitelist") && !l.contains("-ss "),
            "{l}"
        );
        assert!(l.contains("-c:v libx264 -preset medium -crf 23 -pix_fmt yuv420p -c:a aac -b:a 160k -movflags +faststart"), "{l}");

        let fast = json!({ "path": input.display().to_string(), "start": 50, "fast": true });
        let out = run(&tool, fast).await.unwrap();
        assert!(out.text.contains("ключевым кадрам"));
        let l = t.ffmpeg_runs().last().unwrap().join(" ");
        assert!(l.contains("-c copy -avoid_negative_ts make_zero"), "{l}");
        assert!(l.contains("v (фрагмент 00.50-01.40).mika-part.mp4"), "{l}");
        assert!(t.dir.path().join("v (фрагмент 00.50-01.40).mp4").is_file());

        let whole = json!({ "path": input.display().to_string() });
        assert!(
            matches!(tool.assess(&whole), Err(ToolError::BadArguments(m)) if m.contains("весь файл"))
        );
    }

    #[tokio::test]
    async fn equal_files_join_by_a_list_and_different_ones_encode() {
        let t = av();
        let a = t.file("a.mp4");
        let b = t.file("b.mp4");
        let same = probe_json(
            "mov,mp4",
            10.0,
            1_000_000,
            Some(("h264", 1280, 720)),
            Some("aac"),
        );
        t.answer(&same);
        t.answer(&same);
        let tool = AvJoinTool(t.av());
        let args = json!({ "paths": [a.display().to_string(), b.display().to_string()] });
        let assessed = tool.assess(&args).unwrap();
        assert_eq!(
            assessed.summary,
            "Склеить «a.mp4», «b.mp4» (0:20) → «a (склеено).mp4»"
        );
        run(&tool, args).await.unwrap();
        let l = t.ffmpeg_runs()[0].join(" ");
        assert!(
            l.contains("-f concat -safe 0 -protocol_whitelist file -i file:"),
            "{l}"
        );
        assert!(l.contains("-c copy"), "{l}");
        assert!(t.dir.path().join("a (склеено).mp4").is_file());

        let c = t.file("c.mkv");
        t.answer(&probe_json(
            "matroska",
            5.0,
            1_000_000,
            Some(("vp9", 640, 360)),
            None,
        ));
        t.answer(X264_LIST);
        let args = json!({ "paths": [a.display().to_string(), c.display().to_string()], "output": "вместе" });
        let out = run(&tool, args).await.unwrap();
        assert!(t.dir.path().join("вместе.mp4").is_file(), "{}", out.text);
        let runs = t.ffmpeg_runs();
        let l = runs.last().unwrap().join(" ");
        assert!(l.contains("concat=n=2:v=1:a=1[v][a]"), "{l}");
        assert!(
            l.contains("anullsrc=r=48000:cl=stereo,atrim=duration=5.000[a1]"),
            "{l}"
        );
    }

    #[test]
    fn join_refuses_video_with_sound_files() {
        let v = super::super::probe::parse_probe(&probe_json(
            "mov,mp4",
            10.0,
            1,
            Some(("h264", 2, 2)),
            None,
        ))
        .unwrap();
        let a = super::super::probe::parse_probe(&probe_json("mp3", 10.0, 1, None, Some("mp3")))
            .unwrap();
        assert!(join_mode(&[("a.mp4".into(), v.clone()), ("b.mp3".into(), a.clone())]).is_err());
        assert_eq!(
            join_mode(&[("a.mp3".into(), a.clone()), ("b.mp3".into(), a.clone())]).unwrap(),
            JoinMode::List
        );
        assert_eq!(
            join_mode(&[("a.mp3".into(), a.clone()), ("b.wav".into(), a.clone())]).unwrap(),
            JoinMode::Sound
        );
        // FFmpeg reads APE but cannot write it: encoded.
        assert_eq!(
            join_mode(&[("a.ape".into(), a.clone()), ("b.ape".into(), a.clone())]).unwrap(),
            JoinMode::Sound
        );
        // Same size and codec, other encoder settings: encoded.
        let mut other = v.clone();
        if let Some(s) = other.video.as_mut() {
            s.profile = "Main".into();
        }
        assert_eq!(
            join_mode(&[("a.mp4".into(), v.clone()), ("b.mp4".into(), v.clone())]).unwrap(),
            JoinMode::List
        );
        assert_eq!(
            join_mode(&[("a.mp4".into(), v.clone()), ("b.mp4".into(), other)]).unwrap(),
            JoinMode::Video
        );
        let mut he = a.clone();
        if let Some(s) = he.audio.as_mut() {
            s.profile = "HE-AAC".into();
        }
        assert_eq!(
            join_mode(&[("a.mp3".into(), a), ("b.mp3".into(), he)]).unwrap(),
            JoinMode::Sound
        );
    }

    #[tokio::test]
    async fn cut_of_a_format_ffmpeg_cannot_write_is_encoded() {
        let t = av();
        let input = t.file("альбом.ape");
        t.answer(&probe_json("ape", 300.0, 30_000_000, None, Some("ape")));
        t.answer(X264_LIST);
        let tool = AvCutTool(t.av());
        let args =
            json!({ "path": input.display().to_string(), "start": "1:00", "duration": "30" });
        run(&tool, args).await.unwrap();
        let l = t.ffmpeg_runs().last().unwrap().join(" ");
        assert!(l.contains("-c:a aac -b:a 192k"), "{l}");
        assert!(
            t.dir
                .path()
                .join("альбом (фрагмент 01.00-01.30).m4a")
                .is_file(),
            "{l}"
        );
    }

    #[tokio::test]
    async fn gif_rotate_and_speed_build_their_filters() {
        let t = av();
        let input = t.file("v.mov");
        t.answer(&probe_json(
            "mov,mp4",
            60.0,
            4_000_000,
            Some(("h264", 1920, 1080)),
            Some("aac"),
        ));
        let gif = VideoGifTool(t.av());
        let out = run(
            &gif,
            json!({ "path": input.display().to_string(), "start": 10, "duration": "3" }),
        )
        .await
        .unwrap();
        assert!(t.dir.path().join("v.gif").is_file(), "{}", out.text);
        assert!(t.ffmpeg_runs()[0].join(" ").contains("-ss 10.000 -t 3.000"));
        assert!(gif
            .assess(&json!({ "path": input.display().to_string(), "duration": 31 }))
            .is_err());

        t.answer(X264_LIST);
        let rotate = VideoRotateTool(t.av());
        let args = json!({ "path": input.display().to_string(), "angle": 90 });
        assert!(rotate
            .assess(&args)
            .unwrap()
            .summary
            .contains("на 90° по часовой"));
        run(&rotate, args).await.unwrap();
        assert!(t.dir.path().join("v (повёрнуто).mov").is_file());
        let l = t.ffmpeg_runs().last().unwrap().join(" ");
        assert!(l.contains("-vf transpose=1 -c:v libx264"), "{l}");
        assert!(l.contains("-c:a copy"), "{l}");
        assert!(rotate
            .assess(&json!({ "path": input.display().to_string(), "angle": 45 }))
            .is_err());

        let speed = AvSpeedTool(t.av());
        let args = json!({ "path": input.display().to_string(), "factor": "x2" });
        assert!(speed
            .assess(&args)
            .unwrap()
            .summary
            .starts_with("Ускорить «v.mov»"));
        let out = run(&speed, args).await.unwrap();
        assert!(t.dir.path().join("v (x2).mov").is_file(), "{}", out.text);
        let l = t.ffmpeg_runs().last().unwrap().join(" ");
        assert!(l.contains("[0:v]setpts=PTS/2[v];[0:a]atempo=2[a]"), "{l}");
        assert!(speed
            .assess(&json!({ "path": input.display().to_string(), "factor": 1 }))
            .is_err());
        assert!(speed
            .assess(&json!({ "path": input.display().to_string(), "factor": 8 }))
            .is_err());
    }
}
