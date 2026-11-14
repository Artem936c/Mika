//! Sound processing (`audio_process`) and subtitles burnt into a video
//! (`video_subtitles`).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{
    arg_str, arg_str_opt, progress_handle, spec, Assessment, Tool, ToolError, ToolOutput,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::args::{self, audio_ext, fits, s, video_ext, Denoise, Gain};
use super::encoders::{encoders, pick_h264, webm_args};
use super::job::{encode_timeout, run_ffmpeg};
use super::output::{ext_of, resolve_output, stem, PartFile, WorkDir};
use super::probe::check_subtitles;
use super::wav::{parse_loudnorm, parse_silences};
use super::{
    aac, about, bad, done_text, files_card, need_audio, need_video, not_too_long, quoted, AvEnv,
    MediaInfo, MAX_ENCODE_S,
};
use crate::util::{arg_bool, schema};

const OUTPUT_DESC: &str = "Имя или путь нового файла; без него — рядом с исходным";

/// `+6dB`, `-3 dB`, `6 дБ` within ±30 dB, or `150%` within 10..400 %.
pub(crate) fn parse_volume(raw: &str) -> Result<Gain, ToolError> {
    let wrong = || bad("volume — например +6dB, -3dB или 150%");
    let t = raw.trim().to_lowercase().replace(',', ".").replace(' ', "");
    if let Some(p) = t.strip_suffix('%') {
        let p: f64 = p.parse().map_err(|_| wrong())?;
        if !(10.0..=400.0).contains(&p) {
            return Err(bad("громкость в процентах — от 10% до 400%"));
        }
        return Ok(Gain::Times(p / 100.0));
    }
    let db = t
        .strip_suffix("db")
        .or_else(|| t.strip_suffix("дб"))
        .ok_or_else(wrong)?;
    let d: f64 = db.trim_start_matches('+').parse().map_err(|_| wrong())?;
    if !(-30.0..=30.0).contains(&d) || d == 0.0 {
        return Err(bad("громкость в децибелах — от -30dB до +30dB, не ноль"));
    }
    Ok(Gain::Db(d))
}

/// Where speech starts and ends, with 150 ms to spare: `None` when the edges
/// hold no silence.
pub(crate) fn trim_edges(
    silences: &[(f64, Option<f64>)],
    total: f64,
) -> Result<Option<(f64, f64)>, ToolError> {
    const MARGIN: f64 = 0.15;
    let mut start = 0.0;
    let mut end = total;
    if let Some((s, e)) = silences.first() {
        if *s <= 0.05 {
            match e {
                Some(e) if *e < total - 0.05 => start = *e,
                _ => return Err(bad("в файле одна тишина")),
            }
        }
    }
    if let Some((s, e)) = silences.last() {
        if e.is_none_or(|e| e >= total - 0.05) && *s > start {
            end = *s;
        }
    }
    let from = (start - MARGIN).max(0.0);
    let to = (end + MARGIN).min(total);
    Ok((from > 0.01 || to < total - 0.01).then_some((from, to)))
}

pub struct AudioProcessTool(pub AvEnv);

struct ProcessJob {
    input: PathBuf,
    info: MediaInfo,
    normalize: bool,
    trim: bool,
    denoise: Denoise,
    gain: Option<Gain>,
    ext: String,
    output: PathBuf,
}

impl ProcessJob {
    fn steps(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.trim {
            v.push("убрать тишину по краям");
        }
        match self.denoise {
            Denoise::Off => {}
            Denoise::Normal => v.push("убрать шум"),
            Denoise::Strong => v.push("сильно убрать шум"),
        }
        if self.gain.is_some() {
            v.push("изменить громкость");
        }
        if self.normalize {
            v.push("выровнять громкость");
        }
        v
    }
}

