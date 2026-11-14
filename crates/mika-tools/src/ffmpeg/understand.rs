//! Understanding content: speech to text (`av_transcribe`) with the same
//! recognition provider as the microphone, and "what is in this video"
//! (`video_describe`) from frames through the vision model.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use futures::StreamExt;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{
    arg_str_opt, progress_handle, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::args;
use super::job::{encode_timeout, run_ffmpeg, FRAME_TIMEOUT};
use super::locate::path_key;
use super::output::{check_free, ext_of, stem, WorkDir};
use super::srt::{render, Cue};
use super::time::show_time;
use super::wav::{parse_silences, pcm_of, pcm_seconds, slice_wav};
use super::{
    about, bad, files_card, folder_scope, need_audio, need_video, quoted, AvEnv, MediaInfo,
    MAX_TRANSCRIBE_S,
};
use crate::sys::SpeechSegment;
use crate::util::{arg_bool, resolve_path, schema};

/// Longest piece sent to recognition, and how far back a pause is looked for.
const CHUNK_MAX: f64 = 60.0;
const CHUNK_MIN: f64 = 30.0;
/// Phrases for subtitles when the provider tells no times.
const PHRASE_MIN: f64 = 2.0;
const PHRASE_MAX: f64 = 12.0;
/// Pieces recognised at once.
const PARALLEL: usize = 3;
/// Recognition of more than this many seconds is asked as a high risk: it costs.
const PAID_LONG_S: f64 = 600.0;
/// Characters of the transcript the model gets.
const MODEL_CHARS: usize = 3000;

/// Lengths shown on the confirm cards, by file, newest last. Recognition
/// never goes past the length that was asked about: the container may tell
/// less than the file holds, or the file may grow after the card.
static ASKED: Mutex<VecDeque<(String, f64)>> = Mutex::new(VecDeque::new());
/// A folder holds up to a hundred files, and its job may wait in the queue
/// behind others.
const ASKED_KEPT: usize = 256;

/// Also called for a job that waited in the queue, right before its work:
/// the entry of its card may have left the list by then.
pub(crate) fn remember_asked(input: &Path, seconds: f64) {
    let key = path_key(input);
    let mut list = ASKED.lock();
    list.retain(|(k, _)| *k != key);
    list.push_back((key, seconds));
    while list.len() > ASKED_KEPT {
        list.pop_front();
    }
}

/// Drops the length of `input` from the list, as later cards of other files
/// do in time.
#[cfg(test)]
pub(crate) fn forget_asked(input: &Path) {
    let key = path_key(input);
    ASKED.lock().retain(|(k, _)| *k != key);
}

pub(crate) fn asked_length(input: &Path) -> Option<f64> {
    let key = path_key(input);
    ASKED
        .lock()
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, s)| *s)
}

/// Pieces of at most a minute, cut in the middle of a pause when one is
/// found in the second half of the minute.
pub(crate) fn plan_chunks(total: f64, pauses: &[f64]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut start = 0.0;
    while start < total - 0.05 {
        if total - start <= CHUNK_MAX {
            out.push((start, total));
            break;
        }
        let limit = start + CHUNK_MAX;
        let cut = pauses
            .iter()
            .copied()
            .filter(|p| *p >= start + CHUNK_MIN && *p <= limit)
            .fold(None, |best: Option<f64>, p| {
                Some(best.map_or(p, |b| b.max(p)))
            })
            .unwrap_or(limit);
        out.push((start, cut));
        start = cut;
    }
    out
}

/// Phrases between pauses: at least 2 s, longer ones split at 12 s.
pub(crate) fn plan_phrases(total: f64, pauses: &[f64]) -> Vec<(f64, f64)> {
    let mut bounds: Vec<f64> = pauses
        .iter()
        .copied()
        .filter(|p| *p > 0.0 && *p < total)
        .collect();
    bounds.sort_by(f64::total_cmp);
    bounds.push(total);
    let mut merged: Vec<(f64, f64)> = Vec::new();
    let mut start = 0.0;
    for b in bounds {
        if b - start >= PHRASE_MIN {
            merged.push((start, b));
            start = b;
        }
    }
    if start < total - 0.05 {
        match merged.last_mut() {
            Some(last) => last.1 = total,
            None => merged.push((start, total)),
        }
    }
    let mut out = Vec::new();
    for (a, b) in merged {
        let parts = ((b - a) / PHRASE_MAX).ceil().max(1.0) as usize;
        let step = (b - a) / parts as f64;
        for i in 0..parts {
            out.push((
                a + step * i as f64,
                if i + 1 == parts {
                    b
                } else {
                    a + step * (i + 1) as f64
                },
            ));
        }
    }
    out
}

