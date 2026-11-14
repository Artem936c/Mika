//! Long calls of the video and sound tools run as background jobs
//! (decision 0032).
//!
//! The wrapper changes nothing about the risk: the call is assessed and
//! asked about by the tool itself, and the approval the gate gave goes into
//! the job with the call.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use mika_core::bgjobs::{run_tool_in_background, started_output};
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{Assessment, Tool, ToolError, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::args::{COPY_EXTS, VIDEO_EXTS};
use super::convert::plan_convert;
use super::output::ext_of;
use super::time::parse_time;
use super::understand::{asked_length, remember_asked};
use super::{AvEnv, MediaInfo};

/// Encoding this many seconds of video or sound, or more, goes to the background.
pub(crate) const ENCODE_FROM_S: f64 = 180.0;
/// Recognition of this many seconds of speech, or more, goes to the background.
pub(crate) const TRANSCRIBE_FROM_S: f64 = 600.0;

/// The argument that says where the work runs.
const BACKGROUND: &str = "background";
/// The scope of a call that only checks and writes nothing.
const CHECK_SCOPE: &str = "check:";

/// What tells a file from another one put in its place: its size and the
/// time it was last written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stamp {
    pub path: PathBuf,
    /// `None` when the file cannot be looked at.
    seen: Option<(u64, Option<SystemTime>)>,
}

impl Stamp {
    pub(crate) fn of(path: &Path) -> Self {
        let seen = std::fs::metadata(path)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| (m.len(), m.modified().ok()));
        Self {
            path: path.to_path_buf(),
            seen,
        }
    }

    /// The file is there and is the one that was looked at.
    pub(crate) fn holds(&self) -> bool {
        self.seen.is_some() && *self == Self::of(&self.path)
    }
}

/// Why a file of a waiting job is not worked on.
pub(crate) const CHANGED_FILE: &str = "файл изменился после подтверждения";

/// A call as it was approved. A job may wait in the queue for hours, and the
/// approval was given for the files that were there then: a file that was
/// changed or replaced since is not worked on, and recognition takes no more
/// sound than the card said.
struct Approved {
    inner: Arc<dyn Tool>,
    files: Vec<Stamp>,
    /// Lengths the card of a recognition showed, by file.
    asked: Vec<(PathBuf, f64)>,
}

#[async_trait]
impl Tool for Approved {
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        self.inner.assess(args)
    }

    async fn run(
        &self,
        args: Value,
        approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        if let Some(changed) = self.files.iter().find(|f| !f.holds()) {
            let name = changed
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Err(ToolError::Failed(format!(
                "{CHANGED_FILE}: «{name}». Работа не начата; чтобы сделать её с новым файлом, вызови инструмент заново"
            )));
        }
        // The list of lengths is short and other cards may have pushed
        // these out of it while the job waited.
        for (path, seconds) in &self.asked {
            remember_asked(path, *seconds);
        }
        self.inner.run(args, approval, cancel).await
    }
}

/// A tool whose long calls run as background jobs.
pub struct InBackground {
    inner: Arc<dyn Tool>,
    av: AvEnv,
}

/// Wraps `tool`: its long calls answer at once and go on in the queue of `av`.
pub fn in_background(tool: impl Tool + 'static, av: &AvEnv) -> InBackground {
    InBackground {
        inner: Arc::new(tool),
        av: av.clone(),
    }
}

/// `true` or `false`, written as a flag or as a word.
fn flag(args: &Value, key: &str) -> Option<bool> {
    match args.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A time argument in seconds.
fn seconds(args: &Value, key: &str) -> Option<f64> {
    match args.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => parse_time(s).ok(),
        _ => None,
    }
}

/// The call without the argument of the wrapper.
fn without_background(mut args: Value) -> Value {
    if let Value::Object(map) = &mut args {
        map.remove(BACKGROUND);
    }
    args
}

impl InBackground {
    /// What is in the file named by `path`; the look of `assess` is remembered.
    fn info(&self, args: &Value) -> Option<(std::path::PathBuf, MediaInfo)> {
        let paths = self.av.ffmpeg().ok()?;
        let input = self.av.input(args, "path").ok()?;
        let info = self.av.probe_now(&paths, &input).ok()?;
        Some((input, info))
    }