impl AudioProcessTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<ProcessJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let normalize = arg_bool(args, "normalize");
        let trim = arg_bool(args, "trim_silence");
        let denoise = match arg_str_opt(args, "denoise").map(str::trim) {
            None | Some("") | Some("off") => Denoise::Off,
            Some("normal") => Denoise::Normal,
            Some("strong") => Denoise::Strong,
            Some(d) => return Err(bad(format!("denoise — off, normal или strong, а не «{d}»"))),
        };
        let gain = match arg_str_opt(args, "volume").map(str::trim) {
            None | Some("") => None,
            Some(v) => Some(parse_volume(v)?),
        };
        if !normalize && !trim && denoise == Denoise::Off && gain.is_none() {
            return Err(bad(
                "укажи, что сделать: normalize, trim_silence, denoise или volume",
            ));
        }
        if normalize && gain.is_some() {
            return Err(bad("normalize и volume вместе не задаются: выбери одно"));
        }
        let info = info_of(&input)?;
        need_audio(&info)?;
        if trim && info.video.is_some() {
            return Err(bad(
                "тишину по краям убираю только у звуковых файлов; для видео используй av_cut",
            ));
        }
        not_too_long(&info, MAX_ENCODE_S)?;
        let ext = if info.video.is_some() {
            video_ext(&ext_of(&input))
        } else {
            audio_ext(&ext_of(&input))
        }
        .to_string();
        let output = resolve_output(
            &input,
            arg_str_opt(args, "output"),
            &format!("{} (обработано).{ext}", stem(&input)),
            &ext,
            std::slice::from_ref(&input),
            &self.0.user_profile,
        )?;
        Ok(ProcessJob {
            input,
            info,
            normalize,
            trim,
            denoise,
            gain,
            ext,
            output,
        })
    }
}

#[async_trait]
impl Tool for AudioProcessTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "audio_process",
            "Обработать звук файла: выровнять громкость (normalize), убрать тишину по краям (trim_silence, только для звуковых файлов), убрать шум (denoise), изменить громкость (volume: +6dB, -3dB или 150%). Видео остаётся как есть. Результат — новый файл.",
            schema(
                &[
                    ("path", "string", "Полный путь к звуковому файлу или видео"),
                    ("normalize", "boolean", "Выровнять громкость по стандарту EBU R128"),
                    ("trim_silence", "boolean", "Убрать тишину в начале и в конце"),
                    ("denoise", "string", "one of: off, normal, strong"),
                    (
                        "volume",
                        "string",
                        "Изменить громкость: +6dB, -3dB (от -30 до +30) или 150% (от 10% до 400%)",
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
            "Обработать звук {}: {} → {}",
            about(&job.input, &job.info),
            job.steps().join(", "),
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
        self.exec(args, cancel).await
    }
}

impl AudioProcessTool {
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
        let runner = self.0.runner.as_ref();
        let total = job.info.duration();
        let timeout = encode_timeout(total);
        let (detect_w, measure_w, final_from) = match (job.trim, job.normalize) {
            (true, true) => ((0, 15), (15, 50), 50),
            (true, false) => ((0, 20), (0, 0), 20),
            (false, true) => ((0, 0), (0, 40), 40),
            (false, false) => ((0, 0), (0, 0), 0),
        };

        let mut opts = Vec::new();
        let mut kept = total;
        if job.trim {
            let argv = args::silence_args(&job.input, "-50dB", "0.3");
            let out = run_ffmpeg(
                "av_audio", runner, &paths, &argv, None, total, detect_w, &progress, &cancel,
                timeout,
            )
            .await?;
            match trim_edges(&parse_silences(&out.stderr), total)? {
                Some((from, to)) => {
                    if from > 0.0 {
                        opts.extend(["-ss".to_string(), super::time::arg_time(from)]);
                    }
                    if to < total {
                        opts.extend(["-to".to_string(), super::time::arg_time(to)]);
                    }
                    kept = to - from;
                }
                None if job.steps().len() == 1 => {
                    return Ok(ToolOutput::new("Тишины по краям нет, файл не изменён."));
                }
                None => {}
            }
        }

        let list = encoders(runner, &paths, &cancel).await?;
        let has = |e: &str| list.has(e);
        let mut codec = Vec::new();
        if let Some(v) = &job.info.video {
            if fits(&job.ext, &v.codec, true) {
                codec.extend(s(&["-c:v", "copy"]));
            } else if job.ext == "webm" {
                let mut w = webm_args();
                w.truncate(w.len() - 4);
                codec.extend(w);
            } else {
                let h = pick_h264(runner, &paths, &cancel).await?;
                codec.extend(h.quality_args(false, v.width, v.height, v.fps.unwrap_or(30.0)));
            }
            if job.ext == "webm" {
                codec.extend(args::audio_codec("webm", has).map_err(ToolError::Failed)?);
            } else {
                codec.extend(aac());
            }
        } else {
            codec.extend(args::audio_codec(&job.ext, has).map_err(ToolError::Failed)?);
        }

        let filters = args::audio_filters(job.denoise, job.gain);
        let measured = if job.normalize {
            let argv = args::loudnorm_measure_args(&job.input, &opts, &filters);
            let out = run_ffmpeg(
                "av_audio", runner, &paths, &argv, None, kept, measure_w, &progress, &cancel,
                timeout,
            )
            .await?;
            Some(parse_loudnorm(&out.stderr).ok_or_else(|| {
                ToolError::Failed("не удалось измерить громкость: возможно, в файле тишина".into())
            })?)
        } else {
            None
        };
        let part = PartFile::new(&job.output);
        let argv = args::audio_pass_args(
            &job.input,
            &opts,
            &filters,
            measured.as_ref(),
            &codec,
            &job.ext,
            part.path(),
        );
        run_ffmpeg(
            "av_audio",
            runner,
            &paths,
            &argv,
            None,
            kept,
            (final_from, 100),
            &progress,
            &cancel,
            timeout,
        )
        .await?;
        let (done, out_info) = self.0.finish(&paths, part, &job.output, &cancel).await?;
        let text = done_text(&done, out_info.as_ref(), None);
        let card = files_card(&[(done, out_info.and_then(|i| i.duration_s).or(Some(kept)))]);
        Ok(ToolOutput::new(text).with_data(card))
    }
}