/// How pieces are recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Text only, minute pieces.
    Text,
    /// Minute pieces with phrase times from the provider.
    Segments,
    /// Short phrases, each one a cue.
    Phrases,
}

/// Text of one piece and its cues, times from the start of the file.
#[derive(Debug, Clone, Default)]
struct Heard {
    text: String,
    cues: Vec<Cue>,
}

pub struct AvTranscribeTool(pub AvEnv);

struct TranscribeJob {
    input: PathBuf,
    info: MediaInfo,
    srt: bool,
    txt_path: PathBuf,
    srt_path: Option<PathBuf>,
}

fn with_ext(base: &Path, ext: &str) -> PathBuf {
    PathBuf::from(format!("{}.{ext}", base.display()))
}

impl AvTranscribeTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<TranscribeJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let srt = arg_bool(args, "srt");
        let info = info_of(&input)?;
        need_audio(&info)?;
        if info.duration() <= 0.0 {
            return Err(bad("не удалось узнать длительность файла"));
        }
        if info.duration() > MAX_TRANSCRIBE_S {
            return Err(bad("файл длиннее 3 ч: такой распознавать не берусь"));
        }
        let folder = input.parent().unwrap_or(Path::new("")).to_path_buf();
        let raw = arg_str_opt(args, "output")
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let (mut base, explicit) = match raw {
            None => (folder.join(stem(&input)), false),
            Some(r) if !r.contains(['\\', '/', ':']) => (folder.join(r.trim_matches('"')), true),
            Some(r) => {
                let p = resolve_path(r, &self.0.user_profile);
                if p.is_dir() {
                    // A folder names no file: a taken name is counted up.
                    (p.join(stem(&input)), false)
                } else {
                    (p, true)
                }
            }
        };
        if matches!(ext_of(&base).as_str(), "txt" | "srt") {
            base = base.with_extension("");
        }
        let exts: &[&str] = if srt { &["txt", "srt"] } else { &["txt"] };
        if !explicit && exts.iter().any(|e| with_ext(&base, e).exists()) {
            let name = stem(&base);
            let parent = base.parent().unwrap_or(Path::new("")).to_path_buf();
            base = (2..1000)
                .map(|i| parent.join(format!("{name} ({i})")))
                .find(|b| exts.iter().all(|e| !with_ext(b, e).exists()))
                .unwrap_or(base);
        }
        let txt_path = with_ext(&base, "txt");
        check_free(&txt_path, std::slice::from_ref(&input))?;
        let srt_path = if srt {
            let p = with_ext(&base, "srt");
            check_free(&p, std::slice::from_ref(&input))?;
            Some(p)
        } else {
            None
        };
        Ok(TranscribeJob {
            input,
            info,
            srt,
            txt_path,
            srt_path,
        })
    }

    /// The piece from `from` to `to` seconds, tried twice; with its start.
    async fn recognize_at(
        &self,
        mode: Mode,
        pcm: &[u8],
        from: f64,
        to: f64,
    ) -> (f64, Result<Heard, String>) {
        let wav = slice_wav(pcm, from, to);
        let mut last = String::new();
        for _ in 0..2 {
            match self.recognize_once(mode, &wav, from, to).await {
                Ok(h) => return (from, Ok(h)),
                Err(e) => last = e,
            }
        }
        (from, Err(last))
    }

    async fn recognize_once(
        &self,
        mode: Mode,
        wav: &[u8],
        from: f64,
        to: f64,
    ) -> Result<Heard, String> {
        let whole = |text: String| {
            let text = text.trim().to_string();
            let cues = if text.is_empty() {
                Vec::new()
            } else {
                vec![Cue {
                    start: from,
                    end: to,
                    text: text.clone(),
                }]
            };
            Heard { text, cues }
        };
        match mode {
            Mode::Text | Mode::Phrases => Ok(whole(self.0.stt.transcribe(wav).await?)),
            Mode::Segments => match self.0.stt.transcribe_segments(wav).await? {
                Some(segments) => Ok(from_segments(&segments, from, to)),
                None => Ok(whole(self.0.stt.transcribe(wav).await?)),
            },
        }
    }
}