    /// Whether the work is long enough to leave the turn; `None` when the
    /// call cannot be read: the tool itself then says what is wrong.
    fn long(&self, args: &Value) -> Option<bool> {
        let name = self.inner.spec().name;
        if name == "av_join" {
            let paths = self.av.ffmpeg().ok()?;
            let mut total = 0.0;
            for item in args.get("paths")?.as_array()? {
                let input = self.av.input_raw(item.as_str()?).ok()?;
                total += self.av.probe_now(&paths, &input).ok()?.duration();
            }
            return Some(total >= ENCODE_FROM_S);
        }
        let (input, info) = self.info(args)?;
        let length = info.duration();
        Some(match name.as_str() {
            "av_transcribe" => length >= TRANSCRIBE_FROM_S,
            "av_cut" => {
                cut_encodes(args, &input, &info) && cut_length(args, length) >= ENCODE_FROM_S
            }
            "av_convert" => {
                let format = args
                    .get("format")?
                    .as_str()?
                    .trim()
                    .trim_start_matches('.')
                    .to_lowercase();
                plan_convert(&ext_of(&input), &info, &format)
                    .ok()?
                    .encodes()
                    && length >= ENCODE_FROM_S
            }
            _ => length >= ENCODE_FROM_S,
        })
    }

    /// The files the call reads, as far as they can be told from its arguments.
    fn inputs(&self, args: &Value) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if let Ok(path) = self.av.input(args, "path") {
            files.push(path);
        }
        let listed = args.get("paths").and_then(Value::as_array);
        let single = args.get("subtitles").and_then(Value::as_str);
        let raw = listed
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .chain(single);
        for item in raw {
            if let Ok(path) = self.av.input_raw(item) {
                files.push(path);
            }
        }
        files
    }

    /// Whether this call runs as a background job.
    fn goes_background(&self, args: &Value) -> bool {
        match flag(args, BACKGROUND) {
            Some(asked) => asked,
            None => self.long(args).unwrap_or(false),
        }
    }
}

/// A cut that encodes: a fast cut of a video and a cut of a sound file copy
/// the streams and take seconds.
fn cut_encodes(args: &Value, input: &Path, info: &MediaInfo) -> bool {
    let ext = ext_of(input);
    if info.video.is_some() {
        !(flag(args, "fast") == Some(true) && VIDEO_EXTS.contains(&ext.as_str()))
    } else {
        !COPY_EXTS.contains(&ext.as_str())
    }
}

/// Length of the piece `av_cut` is asked for; the whole file when no time is given.
fn cut_length(args: &Value, total: f64) -> f64 {
    let start = seconds(args, "start").unwrap_or(0.0);
    let length = match (seconds(args, "end"), seconds(args, "duration")) {
        (Some(end), _) => end.min(total) - start,
        (None, Some(d)) => d.min(total - start),
        (None, None) => total - start,
    };
    length.max(0.0)
}

#[async_trait]
impl Tool for InBackground {
    fn spec(&self) -> ToolSpec {
        let mut spec = self.inner.spec();
        spec.description
            .push_str(" Долгая работа идёт в фоне: в чате появится карточка с ходом.");
        if let Some(props) = spec
            .parameters
            .get_mut("properties")
            .and_then(Value::as_object_mut)
        {
            props.insert(
                BACKGROUND.into(),
                json!({
                    "type": "boolean",
                    "description": "true — выполнить в фоне и сразу ответить; false — дождаться конца. Без него длинная работа сама уходит в фон"
                }),
            );
        }
        spec
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let mut a = self.inner.assess(args)?;
        if !a.scope.starts_with(CHECK_SCOPE) && self.goes_background(args) {
            a.reason = format!("{}; работа пойдёт в фоне", a.reason);
        }
        Ok(a)
    }