// ---------------------------------------------------------------- video_subtitles

pub struct VideoSubtitlesTool(pub AvEnv);

const SUBTITLE_EXTS: &[&str] = &["srt", "ass", "ssa", "vtt"];

/// Subtitles as UTF-8 without a BOM, since the filter reads them as UTF-8:
/// UTF-16 with a BOM is decoded, and text that is not UTF-8 is taken for
/// Windows-1251, as Russian subtitles often are.
pub(crate) fn subtitles_utf8(bytes: &[u8]) -> Result<String, ToolError> {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        crate::docs::plain_text(bytes).ok_or_else(|| bad("в файле субтитров не текст"))?
    };
    Ok(text.trim_start_matches('\u{FEFF}').to_string())
}

struct SubtitlesJob {
    input: PathBuf,
    info: MediaInfo,
    subtitles: PathBuf,
    sub_ext: String,
    font_size: u32,
    output: PathBuf,
}

impl VideoSubtitlesTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<SubtitlesJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let subtitles = self.0.input_raw(arg_str(args, "subtitles")?)?;
        let sub_ext = ext_of(&subtitles);
        if !SUBTITLE_EXTS.contains(&sub_ext.as_str()) {
            return Err(bad("субтитры — файл .srt, .ass, .ssa или .vtt"));
        }
        let font_size = match args.get("font_size") {
            None | Some(Value::Null) => 22,
            Some(v) => {
                let n = v
                    .as_f64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    .ok_or_else(|| bad("font_size — число"))?;
                if !(10.0..=72.0).contains(&n) {
                    return Err(bad("font_size — от 10 до 72"));
                }
                n.round() as u32
            }
        };
        let info = info_of(&input)?;
        need_video(&info)?;
        not_too_long(&info, MAX_ENCODE_S)?;
        let output = resolve_output(
            &input,
            arg_str_opt(args, "output"),
            &format!("{} (субтитры).mp4", stem(&input)),
            "mp4",
            &[input.clone(), subtitles.clone()],
            &self.0.user_profile,
        )?;
        Ok(SubtitlesJob {
            input,
            info,
            subtitles,
            sub_ext,
            font_size,
            output,
        })
    }
}