fn from_segments(segments: &[SpeechSegment], from: f64, to: f64) -> Heard {
    let cues: Vec<Cue> = segments
        .iter()
        .map(|s| Cue {
            start: (from + s.start).min(to),
            end: (from + s.end.max(s.start)).min(to),
            text: s.text.trim().to_string(),
        })
        .filter(|c| !c.text.is_empty())
        .collect();
    let text = cues
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Heard { text, cues }
}

/// Writes a new file, never over an existing one.
fn write_new(path: &Path, text: &str) -> Result<(), ToolError> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| ToolError::Failed(format!("не удалось записать {}: {e}", path.display())))?;
    f.write_all(text.as_bytes())
        .map_err(|e| ToolError::Failed(format!("не удалось записать {}: {e}", path.display())))
}

#[async_trait]
impl Tool for AvTranscribeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "av_transcribe",
            "Расшифровать речь из видео или звукового файла в текст (.txt), по желанию и в субтитры (.srt). Звук уходит провайдеру распознавания речи, это платно.",
            schema(
                &[
                    ("path", "string", "Полный путь к файлу"),
                    ("srt", "boolean", "true — ещё и субтитры .srt с временем фраз"),
                    (
                        "output",
                        "string",
                        "Имя без расширения или папка для результата; без него — рядом с исходным",
                    ),
                ],
                &["path"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        let mut names = quoted(&job.txt_path);
        if let Some(s) = &job.srt_path {
            names.push_str(&format!(" и {}", quoted(s)));
        }
        let summary = format!(
            "Расшифровать речь из {} → {names}",
            about(&job.input, &job.info)
        );
        let mut a = self.0.assess_io(summary, &[&job.input], &job.txt_path);
        const PAID: &str = "звук уйдёт провайдеру распознавания, это платно";
        if a.level == RiskLevel::Medium {
            a.reason = PAID.into();
        }
        let length = job.info.duration();
        remember_asked(&job.input, length);
        if length > PAID_LONG_S {
            a = a.at_least(RiskLevel::High, PAID);
            a.summary = format!(
                "Распознать {} мин звука из {} — это платно",
                (length / 60.0).ceil(),
                quoted(&job.input)
            );
        }
        Ok(a)
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

impl AvTranscribeTool {
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
        // No more than the card said, whatever the file holds now.
        let total = asked_length(&job.input)
            .unwrap_or(f64::INFINITY)
            .min(job.info.duration());
        let limit = total + 1.0;
        let work = WorkDir::new()?;
        let wav_path = work.path().join("audio.wav");
        let argv = args::wav_args(&job.input, limit, &wav_path);
        run_ffmpeg(
            runner,
            &paths,
            &argv,
            None,
            total,
            (0, 7),
            &progress,
            &cancel,
            encode_timeout(total),
        )
        .await?;
        let argv = args::silence_args(&wav_path, "-35dB", "0.35");
        let out = run_ffmpeg(
            runner,
            &paths,
            &argv,
            None,
            total,
            (7, 10),
            &progress,
            &cancel,
            encode_timeout(total),
        )
        .await?;
        let pauses: Vec<f64> = parse_silences(&out.stderr)
            .into_iter()
            .filter_map(|(s, e)| e.map(|e| (s + e) / 2.0))
            .collect();
        let bytes = std::fs::read(&wav_path)
            .map_err(|e| ToolError::Failed(format!("звук не извлёкся: {e}")))?;
        let pcm = pcm_of(&bytes).map_err(ToolError::Failed)?;
        let length = pcm_seconds(pcm).min(limit);

        let chunks = plan_chunks(length, &pauses);
        let (mode, pieces, first) = if !job.srt {
            (Mode::Text, chunks, None)
        } else {
            let Some(&(a, b)) = chunks.first() else {
                return Ok(ToolOutput::new(
                    "Речи в файле не найдено, ничего не записано.",
                ));
            };
            let wav = slice_wav(pcm, a, b);
            let probe = tokio::select! {
                _ = cancel.cancelled() => return Err(ToolError::Cancelled),
                r = self.0.stt.transcribe_segments(&wav) => r,
            };
            match probe {
                Ok(Some(segments)) => {
                    (Mode::Segments, chunks, Some(from_segments(&segments, a, b)))
                }
                Ok(None) => (Mode::Phrases, plan_phrases(length, &pauses), None),
                // The first piece is tried again as an ordinary one.
                Err(_) => (Mode::Segments, chunks, None),
            }
        };

        let count = pieces.len().max(1);
        let skip = usize::from(first.is_some());
        let mut heard: Vec<Heard> = first.into_iter().collect();
        let mut failure: Option<(f64, String)> = None;
        {
            let jobs = pieces
                .iter()
                .skip(skip)
                .map(|&(a, b)| (a, b))
                .collect::<Vec<(f64, f64)>>()
                .into_iter()
                .map(move |(a, b): (f64, f64)| self.recognize_at(mode, pcm, a, b));
            let mut stream = futures::stream::iter(jobs).buffered(PARALLEL);
            loop {
                let next = tokio::select! {
                    _ = cancel.cancelled() => return Err(ToolError::Cancelled),
                    n = stream.next() => n,
                };
                let Some((at, result)) = next else { break };
                match result {
                    Ok(h) => heard.push(h),
                    Err(e) => {
                        failure = Some((at, e));
                        break;
                    }
                }
                progress.set((10 + 90 * heard.len() / count) as u8);
            }
        }

        let texts: Vec<&str> = heard
            .iter()
            .map(|h| h.text.as_str())
            .filter(|t| !t.is_empty())
            .collect();
        if texts.is_empty() {
            return match failure {
                Some((_, e)) => Err(ToolError::Failed(format!("распознавание не удалось: {e}"))),
                None => Ok(ToolOutput::new(
                    "Речи в файле не найдено, ничего не записано.",
                )),
            };
        }
        let transcript = texts.join("\n");
        write_new(&job.txt_path, &format!("{transcript}\n"))?;
        self.0.outputs.remember(&job.txt_path);
        let mut files = vec![(job.txt_path.clone(), None)];
        if let Some(srt) = &job.srt_path {
            let cues: Vec<Cue> = heard.iter().flat_map(|h| h.cues.iter().cloned()).collect();
            write_new(srt, &render(&cues))?;
            self.0.outputs.remember(srt);
            files.push((srt.clone(), None));
        }
        let mut text = format!("Расшифровка записана: {}", job.txt_path.display());
        if let Some(srt) = &job.srt_path {
            text.push_str(&format!(" и {}", srt.display()));
        }
        text.push('.');
        if let Some((at, e)) = &failure {
            text.push_str(&format!(
                " Распознано до {}, дальше ошибка: {e}.",
                show_time(*at)
            ));
        }
        let shown: String = transcript.chars().take(MODEL_CHARS).collect();
        let more = if shown.len() < transcript.len() {
            "…"
        } else {
            ""
        };
        text.push_str(&format!("\n\n{shown}{more}"));
        Ok(ToolOutput::new(text).with_data(files_card(&files)))
    }
}

// ---------------------------------------------------------------- video_describe

pub struct VideoDescribeTool(pub AvEnv);

const DEFAULT_QUESTION: &str = "Что происходит в этом видео?";

struct DescribeJob {
    input: PathBuf,
    info: MediaInfo,
    frames: usize,
    question: String,
}

/// Times of `n` frames spread over the video: in the middle of equal parts.
pub(crate) fn frame_times(total: f64, n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| total * (i as f64 + 0.5) / n as f64)
        .collect()
}