    async fn run(
        &self,
        args: Value,
        approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let background = self.goes_background(&args);
        let args = without_background(args);
        if !background {
            return self.inner.run(args, approval, cancel).await;
        }
        // The title of the job is what the confirm card said.
        let a = self.inner.assess(&args)?;
        if a.scope.starts_with(CHECK_SCOPE) {
            return self.inner.run(args, approval, cancel).await;
        }
        // What was approved is fixed now: the job may start much later.
        let inputs = self.inputs(&args);
        let asked = inputs
            .iter()
            .filter(|_| self.inner.spec().name == "av_transcribe")
            .filter_map(|p| asked_length(p).map(|s| (p.clone(), s)))
            .collect();
        let approved = Arc::new(Approved {
            inner: self.inner.clone(),
            files: inputs.iter().map(|p| Stamp::of(p)).collect(),
            asked,
        });
        match run_tool_in_background(&self.av.jobs, approved, args, approval, a.summary) {
            Ok(submitted) => Ok(started_output(&submitted)),
            Err(e) => Err(ToolError::Failed(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mika_core::bgjobs::{JobCtx, JobSpec, JobState, MAX_QUEUED, STARTED_PREFIX};
    use mika_core::tools::{RiskLevel, ToolOutput};
    use serde_json::json;

    use super::super::testkit::{av, probe_json, Av, X264_LIST};
    use super::super::{
        locate, AudioProcessTool, AvConvertTool, AvCutTool, AvJoinTool, AvSpeedTool,
        AvTranscribeTool, VideoCompressTool, VideoRotateTool, VideoSubtitlesTool, MISSING_TEXT,
    };
    use super::*;

    const WAIT: Duration = Duration::from_secs(10);
    const MB: u64 = 1024 * 1024;

    /// A file that ffprobe has already looked at and found `probe` in: later
    /// looks come from the cache, in whatever order the test asks.
    fn media(t: &Av, name: &str, probe: &str) -> String {
        let input = t.file(name);
        t.answer(probe);
        let env = t.av();
        env.probe_now(&env.ffmpeg().unwrap(), &input).unwrap();
        input.display().to_string()
    }

    /// A video with H.264 and AAC of `seconds`.
    fn video(t: &Av, name: &str, seconds: f64) -> String {
        let probe = probe_json(
            "mov,mp4",
            seconds,
            200 * MB,
            Some(("h264", 1920, 1080)),
            Some("aac"),
        );
        media(t, name, &probe)
    }

    fn sound(t: &Av, name: &str, seconds: f64) -> String {
        media(
            t,
            name,
            &probe_json("mp3", seconds, 5 * MB, None, Some("mp3")),
        )
    }

    #[test]
    fn the_argument_wins_over_the_estimate() {
        let t = av();
        let tool = in_background(VideoCompressTool(t.av()), &t.av());
        let short = video(&t, "short.mp4", 20.0);
        let long = video(&t, "long.mp4", 3600.0);
        let call = |path: &str, background: Value| json!({ "path": path, "quality": "normal", "background": background });
        assert!(tool.goes_background(&call(&short, json!(true))));
        assert!(tool.goes_background(&call(&short, json!("true"))));
        assert!(!tool.goes_background(&call(&long, json!(false))));
        assert!(!tool.goes_background(&call(&long, json!("false"))));
        // Not a flag: the estimate decides.
        assert!(tool.goes_background(&call(&long, json!("да"))));
        assert!(!tool.goes_background(&call(&short, Value::Null)));
        // A call the tool cannot read stays in the turn and fails there.
        assert!(!tool.goes_background(&json!({ "quality": "normal" })));
        assert!(!tool.goes_background(&json!({ "path": "C:\\нет\\такого.mp4" })));
    }

    #[test]
    fn long_encoding_goes_out_and_quick_work_stays() {
        let t = av();
        let env = t.av();
        let compress = in_background(VideoCompressTool(env.clone()), &env);
        let ten = video(&t, "ten.mp4", 600.0);
        let two = video(&t, "two.mp4", 120.0);
        let edge = video(&t, "edge.mp4", 180.0);
        assert!(compress.goes_background(&json!({ "path": ten, "target_mb": 50 })));
        assert!(!compress.goes_background(&json!({ "path": two, "target_mb": 50 })));
        assert!(compress.goes_background(&json!({ "path": edge, "quality": "strong" })));
        for tool in [
            in_background(VideoRotateTool(env.clone()), &env),
            in_background(AvSpeedTool(env.clone()), &env),
            in_background(AudioProcessTool(env.clone()), &env),
            in_background(VideoSubtitlesTool(env.clone()), &env),
        ] {
            let name = tool.spec().name;
            assert!(tool.goes_background(&json!({ "path": ten })), "{name}");
            assert!(!tool.goes_background(&json!({ "path": two })), "{name}");
        }

        // A conversion that only copies the streams takes seconds.
        let convert = in_background(AvConvertTool(env.clone()), &env);
        let mkv = media(
            &t,
            "film.mkv",
            &probe_json(
                "matroska,webm",
                7200.0,
                900 * MB,
                Some(("h264", 1920, 1080)),
                Some("aac"),
            ),
        );
        assert!(!convert.goes_background(&json!({ "path": mkv, "format": "mp4" })));
        assert!(
            convert.goes_background(&json!({ "path": mkv, "format": "webm" })),
            "VP9 is encoded"
        );
        assert!(
            convert.goes_background(&json!({ "path": mkv, "format": ".MP3" })),
            "the sound is encoded"
        );
        assert!(!convert.goes_background(&json!({ "path": two, "format": "webm" })));
        assert!(!convert.goes_background(&json!({ "path": mkv })));

        // Recognition waits for ten minutes of speech.
        let transcribe = in_background(AvTranscribeTool(env.clone()), &env);
        let talk = sound(&t, "talk.mp3", 1200.0);
        let memo = sound(&t, "memo.mp3", 300.0);
        assert!(transcribe.goes_background(&json!({ "path": talk })));
        assert!(!transcribe.goes_background(&json!({ "path": memo })));
        assert!(!transcribe.goes_background(&json!({ "path": edge })));
    }

    #[test]
    fn a_cut_goes_out_by_the_length_of_the_piece() {
        let t = av();
        let env = t.av();
        let cut = in_background(AvCutTool(env.clone()), &env);
        let film = video(&t, "film.mp4", 3600.0);
        let call = |extra: Value| {
            let mut args = json!({ "path": film });
            for (k, v) in extra.as_object().unwrap() {
                args[k] = v.clone();
            }
            args
        };
        assert!(cut.goes_background(&call(json!({ "start": "10:00", "end": "20:00" }))));
        assert!(!cut.goes_background(&call(json!({ "start": "10:00", "end": "11:00" }))));
        assert!(cut.goes_background(&call(json!({ "start": 60, "duration": 300 }))));
        assert!(!cut.goes_background(&call(json!({ "duration": "2:59" }))));
        assert!(
            cut.goes_background(&call(json!({ "start": "30:00" }))),
            "to the end"
        );
        assert!(!cut.goes_background(&call(json!({ "start": "58:00" }))));
        assert!(
            !cut.goes_background(&call(
                json!({ "start": "10:00", "end": "50:00", "fast": true })
            )),
            "a fast cut copies the streams"
        );
        // A sound file is cut without encoding.
        let song = sound(&t, "song.mp3", 3600.0);
        assert!(!cut.goes_background(&json!({ "path": song, "end": "30:00" })));
        assert_eq!(cut_length(&json!({ "start": 50, "end": 20 }), 100.0), 0.0);
    }

    #[test]
    fn a_join_goes_out_by_the_sum_of_its_files() {
        let t = av();
        let env = t.av();
        let join = in_background(AvJoinTool(env.clone()), &env);
        let a = video(&t, "a.mp4", 100.0);
        let b = video(&t, "b.mp4", 100.0);
        let c = video(&t, "c.mp4", 30.0);
        assert!(join.goes_background(&json!({ "paths": [a, b] })));
        assert!(!join.goes_background(&json!({ "paths": [a, c] })));
        assert!(!join.goes_background(&json!({ "paths": "a.mp4" })));
        assert!(!join.goes_background(&json!({})));
    }

    #[test]
    fn the_card_changes_only_in_its_reason() {
        let t = av();
        let env = t.av();
        let plain = VideoCompressTool(env.clone());
        let wrapped = in_background(VideoCompressTool(env.clone()), &env);
        let long = video(&t, "long.mp4", 751.0);
        let args = json!({ "path": long, "target_mb": 50 });
        let was = plain.assess(&args).unwrap();
        let now = wrapped.assess(&args).unwrap();
        assert_eq!(
            (now.level, &now.summary, &now.target, &now.scope),
            (was.level, &was.summary, &was.target, &was.scope)
        );
        assert_eq!(now.level, RiskLevel::Medium);
        assert_eq!(now.reason, format!("{}; работа пойдёт в фоне", was.reason));

        // A call that waits says nothing about the background.
        let waits = json!({ "path": long, "target_mb": 50, "background": false });
        assert_eq!(wrapped.assess(&waits).unwrap(), was);
        let short = video(&t, "short.mp4", 60.0);
        let args = json!({ "path": short, "quality": "normal" });
        assert_eq!(wrapped.assess(&args).unwrap(), plain.assess(&args).unwrap());

        // A high risk stays high: recognition of twenty minutes is paid.
        let transcribe = in_background(AvTranscribeTool(env.clone()), &env);
        let talk = sound(&t, "talk.mp3", 1200.0);
        let a = transcribe.assess(&json!({ "path": talk })).unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(
            a.reason,
            "звук уйдёт провайдеру распознавания, это платно; работа пойдёт в фоне"
        );
    }

    #[test]
    fn the_schema_gains_one_argument() {
        let t = av();
        let env = t.av();
        for (tool, inner) in [
            (
                in_background(VideoCompressTool(env.clone()), &env),
                VideoCompressTool(env.clone()).spec(),
            ),
            (
                in_background(AvJoinTool(env.clone()), &env),
                AvJoinTool(env.clone()).spec(),
            ),
        ] {
            let spec = tool.spec();
            assert_eq!(spec.name, inner.name);
            assert_eq!(
                spec.description,
                format!(
                    "{} Долгая работа идёт в фоне: в чате появится карточка с ходом.",
                    inner.description
                )
            );
            assert_eq!(
                spec.parameters["properties"]["background"]["type"],
                "boolean"
            );
            assert_eq!(spec.parameters["required"], inner.parameters["required"]);
            let mut props = spec.parameters["properties"].as_object().unwrap().clone();
            props.remove("background");
            assert_eq!(Value::Object(props), inner.parameters["properties"]);
        }
    }

    #[tokio::test]
    async fn a_long_call_answers_at_once_and_ends_with_the_tool_s_card() {
        let t = av();
        let env = t.av();
        let tool = in_background(VideoCompressTool(env.clone()), &env);
        let input = video(&t, "отпуск.mp4", 751.0);
        let args = json!({ "path": input, "quality": "normal" });
        let a = tool.assess(&args).unwrap();
        // The encoder list, the encoding, the look at the result.
        t.answer(X264_LIST);
        t.answer("out_time_us=375500000\nprogress=end");
        t.answer(&probe_json(
            "mov,mp4",
            751.0,
            48 * MB,
            Some(("h264", 1920, 1080)),
            Some("aac"),
        ));
        let mut events = t.env.shared.bus.subscribe();
        let out = tool
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        assert!(out.text.starts_with(STARTED_PREFIX), "{}", out.text);
        assert!(
            out.text.contains(&format!("«{}»", a.summary)),
            "{}",
            out.text
        );
        let card = out.data.unwrap();
        assert_eq!(card["kind"], "bg_job");
        assert_eq!(card["job"]["kind"], "video_compress");
        assert_eq!(card["job"]["title"], a.summary.as_str());
        assert_eq!(card["job"]["state"], "running");
        let id = card["job"]["id"].as_u64().unwrap();

        let ended = env.jobs.wait(id, WAIT).await.unwrap();
        assert_eq!(ended.state, JobState::Done, "{}", ended.text);
        let result = t.dir.path().join("отпуск (сжато).mp4");
        assert!(result.is_file());
        assert!(
            ended
                .text
                .starts_with(&format!("Готово: {}", result.display())),
            "{}",
            ended.text
        );
        let files = ended.data.unwrap();
        assert_eq!(files["kind"], "av_files");
        assert_eq!(files["files"][0]["path"], result.display().to_string());
        assert!(env.outputs.contains(&result));
        // The progress of FFmpeg went to the job, not to a tool call.
        let mut shares = Vec::new();
        while let Ok(e) = events.try_recv() {
            match e {
                mika_core::Event::BgJobUpdated { job } => shares.push(job.share),
                mika_core::Event::ToolProgress { .. } => panic!("no call shows this progress"),
                _ => {}
            }
        }
        assert!(shares.contains(&50), "{shares:?}");
        assert_eq!(shares.last(), Some(&100));
    }

    #[tokio::test]
    async fn a_short_call_and_a_call_told_to_wait_run_in_the_turn() {
        let t = av();
        let env = t.av();
        let tool = in_background(VideoCompressTool(env.clone()), &env);
        let short = video(&t, "short.mp4", 60.0);
        t.answer(X264_LIST);
        let out = tool
            .run(
                json!({ "path": short, "quality": "normal" }),
                Approval::for_tests(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(out.text.starts_with("Готово: "), "{}", out.text);
        assert_eq!(out.data.unwrap()["kind"], "av_files");

        let long = video(&t, "long.mp4", 3600.0);
        let out = tool
            .run(
                json!({ "path": long, "quality": "normal", "background": false }),
                Approval::for_tests(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(out.text.starts_with("Готово: "), "{}", out.text);
        assert!(env.jobs.list().is_empty(), "no job was made");
    }

    #[tokio::test]
    async fn a_short_call_goes_out_when_asked() {
        let t = av();
        let env = t.av();
        let tool = in_background(AvConvertTool(env.clone()), &env);
        let input = t.file("clip.mkv");
        t.answer(&probe_json(
            "matroska,webm",
            31.0,
            3_000_000,
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        let args = json!({
            "path": input.display().to_string(), "format": "mp4", "background": true
        });
        let a = tool.assess(&args).unwrap();
        assert!(a.reason.ends_with("; работа пойдёт в фоне"), "{}", a.reason);
        let out = tool
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        let card = out.data.unwrap();
        assert_eq!(card["kind"], "bg_job");
        let id = card["job"]["id"].as_u64().unwrap();
        let ended = env.jobs.wait(id, WAIT).await.unwrap();
        assert_eq!(ended.state, JobState::Done, "{}", ended.text);
        assert!(t.dir.path().join("clip.mp4").is_file());
        // The wrapper's own argument never reaches FFmpeg.
        assert!(t
            .ffmpeg_runs()
            .iter()
            .all(|run| !run.iter().any(|a| a.contains("background"))));
    }

    /// Takes the queue until the returned token is cancelled.
    fn busy_queue(env: &AvEnv) -> CancellationToken {
        let release = CancellationToken::new();
        let gate = release.clone();
        let hold = move |_ctx: JobCtx| -> mika_core::bgjobs::JobFuture {
            Box::pin(async move {
                gate.cancelled().await;
                Ok(ToolOutput::new("готово"))
            })
        };
        env.jobs
            .submit(JobSpec::queued("t", "занято"), Box::new(hold))
            .unwrap();
        release
    }

    #[tokio::test]
    async fn a_file_replaced_while_the_job_waited_is_not_worked_on() {
        let t = av();
        let env = t.av();
        let tool = in_background(VideoCompressTool(env.clone()), &env);
        let input = video(&t, "отпуск.mp4", 751.0);
        let args = json!({ "path": input, "quality": "normal" });
        tool.assess(&args).unwrap();
        let release = busy_queue(&env);
        let out = tool
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        let id = out.data.unwrap()["job"]["id"].as_u64().unwrap();
        assert_eq!(env.jobs.get(id).unwrap().state, JobState::Queued);

        // Another file takes the place of the approved one.
        std::fs::write(&input, b"quite another file, of another size").unwrap();
        let before = t.ffmpeg_runs().len();
        release.cancel();
        let ended = env.jobs.wait(id, WAIT).await.unwrap();
        assert_eq!(ended.state, JobState::Failed, "{}", ended.text);
        assert!(
            ended
                .text
                .starts_with("файл изменился после подтверждения: «отпуск.mp4»."),
            "{}",
            ended.text
        );
        assert_eq!(
            t.ffmpeg_runs().len(),
            before,
            "FFmpeg never saw the new file"
        );
    }

    #[tokio::test]
    async fn a_file_that_is_gone_or_only_touched_is_told_apart() {
        let t = av();
        let path = t.file("a.mp4");
        let stamp = Stamp::of(&path);
        assert!(stamp.holds());
        // The same bytes written again at another time are another file.
        let later = std::time::SystemTime::now() + Duration::from_secs(90);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(later).unwrap();
        drop(file);
        assert!(!stamp.holds());
        assert!(Stamp::of(&path).holds());
        std::fs::remove_file(&path).unwrap();
        assert!(!stamp.holds());
        assert!(
            !Stamp::of(&path).holds(),
            "a missing file never holds, even against itself"
        );
    }

    #[tokio::test]
    async fn recognition_that_waited_takes_no_more_sound_than_its_card_said() {
        let t = av();
        let env = t.av();
        let tool = in_background(AvTranscribeTool(env.clone()), &env);
        let input = sound(&t, "лекция.mp3", 700.0);
        let args = json!({ "path": input });
        tool.assess(&args).unwrap();
        let release = busy_queue(&env);
        let out = tool
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        let id = out.data.unwrap()["job"]["id"].as_u64().unwrap();

        // While it waits, cards of other files push its length out of the
        // short list of lengths.
        super::super::understand::forget_asked(Path::new(&input));
        assert_eq!(asked_length(Path::new(&input)), None);
        // By the time it runs the container tells a longer length.
        t.answer(&probe_json("mp3", 9000.0, 5 * MB, None, Some("mp3")));
        release.cancel();
        let ended = env.jobs.wait(id, WAIT).await.unwrap();
        assert_ne!(ended.state, JobState::Stopped, "{}", ended.text);
        let cut: Vec<String> = t
            .ffmpeg_runs()
            .into_iter()
            .filter(|a| a.contains(&"pcm_s16le".to_string()))
            .map(|a| a.join(" "))
            .collect();
        assert_eq!(cut.len(), 1, "{cut:?}");
        assert!(cut[0].contains("-t 701.000"), "{}", cut[0]);
    }

    #[tokio::test]
    async fn a_full_queue_refuses_the_call() {
        let t = av();
        let env = t.av();
        let tool = in_background(VideoCompressTool(env.clone()), &env);
        let hold = |ctx: JobCtx| -> mika_core::bgjobs::JobFuture {
            Box::pin(async move {
                ctx.cancel.cancelled().await;
                Err(ToolError::Cancelled)
            })
        };
        for i in 0..=MAX_QUEUED {
            env.jobs
                .submit(JobSpec::queued("t", format!("j{i}")), Box::new(hold))
                .unwrap();
        }
        let long = video(&t, "long.mp4", 3600.0);
        let r = tool
            .run(
                json!({ "path": long, "quality": "normal" }),
                Approval::for_tests(),
                CancellationToken::new(),
            )
            .await;
        match r {
            Err(ToolError::Failed(m)) => assert!(m.starts_with("очередь фоновых заданий полна")),
            other => panic!("{other:?}"),
        }
        assert!(t.ffmpeg_runs().is_empty(), "nothing was started");
        assert_eq!(env.jobs.list().len(), MAX_QUEUED + 1);
        env.jobs.shutdown(WAIT).await;
    }

    #[tokio::test]
    async fn without_ffmpeg_the_download_card_comes_back() {
        let t = av();
        let env = t.av();
        let tool = in_background(VideoCompressTool(env.clone()), &env);
        let input = t.file("x.mp4");
        *t.env.shared.ffmpeg_path.write() = "C:\\нет\\такой\\папки".into();
        // PATH or the download folder of this machine may hold one.
        if locate("C:\\нет\\такой\\папки").is_some() {
            return;
        }
        let args = json!({
            "path": input.display().to_string(), "quality": "normal", "background": true
        });
        for r in [
            tool.assess(&args).map(|_| ()),
            tool.run(
                args.clone(),
                Approval::for_tests(),
                CancellationToken::new(),
            )
            .await
            .map(|_| ()),
        ] {
            match r {
                Err(ToolError::NotReady { message, data }) => {
                    assert_eq!(message, MISSING_TEXT);
                    assert_eq!(data, json!({ "kind": "ffmpeg_missing" }));
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(env.jobs.list().is_empty());
    }

    #[tokio::test]
    async fn a_call_that_only_checks_never_goes_out() {
        let t = av();
        let env = t.av();
        let tool = in_background(VideoCompressTool(env.clone()), &env);
        let input = t.file("small.mp4");
        // An hour, yet only 10 МБ: already under the size asked for.
        t.answer(&probe_json(
            "mov,mp4",
            3600.0,
            10 * MB,
            Some(("h264", 1280, 720)),
            Some("aac"),
        ));
        let args = json!({
            "path": input.display().to_string(), "target_mb": 50, "background": true
        });
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert!(a.scope.starts_with("check:"));
        assert_eq!(a.reason, "ничего не записывается");
        let out = tool
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        assert!(
            out.text.starts_with("Файл уже меньше 10 МБ"),
            "{}",
            out.text
        );
        assert!(env.jobs.list().is_empty());
        assert!(t.ffmpeg_runs().is_empty());
    }
}