#[async_trait]
impl Tool for VideoSubtitlesTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "video_subtitles",
            "Вшить субтитры из файла .srt, .ass, .ssa или .vtt в изображение видео. Результат — новый файл mp4.",
            schema(
                &[
                    ("path", "string", "Полный путь к видео"),
                    ("subtitles", "string", "Полный путь к файлу субтитров"),
                    ("font_size", "integer", "Размер шрифта: от 10 до 72, по умолчанию 22"),
                    ("output", "string", OUTPUT_DESC),
                ],
                &["path", "subtitles"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        let summary = format!(
            "Вшить субтитры {} в {} → {}",
            quoted(&job.subtitles),
            about(&job.input, &job.info),
            quoted(&job.output)
        );
        Ok(self
            .0
            .assess_io(summary, &[&job.input, &job.subtitles], &job.output))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
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
        let work = WorkDir::new()?;
        let name = format!("mika_subs.{}", job.sub_ext);
        let copy = work.path().join(&name);
        let bytes = std::fs::read(&job.subtitles)
            .map_err(|e| ToolError::Failed(format!("не удалось прочитать субтитры: {e}")))?;
        std::fs::write(&copy, subtitles_utf8(&bytes)?)
            .map_err(|e| ToolError::Failed(format!("не удалось скопировать субтитры: {e}")))?;
        check_subtitles(runner, &paths, &copy, &cancel).await?;
        let h = pick_h264(runner, &paths, &cancel).await?;
        let v = need_video(&job.info)?;
        let mut codec = h.quality_args(false, v.width, v.height, v.fps.unwrap_or(30.0));
        match &job.info.audio {
            Some(a) if matches!(a.codec.as_str(), "aac" | "mp3") => {
                codec.extend(s(&["-c:a", "copy"]))
            }
            Some(_) => codec.extend(aac()),
            None => codec.push("-an".into()),
        }
        let filter = args::subtitles_filter(&name, &job.sub_ext, job.font_size);
        let part = PartFile::new(&job.output);
        let argv = args::subtitles_args(&job.input, &filter, &codec, part.path());
        let duration = job.info.duration();
        run_ffmpeg(
            "av_audio",
            runner,
            &paths,
            &argv,
            Some(work.path()),
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

#[cfg(test)]
mod tests {
    use super::super::testkit::{av, probe_json, X264_LIST};
    use super::*;
    use crate::testing::run;
    use serde_json::json;

    #[test]
    fn volume_is_read_in_decibels_or_percent() {
        assert_eq!(parse_volume("+6dB").unwrap(), Gain::Db(6.0));
        assert_eq!(parse_volume("-3 dB").unwrap(), Gain::Db(-3.0));
        assert_eq!(parse_volume("4,5 дБ").unwrap(), Gain::Db(4.5));
        assert_eq!(parse_volume("150%").unwrap(), Gain::Times(1.5));
        for bad in ["40dB", "0dB", "5%", "500%", "громче", "6"] {
            assert!(parse_volume(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn edges_of_silence_are_cut_with_a_margin() {
        let s = [(0.0, Some(1.25)), (4.5, Some(5.1)), (9.75, None)];
        let (from, to) = trim_edges(&s, 10.0).unwrap().unwrap();
        assert!((from - 1.1).abs() < 1e-9 && (to - 9.9).abs() < 1e-9);
        assert_eq!(trim_edges(&[(4.5, Some(5.1))], 10.0).unwrap(), None);
        assert_eq!(trim_edges(&[], 10.0).unwrap(), None);
        assert!(trim_edges(&[(0.0, None)], 10.0).is_err());
        let tail_only = trim_edges(&[(8.0, Some(10.0))], 10.0).unwrap().unwrap();
        assert!(tail_only.0 == 0.0 && (tail_only.1 - 8.15).abs() < 1e-9);
    }

    #[tokio::test]
    async fn normalize_runs_two_loudnorm_passes() {
        let t = av();
        let input = t.file("голос.wav");
        t.answer(&probe_json("wav", 10.0, 320_000, None, Some("pcm_s16le")));
        t.answer(X264_LIST);
        t.answer_err("{\"input_i\" : \"-27.61\", \"input_tp\" : \"-4.47\", \"input_lra\" : \"18.06\", \"input_thresh\" : \"-39.20\", \"target_offset\" : \"0.58\"}");
        let tool = AudioProcessTool(t.av());
        let args =
            json!({ "path": input.display().to_string(), "normalize": true, "denoise": "normal" });
        let a = tool.assess(&args).unwrap();
        assert!(
            a.summary.contains("убрать шум, выровнять громкость"),
            "{}",
            a.summary
        );
        run(&tool, args).await.unwrap();
        let runs = t.ffmpeg_runs();
        assert_eq!(runs.len(), 3, "the list and two passes");
        let one = runs[1].join(" ");
        assert!(one.contains("highpass=f=80,afftdn=nr=12:nf=-50:tn=1,loudnorm=I=-16:TP=-1.5:LRA=11:print_format=json"), "{one}");
        let two = runs[2].join(" ");
        assert!(two.contains("measured_I=-27.61"), "{two}");
        assert!(two.contains("-ar 48000 -c:a pcm_s16le"), "{two}");
        assert!(t.dir.path().join("голос (обработано).wav").is_file());
    }

    #[tokio::test]
    async fn trim_without_silence_writes_nothing() {
        let t = av();
        let input = t.file("a.mp3");
        t.answer(&probe_json("mp3", 10.0, 160_000, None, Some("mp3")));
        t.answer_err("[silencedetect @ 0] silence_start: 4.5\n[silencedetect @ 0] silence_end: 5.1 | silence_duration: 0.6\n");
        let tool = AudioProcessTool(t.av());
        let out = run(
            &tool,
            json!({ "path": input.display().to_string(), "trim_silence": true }),
        )
        .await
        .unwrap();
        assert_eq!(out.text, "Тишины по краям нет, файл не изменён.");
        assert!(out.data.is_none());
    }

    #[tokio::test]
    async fn video_keeps_its_picture_and_trim_is_refused() {
        let t = av();
        let input = t.file("v.mp4");
        t.answer(&probe_json(
            "mov,mp4",
            10.0,
            1_000_000,
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        let tool = AudioProcessTool(t.av());
        assert!(matches!(
            tool.assess(&json!({ "path": input.display().to_string(), "trim_silence": true })),
            Err(ToolError::BadArguments(m)) if m.contains("av_cut")
        ));
        assert!(tool
            .assess(
                &json!({ "path": input.display().to_string(), "normalize": true, "volume": "+3dB" })
            )
            .is_err());
        t.answer(X264_LIST);
        run(
            &tool,
            json!({ "path": input.display().to_string(), "volume": "+6dB" }),
        )
        .await
        .unwrap();
        let l = t.ffmpeg_runs().last().unwrap().join(" ");
        assert!(
            l.contains("-af volume=6dB,alimiter=limit=0.95 -c:v copy -c:a aac -b:a 160k"),
            "{l}"
        );
    }

    #[tokio::test]
    async fn subtitles_are_read_from_the_work_folder() {
        let t = av();
        let input = t.file("v.mkv");
        let subs = t.file("v.srt");
        t.answer(&probe_json(
            "matroska",
            10.0,
            1_000_000,
            Some(("hevc", 1920, 1080)),
            Some("opus"),
        ));
        t.answer("{\"format\":{\"format_name\":\"srt\"}}");
        t.answer(X264_LIST);
        let tool = VideoSubtitlesTool(t.av());
        let args =
            json!({ "path": input.display().to_string(), "subtitles": subs.display().to_string() });
        assert!(tool
            .assess(&args)
            .unwrap()
            .summary
            .starts_with("Вшить субтитры «v.srt» в «v.mkv»"));
        run(&tool, args).await.unwrap();
        let l = t.ffmpeg_runs().last().unwrap().join(" ");
        assert!(l.contains("-vf subtitles=filename=mika_subs.srt:charenc=UTF-8:force_style='FontName=Segoe UI,FontSize=22' -c:v libx264"), "{l}");
        assert!(l.contains("-c:a aac -b:a 160k"), "{l}");
        let cwd = t.f.run_cwds.lock().last().cloned().flatten().unwrap();
        assert!(cwd
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("mika-av-"));
        assert!(t.dir.path().join("v (субтитры).mp4").is_file());
        let txt = t.file("v.txt");
        assert!(tool
            .assess(&json!({ "path": input.display().to_string(), "subtitles": txt.display().to_string() }))
            .is_err());
    }

    #[tokio::test]
    async fn subtitles_taken_for_something_else_are_refused() {
        let t = av();
        let input = t.file("v.mp4");
        let subs = t.file("movie.srt");
        t.answer(&probe_json(
            "mov,mp4",
            10.0,
            1_000_000,
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        t.answer("{\"format\":{\"format_name\":\"dash\"}}");
        let r = run(
            &VideoSubtitlesTool(t.av()),
            json!({ "path": input.display().to_string(), "subtitles": subs.display().to_string() }),
        )
        .await;
        assert!(matches!(r, Err(ToolError::BadArguments(m)) if m.contains("не читается")));
        assert!(t.ffmpeg_runs().is_empty(), "nothing was burnt");
    }

    #[test]
    fn subtitles_become_utf8() {
        // «Привет» in Windows-1251.
        let cp1251 = b"1\r\n00:00:01,000 --> 00:00:02,000\r\n\xCF\xF0\xE8\xE2\xE5\xF2\r\n";
        assert!(subtitles_utf8(cp1251).unwrap().contains("Привет"));
        let utf8 = "\u{FEFF}1\r\nПривет\r\n";
        assert_eq!(subtitles_utf8(utf8.as_bytes()).unwrap(), "1\r\nПривет\r\n");
        let mut le = vec![0xFF, 0xFE];
        let mut be = vec![0xFE, 0xFF];
        for u in "Привет".encode_utf16() {
            le.extend(u.to_le_bytes());
            be.extend(u.to_be_bytes());
        }
        assert_eq!(subtitles_utf8(&le).unwrap(), "Привет");
        assert_eq!(subtitles_utf8(&be).unwrap(), "Привет");
    }
}