impl VideoDescribeTool {
    fn prepare(
        &self,
        args: &Value,
        info_of: impl Fn(&Path) -> Result<MediaInfo, ToolError>,
    ) -> Result<DescribeJob, ToolError> {
        let input = self.0.input(args, "path")?;
        let frames = match args.get("frames") {
            None | Some(Value::Null) => 6,
            Some(v) => {
                let n = v
                    .as_f64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    .ok_or_else(|| bad("frames — число"))?;
                if !(2.0..=12.0).contains(&n) {
                    return Err(bad("frames — от 2 до 12"));
                }
                n.round() as usize
            }
        };
        let question = arg_str_opt(args, "question")
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .unwrap_or(DEFAULT_QUESTION)
            .to_string();
        let info = info_of(&input)?;
        need_video(&info)?;
        if info.duration() <= 0.0 {
            return Err(bad("не удалось узнать длительность видео"));
        }
        Ok(DescribeJob {
            input,
            info,
            frames,
            question,
        })
    }
}

#[async_trait]
impl Tool for VideoDescribeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "video_describe",
            "Понять, что в видеофайле: несколько кадров по всей длине уходят модели зрения вместе с вопросом. Звук не слушает: для речи — av_transcribe.",
            schema(
                &[
                    ("path", "string", "Полный путь к видео"),
                    ("question", "string", "Вопрос о видео; по умолчанию — что в нём происходит"),
                    ("frames", "integer", "Сколько кадров взять: от 2 до 12, по умолчанию 6"),
                ],
                &["path"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let paths = self.0.ffmpeg()?;
        let job = self.prepare(args, |p| self.0.probe_now(&paths, p))?;
        Ok(Assessment::new(
            RiskLevel::Medium,
            format!(
                "Показать модели зрения {} кадров из {}",
                job.frames,
                about(&job.input, &job.info)
            ),
            job.input.display().to_string(),
            "кадры уйдут провайдеру зрения",
        )
        .with_scope(folder_scope(&job.input)))
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
        let work = WorkDir::new()?;
        let times = frame_times(job.info.duration(), job.frames);
        let mut pngs = Vec::new();
        let mut shown = Vec::new();
        for (i, t) in times.iter().enumerate() {
            let out = work.path().join(format!("frame-{:02}.png", i + 1));
            let argv = args::frame_args(&job.input, *t, &out);
            let window = (
                (i * 80 / times.len()) as u8,
                ((i + 1) * 80 / times.len()) as u8,
            );
            run_ffmpeg(
                self.0.runner.as_ref(),
                &paths,
                &argv,
                None,
                0.0,
                window,
                &progress,
                &cancel,
                FRAME_TIMEOUT,
            )
            .await?;
            match std::fs::read(&out) {
                Ok(png) if !png.is_empty() => {
                    shown.push(format!("{} — {}", pngs.len() + 1, show_time(*t)));
                    pngs.push(png);
                }
                _ => log::warn!("no frame at {t:.1} s"),
            }
        }
        if pngs.is_empty() {
            return Err(ToolError::Failed(
                "не удалось взять ни одного кадра из видео".into(),
            ));
        }
        let question = format!("{}\nКадры по порядку: {}", job.question, shown.join(", "));
        let answer = tokio::select! {
            _ = cancel.cancelled() => return Err(ToolError::Cancelled),
            a = self.0.vision.ask_frames(&pngs, &question) => a.map_err(ToolError::Failed)?,
        };
        progress.set(100);
        Ok(ToolOutput::new(answer))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{av, probe_json};
    use super::super::wav::tests::wav_of;
    use super::*;
    use crate::testing::run;
    use serde_json::json;

    #[test]
    fn chunks_end_in_pauses_within_the_minute() {
        assert_eq!(plan_chunks(50.0, &[]), vec![(0.0, 50.0)]);
        assert_eq!(
            plan_chunks(130.0, &[20.0, 45.0, 55.0, 100.0]),
            vec![(0.0, 55.0), (55.0, 100.0), (100.0, 130.0)]
        );
        assert_eq!(
            plan_chunks(130.0, &[10.0]),
            vec![(0.0, 60.0), (60.0, 120.0), (120.0, 130.0)],
            "a hard cut without a pause"
        );
    }

    #[test]
    fn phrases_are_merged_and_split() {
        assert_eq!(
            plan_phrases(10.0, &[0.5, 1.0, 3.0, 9.5]),
            vec![(0.0, 3.0), (3.0, 10.0)]
        );
        let long = plan_phrases(30.0, &[]);
        assert_eq!(long, vec![(0.0, 10.0), (10.0, 20.0), (20.0, 30.0)]);
        assert_eq!(plan_phrases(1.0, &[]), vec![(0.0, 1.0)]);
    }

    #[test]
    fn frames_are_spread_over_the_video() {
        assert_eq!(frame_times(60.0, 3), vec![10.0, 30.0, 50.0]);
    }

    /// A video of `seconds` whose sound FFmpeg would extract: the fake
    /// writes a WAV of that length to every output.
    fn video(t: &super::super::testkit::Av, name: &str, seconds: f64) -> PathBuf {
        let input = t.file(name);
        t.answer(&probe_json(
            "mov,mp4",
            seconds,
            10_000_000,
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        *t.f.run_write.lock() = Some(wav_of(seconds));
        input
    }

    #[tokio::test]
    async fn text_is_recognised_by_minutes_and_written() {
        let t = av();
        let input = video(&t, "лекция.mp4", 130.0);
        *t.f.stt_text.lock() = "Привет.".into();
        // The wav run, then pauses in the second half of the first minute.
        t.answer("");
        t.answer_err("silence_start: 50\nsilence_end: 51 | silence_duration: 1\n");
        let tool = AvTranscribeTool(t.av());
        let args = json!({ "path": input.display().to_string() });
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert_eq!(a.reason, "звук уйдёт провайдеру распознавания, это платно");
        let out = run(&tool, args).await.unwrap();
        let txt = t.dir.path().join("лекция.txt");
        assert_eq!(
            std::fs::read_to_string(&txt).unwrap(),
            "Привет.\nПривет.\nПривет.\n"
        );
        // Pieces: 0–50.5, 50.5–110.5, 110.5–130.
        let mut wavs = t.f.stt_wavs.lock().clone();
        wavs.sort();
        assert_eq!(wavs, vec![44 + 624_000, 44 + 1_616_000, 44 + 1_920_000]);
        assert!(out
            .text
            .starts_with(&format!("Расшифровка записана: {}.", txt.display())));
        assert!(out.text.ends_with("Привет.\nПривет.\nПривет."));
        assert_eq!(
            out.data.as_ref().unwrap()["files"][0]["path"],
            txt.display().to_string()
        );
        assert!(out.data.unwrap()["files"][0].get("duration_s").is_none());
        assert!(t.av().outputs.contains(&txt));
    }

    #[tokio::test]
    async fn subtitles_use_segment_times_when_the_provider_gives_them() {
        let t = av();
        let input = video(&t, "v.mp4", 70.0);
        *t.f.stt_segments.lock() = Some(vec![SpeechSegment {
            start: 1.0,
            end: 2.0,
            text: "Раз.".into(),
        }]);
        t.answer("");
        t.answer_err("");
        let tool = AvTranscribeTool(t.av());
        let out = run(
            &tool,
            json!({ "path": input.display().to_string(), "srt": true }),
        )
        .await
        .unwrap();
        let srt = std::fs::read_to_string(t.dir.path().join("v.srt")).unwrap();
        assert_eq!(
            srt,
            "1\r\n00:00:01,000 --> 00:00:02,000\r\nРаз.\r\n\r\n2\r\n00:01:01,000 --> 00:01:02,000\r\nРаз.\r\n\r\n"
        );
        assert_eq!(out.data.unwrap()["files"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn subtitles_fall_back_to_phrases_without_times() {
        let t = av();
        let input = video(&t, "v.mp4", 5.0);
        *t.f.stt_text.lock() = "Фраза.".into();
        t.answer("");
        t.answer_err("silence_start: 2.4\nsilence_end: 2.6 | silence_duration: 0.2\n");
        let tool = AvTranscribeTool(t.av());
        run(
            &tool,
            json!({ "path": input.display().to_string(), "srt": true }),
        )
        .await
        .unwrap();
        let srt = std::fs::read_to_string(t.dir.path().join("v.srt")).unwrap();
        assert!(
            srt.contains("00:00:00,000 --> 00:00:02,500\r\nФраза."),
            "{srt}"
        );
        assert!(
            srt.contains("00:00:02,500 --> 00:00:05,000\r\nФраза."),
            "{srt}"
        );
    }

    #[tokio::test]
    async fn one_failure_is_retried_and_a_second_keeps_the_part_heard() {
        const PAUSE: &str = "silence_start: 50\nsilence_end: 51 | silence_duration: 1\n";
        let t = av();
        let input = video(&t, "v.mp4", 130.0);
        *t.f.stt_text.lock() = "Есть.".into();
        t.answer("");
        t.answer_err(PAUSE);
        // The minute 0:50–1:50 never works.
        *t.f.stt_fail_len.lock() = Some(44 + 1_920_000);
        let out = run(
            &AvTranscribeTool(t.av()),
            json!({ "path": input.display().to_string() }),
        )
        .await
        .unwrap();
        assert!(
            out.text
                .contains("Распознано до 0:50, дальше ошибка: сбой распознавания."),
            "{}",
            out.text
        );
        assert_eq!(
            std::fs::read_to_string(t.dir.path().join("v.txt")).unwrap(),
            "Есть.\n"
        );

        let t = av();
        let input = video(&t, "w.mp4", 30.0);
        *t.f.stt_text.lock() = "Есть.".into();
        t.answer("");
        t.answer_err("");
        *t.f.stt_errors.lock() = 1;
        let out = run(
            &AvTranscribeTool(t.av()),
            json!({ "path": input.display().to_string() }),
        )
        .await
        .unwrap();
        assert!(!out.text.contains("ошибка"), "{}", out.text);

        let t = av();
        let input = video(&t, "z.mp4", 30.0);
        t.answer("");
        t.answer_err("");
        *t.f.stt_errors.lock() = 2;
        let r = run(
            &AvTranscribeTool(t.av()),
            json!({ "path": input.display().to_string() }),
        )
        .await;
        assert!(matches!(r, Err(ToolError::Failed(m)) if m.contains("сбой распознавания")));
        assert!(!t.dir.path().join("z.txt").exists());
    }

    #[tokio::test]
    async fn recognition_stops_at_the_length_on_the_card() {
        let t = av();
        let input = video(&t, "v.mp4", 70.0);
        // The container says 70 s; the sound inside is longer.
        *t.f.run_write.lock() = Some(wav_of(200.0));
        *t.f.stt_text.lock() = "Да.".into();
        t.answer("");
        t.answer_err("");
        run(
            &AvTranscribeTool(t.av()),
            json!({ "path": input.display().to_string() }),
        )
        .await
        .unwrap();
        let wav = t.ffmpeg_runs()[0].join(" ");
        assert!(wav.contains("-t 71.000 -protocol_whitelist"), "{wav}");
        assert_eq!(t.f.stt_wavs.lock().len(), 2, "0:00–1:00 and 1:00–1:11 only");
    }

    #[tokio::test]
    async fn transcribe_scope_names_the_folder_read() {
        let t = av();
        let input = video(&t, "v.mp4", 30.0);
        let out = tempfile::tempdir().unwrap();
        let a = AvTranscribeTool(t.av())
            .assess(&json!({
                "path": input.display().to_string(),
                "output": out.path().display().to_string()
            }))
            .unwrap();
        assert_eq!(
            a.scope,
            format!(
                "{} -> {}",
                t.dir.path().display().to_string().to_lowercase(),
                out.path().display().to_string().to_lowercase()
            )
        );
    }

    #[tokio::test]
    async fn long_recognition_is_a_high_risk() {
        let t = av();
        let input = t.file("long.mp3");
        t.answer(&probe_json("mp3", 1200.0, 20_000_000, None, Some("mp3")));
        let a = AvTranscribeTool(t.av())
            .assess(&json!({ "path": input.display().to_string() }))
            .unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(
            a.summary,
            "Распознать 20 мин звука из «long.mp3» — это платно"
        );
    }

    #[tokio::test]
    async fn describe_sends_the_frames_with_their_times() {
        let t = av();
        let input = t.file("v.mp4");
        t.answer(&probe_json(
            "mov,mp4",
            60.0,
            1_000_000,
            Some(("h264", 1280, 720)),
            None,
        ));
        *t.f.run_write.lock() = Some(b"\x89PNG".to_vec());
        let tool = VideoDescribeTool(t.av());
        let args = json!({ "path": input.display().to_string(), "frames": 3 });
        let a = tool.assess(&args).unwrap();
        assert_eq!(
            (a.level, a.reason.as_str()),
            (RiskLevel::Medium, "кадры уйдут провайдеру зрения")
        );
        let out = run(&tool, args).await.unwrap();
        assert_eq!(
            out.text,
            "вижу 3 кадров, вопрос: Что происходит в этом видео?\nКадры по порядку: 1 — 0:10, 2 — 0:30, 3 — 0:50"
        );
        assert_eq!(*t.f.vision_frames.lock(), vec![3]);
        let runs = t.ffmpeg_runs();
        assert_eq!(runs.len(), 3);
        assert!(runs[1].join(" ").contains("-ss 30.000"));
        assert!(out.data.is_none());
        assert!(tool
            .assess(&json!({ "path": input.display().to_string(), "frames": 20 }))
            .is_err());
    }
}
