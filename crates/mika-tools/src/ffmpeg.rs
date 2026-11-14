//! Video and sound files through FFmpeg (decision 0027).
//!
//! FFmpeg is a separate program: the user's own, one found in `PATH`, or the
//! LGPL build Mika downloads only after the user says yes. Without it every
//! tool here answers with a card that offers the download.
//!
//! Originals are never written or deleted: a result goes to a new file next
//! to the input, written under a temporary name and renamed when ready.

mod args;
mod audio;
mod background;
mod convert;
mod edit;
mod encoders;
mod folder;
mod info;
mod job;
mod locate;
mod output;
mod probe;
mod progress;
mod record;
mod srt;
mod time;
mod understand;
mod voice;
mod wav;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mika_core::bgjobs::JobBoard;
use mika_core::policy::paths::PathAccess;
use mika_core::sound_capture::{NoSound, SoundCapture};
use mika_core::tools::{arg_str, Assessment, RiskLevel, ToolError};
use parking_lot::{Mutex, RwLock};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

pub use audio::{AudioProcessTool, VideoSubtitlesTool};
pub use background::{in_background, InBackground};
pub use convert::{AvConvertTool, VideoCompressTool};
pub use edit::{AvCutTool, AvJoinTool, AvSpeedTool, VideoGifTool, VideoRotateTool};
pub use folder::AvFolderTool;
pub use info::AvInfoTool;
pub use locate::{locate, locate_in, status, version};
pub use probe::{AudioStream, MediaInfo, VideoStream};
pub use record::{ScreenRecordStopTool, ScreenRecordTool};
pub use understand::{AvTranscribeTool, VideoDescribeTool};
pub use voice::FfmpegSound;

use crate::files::{path_arg, path_assessment};
use crate::sys::{Runner, SpeechToText, ToolEnv, Vision};

/// Where the FFmpeg in use came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegSource {
    Settings,
    Path,
    Downloaded,
}

impl FfmpegSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            FfmpegSource::Settings => "settings",
            FfmpegSource::Path => "path",
            FfmpegSource::Downloaded => "downloaded",
        }
    }
}

/// `ffmpeg.exe` and `ffprobe.exe` of one folder.
#[derive(Debug, Clone, PartialEq)]
pub struct FfmpegPaths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub source: FfmpegSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FfmpegStatus {
    pub paths: Option<FfmpegPaths>,
    /// A path is set in the settings and holds no FFmpeg.
    pub setting_invalid: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FfmpegVersion {
    /// `n9.0.2-39-g7adfbe40dc-20261031`.
    pub version: String,
    /// Built with `--enable-gpl`.
    pub gpl: bool,
}

/// Files the tools here made, newest last. Only these open from the chat card.
#[derive(Clone, Default)]
pub struct Outputs(Arc<Mutex<VecDeque<PathBuf>>>);

const OUTPUTS_KEPT: usize = 200;

impl Outputs {
    pub fn remember(&self, path: &Path) {
        let key = locate::path_key(path);
        let mut list = self.0.lock();
        list.retain(|p| locate::path_key(p) != key);
        list.push_back(path.to_path_buf());
        while list.len() > OUTPUTS_KEPT {
            list.pop_front();
        }
    }

    pub fn contains(&self, path: &Path) -> bool {
        let key = locate::path_key(path);
        self.0.lock().iter().any(|p| locate::path_key(p) == key)
    }
}

/// What the model is told when FFmpeg is missing; the chat shows a card.
pub(crate) const MISSING_TEXT: &str = "FFmpeg не найден, поэтому с файлом ничего не сделано. В чате показана кнопка «Скачать FFmpeg» (161 МБ, LGPL v3); ещё можно указать путь к своему FFmpeg в Настройки → Инструменты. Скажи это пользователю коротко и не вызывай инструменты видео и звука, пока он не скажет, что FFmpeg готов.";

/// Longest file that is encoded.
pub(crate) const MAX_ENCODE_S: f64 = 4.0 * 3600.0;
/// Longest file whose speech is recognised.
pub(crate) const MAX_TRANSCRIBE_S: f64 = 3.0 * 3600.0;

fn missing() -> ToolError {
    ToolError::NotReady {
        message: MISSING_TEXT.into(),
        data: json!({ "kind": "ffmpeg_missing" }),
    }
}

pub(crate) fn bad(msg: impl Into<String>) -> ToolError {
    ToolError::BadArguments(msg.into())
}

/// What a recording needs from the application.
#[derive(Clone)]
pub struct RecordDeps {
    pub sound: Arc<dyn SoundCapture>,
    /// `privacy.microphone_enabled`, read at every call.
    pub mic_allowed: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Default for RecordDeps {
    /// No sound devices and the microphone switched off.
    fn default() -> Self {
        Self {
            sound: Arc::new(NoSound),
            mic_allowed: Arc::new(|| false),
        }
    }
}

/// Blocks until no temporary file or folder waits for removal, or `timeout`.
/// For the exit of the application: a removal thread dies with the process.
pub fn wait_cleanup(timeout: std::time::Duration) {
    let started = std::time::Instant::now();
    while output::pending_removals() > 0 && started.elapsed() < timeout {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Everything the tools here use.
#[derive(Clone)]
pub struct AvEnv {
    pub runner: Arc<dyn Runner>,
    /// `tools.ffmpeg_path` from the settings.
    pub ffmpeg_path: Arc<RwLock<String>>,
    pub outputs: Outputs,
    pub stt: Arc<dyn SpeechToText>,
    pub vision: Arc<dyn Vision>,
    pub user_profile: String,
    /// Long work goes here and runs after the turn has ended.
    pub jobs: JobBoard,
    pub rec: RecordDeps,
}

impl AvEnv {
    pub fn new(env: &ToolEnv) -> Self {
        Self {
            runner: env.runner.clone(),
            ffmpeg_path: env.shared.ffmpeg_path.clone(),
            outputs: env.shared.av_outputs.clone(),
            stt: env.shared.stt.clone(),
            vision: env.shared.vision.clone(),
            user_profile: env.user_profile.clone(),
            jobs: env.shared.jobs.clone(),
            rec: env.shared.rec.clone(),
        }
    }

    /// FFmpeg to use now; without it, an error that carries the download card.
    pub(crate) fn ffmpeg(&self) -> Result<FfmpegPaths, ToolError> {
        let setting = self.ffmpeg_path.read().clone();
        locate(&setting).ok_or_else(missing)
    }

    /// A local file named by the argument `key`.
    pub(crate) fn input(&self, args: &Value, key: &str) -> Result<PathBuf, ToolError> {
        let raw = arg_str(args, key)?;
        local_only(raw)?;
        let path = path_arg(args, key, &self.user_profile)?;
        local_file(path)
    }

    /// A local file named by one item of a list.
    pub(crate) fn input_raw(&self, raw: &str) -> Result<PathBuf, ToolError> {
        local_only(raw)?;
        if raw.trim().is_empty() {
            return Err(bad("пустой путь"));
        }
        local_file(crate::util::resolve_path(raw, &self.user_profile))
    }

    /// A look into the file from the risk check, which cannot wait.
    pub(crate) fn probe_now(
        &self,
        paths: &FfmpegPaths,
        input: &Path,
    ) -> Result<MediaInfo, ToolError> {
        probe::probe_blocking(self.runner.as_ref(), paths, input)
    }

    pub(crate) async fn probe(
        &self,
        paths: &FfmpegPaths,
        input: &Path,
        cancel: &CancellationToken,
    ) -> Result<MediaInfo, ToolError> {
        probe::probe(self.runner.as_ref(), paths, input, cancel).await
    }

    /// Risk of reading `inputs` and writing `output`, summed up in `summary`.
    /// The scope names the folders read and the folder written, so a
    /// permanent permission covers only that pair of places.
    pub(crate) fn assess_io(&self, summary: String, inputs: &[&Path], output: &Path) -> Assessment {
        let read = |p: &Path| {
            path_assessment(
                p,
                &self.user_profile,
                PathAccess::Read,
                String::new(),
                RiskLevel::Low,
            )
        };
        let write = path_assessment(
            output,
            &self.user_profile,
            PathAccess::Write,
            String::new(),
            RiskLevel::Medium,
        );
        let first = inputs.first().copied().unwrap_or(output);
        let target = format!("{} → {}", first.display(), output.display());
        let mut a = crate::archive::pair(read(first), write, &summary, target);
        for p in inputs.iter().skip(1) {
            let r = read(p);
            a = a.at_least(r.level, r.reason);
        }
        a.with_scope(io_scope(inputs, output))
    }

    /// The result's name, remembered for the chat card, and what is in it.
    pub(crate) async fn finish(
        &self,
        paths: &FfmpegPaths,
        part: output::PartFile,
        result: &Path,
        cancel: &CancellationToken,
    ) -> Result<(PathBuf, Option<MediaInfo>), ToolError> {
        let done = part.keep(result)?;
        self.outputs.remember(&done);
        let info = self.probe(paths, &done, cancel).await.ok();
        Ok((done, info))
    }
}

/// Lower-case folder of a file: the scope of a permanent permission.
pub(crate) fn folder_scope(path: &Path) -> String {
    locate::folder_of(path).display().to_string().to_lowercase()
}

/// `c:\видео + d:\звук -> c:\готовое`: folders read, then the folder written.
pub(crate) fn io_scope(inputs: &[&Path], output: &Path) -> String {
    let mut from: Vec<String> = Vec::new();
    for p in inputs {
        let f = folder_scope(p);
        if !from.contains(&f) {
            from.push(f);
        }
    }
    format!("{} -> {}", from.join(" + "), folder_scope(output))
}

/// Scripts and playlists: FFmpeg would run them (AviSynth, VapourSynth) or
/// follow them to other files and addresses. Refused before ffprobe opens them.
const NOT_MEDIA: &[&str] = &[
    "avs", "avsi", "vpy", "m3u", "m3u8", "mpd", "ffconcat", "sdp",
];

fn local_file(path: PathBuf) -> Result<PathBuf, ToolError> {
    local_only(&path.to_string_lossy())?;
    if NOT_MEDIA.contains(&output::ext_of(&path).as_str()) {
        return Err(bad("это скрипт или плейлист, а не медиафайл"));
    }
    if !path.is_file() {
        return Err(bad(format!("нет файла {}", path.display())));
    }
    Ok(path)
}

/// Sound inside mp4, mov and mkv when it has to be encoded.
pub(crate) fn aac() -> Vec<String> {
    args::s(&["-c:a", "aac", "-b:a", "160k"])
}

/// Only files of this computer: no addresses, devices or patterns.
fn local_only(raw: &str) -> Result<(), ToolError> {
    let raw = raw.trim().trim_matches('"');
    let refuse = || bad("только файлы на этом компьютере");
    if raw.contains("://") || raw.contains(['*', '?']) {
        return Err(refuse());
    }
    let unified = raw.replace('/', "\\");
    if unified.starts_with("\\\\.\\") {
        return Err(refuse());
    }
    // `http:`, `pipe:`, `concat:` and the like; `C:` is a drive.
    if let Some((scheme, _)) = raw.split_once(':') {
        let mut chars = scheme.chars();
        let letter = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
        let rest = scheme.len() > 1
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
        if letter && rest {
            return Err(refuse());
        }
    }
    // A colon past the drive letter names a hidden stream of a file.
    if raw.char_indices().any(|(i, c)| c == ':' && i != 1) {
        return Err(refuse());
    }
    Ok(())
}

/// `48,7 МБ`, `245 МБ`, `1,2 ГБ`, `300 КБ`.
pub(crate) fn size_text(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    let text = if b >= 1024.0 * MB {
        format!("{:.1} ГБ", b / 1024.0 / MB)
    } else if b >= 100.0 * MB {
        format!("{:.0} МБ", b / MB)
    } else if b >= MB {
        format!("{:.1} МБ", b / MB)
    } else {
        format!("{} КБ", (b / 1024.0).ceil() as u64)
    };
    text.replace('.', ",")
}

/// `H.264`, `AAC`: codec names as people know them.
pub(crate) fn codec_name(codec: &str) -> String {
    match codec {
        "h264" => "H.264".into(),
        "hevc" => "H.265".into(),
        "av1" => "AV1".into(),
        "vp9" => "VP9".into(),
        "vp8" => "VP8".into(),
        "mpeg4" => "MPEG-4".into(),
        "aac" => "AAC".into(),
        "mp3" => "MP3".into(),
        "opus" => "Opus".into(),
        "vorbis" => "Vorbis".into(),
        "flac" => "FLAC".into(),
        "alac" => "ALAC".into(),
        "ac3" => "AC-3".into(),
        "eac3" => "E-AC-3".into(),
        "gif" => "GIF".into(),
        c if c.starts_with("pcm_") => "PCM".into(),
        c => c.to_uppercase(),
    }
}

/// `Готово: C:\…\x (сжато).mp4, 48,7 МБ (было 245 МБ), 12:31, H.264 1280×720.`
pub(crate) fn done_text(path: &Path, info: Option<&MediaInfo>, before: Option<u64>) -> String {
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut parts = vec![format!("Готово: {}", path.display()), size_text(bytes)];
    if let Some(b) = before {
        let last = parts.pop().unwrap_or_default();
        parts.push(format!("{last} (было {})", size_text(b)));
    }
    if let Some(i) = info {
        if let Some(d) = i.duration_s {
            parts.push(time::show_time(d));
        }
        if let Some(v) = &i.video {
            parts.push(format!("{} {}×{}", codec_name(&v.codec), v.width, v.height));
        } else if let Some(a) = &i.audio {
            parts.push(codec_name(&a.codec));
        }
    }
    format!("{}.", parts.join(", "))
}

/// The chat card: files with their size and, for media, length.
pub(crate) fn files_card(files: &[(PathBuf, Option<f64>)]) -> Value {
    let files: Vec<Value> = files
        .iter()
        .map(|(path, duration)| {
            let mut f = json!({
                "path": path.display().to_string(),
                "bytes": std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            });
            if let Some(d) = duration {
                f["duration_s"] = json!((d * 10.0).round() / 10.0);
            }
            f
        })
        .collect();
    json!({ "kind": "av_files", "files": files })
}

/// `«x.mp4» (12:31, 245 МБ)` for a summary.
pub(crate) fn about(path: &Path, info: &MediaInfo) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match info.duration_s {
        Some(d) => format!(
            "«{name}» ({}, {})",
            time::show_time(d),
            size_text(info.size)
        ),
        None => format!("«{name}» ({})", size_text(info.size)),
    }
}

/// `«x (сжато).mp4»`.
pub(crate) fn quoted(path: &Path) -> String {
    format!(
        "«{}»",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    )
}

/// Length check before encoding.
pub(crate) fn not_too_long(info: &MediaInfo, max: f64) -> Result<(), ToolError> {
    if info.duration() > max {
        return Err(bad(format!(
            "файл длиннее {} ч: такой обрабатывать не берусь",
            (max / 3600.0).round()
        )));
    }
    Ok(())
}

pub(crate) fn need_video(info: &MediaInfo) -> Result<&VideoStream, ToolError> {
    info.video.as_ref().ok_or_else(|| bad("в файле нет видео"))
}

pub(crate) fn need_audio(info: &MediaInfo) -> Result<&AudioStream, ToolError> {
    info.audio.as_ref().ok_or_else(|| bad("в файле нет звука"))
}

/// Video encoding for a result in `ext`: VP9 for WebM, H.264 otherwise,
/// with the sound encoded or copied as `audio` says.
pub(crate) fn video_codec(
    ext: &str,
    h264: encoders::H264,
    v: &VideoStream,
    audio: Option<Vec<String>>,
) -> Vec<String> {
    let mut out = if ext == "webm" {
        // VP9 and Opus come together.
        let mut w = encoders::webm_args();
        if audio.is_none() {
            w.truncate(w.len() - 4);
            w.push("-an".into());
        }
        return w;
    } else {
        h264.quality_args(false, v.width, v.height, v.fps.unwrap_or(30.0))
    };
    match audio {
        Some(a) => out.extend(a),
        None => out.push("-an".into()),
    }
    out
}

/// Sound inside a video result: copied when it fits, AAC otherwise.
pub(crate) fn video_sound(ext: &str, info: &MediaInfo) -> Option<Vec<String>> {
    let a = info.audio.as_ref()?;
    if ext != "webm" && args::fits(ext, &a.codec, false) {
        Some(args::s(&["-c:a", "copy"]))
    } else {
        Some(args::s(&["-c:a", "aac", "-b:a", "160k"]))
    }
}

#[cfg(test)]
pub(crate) mod testkit {
    //! A fake FFmpeg for tool tests: two empty programs in a folder and a
    //! sample input next to them.

    use std::path::PathBuf;

    use crate::sys::{RunOutput, ToolEnv};
    use crate::testing::Fakes;

    pub(crate) use super::probe::tests::probe_json;

    pub(crate) struct Av {
        pub env: ToolEnv,
        pub f: std::sync::Arc<Fakes>,
        pub dir: tempfile::TempDir,
    }

    impl Av {
        pub(crate) fn file(&self, name: &str) -> PathBuf {
            let p = self.dir.path().join(name);
            std::fs::write(&p, b"media").unwrap();
            p
        }

        pub(crate) fn answer(&self, stdout: &str) {
            self.f.run_outputs.lock().push_back(RunOutput {
                code: Some(0),
                stdout: stdout.into(),
                ..Default::default()
            });
        }

        pub(crate) fn answer_err(&self, stderr: &str) {
            self.f.run_outputs.lock().push_back(RunOutput {
                code: Some(0),
                stderr: stderr.into(),
                ..Default::default()
            });
        }

        /// Arguments of every FFmpeg run, not ffprobe's.
        pub(crate) fn ffmpeg_runs(&self) -> Vec<Vec<String>> {
            self.f
                .runs
                .lock()
                .iter()
                .filter(|(p, _, _)| p.ends_with("ffmpeg.exe"))
                .map(|(_, a, _)| a.clone())
                .collect()
        }

        pub(crate) fn av(&self) -> super::AvEnv {
            super::AvEnv::new(&self.env)
        }
    }

    pub(crate) const X264_LIST: &str = " ------\n V....D libx264  H.264\n V....D libvpx-vp9  VP9\n A....D libmp3lame  MP3\n A....D libopus  Opus\n A....D aac  AAC\n";

    /// FFmpeg found by the settings; every program writes `media` to its output.
    pub(crate) fn av() -> Av {
        let (env, f) = crate::testing::env();
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("ffmpeg-bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("ffmpeg.exe"), b"").unwrap();
        std::fs::write(bin.join("ffprobe.exe"), b"").unwrap();
        *env.shared.ffmpeg_path.write() = bin.display().to_string();
        *f.run_write.lock() = Some(b"media".to_vec());
        // Runs without a queued answer end well and print nothing.
        *f.run_output.lock() = RunOutput {
            code: Some(0),
            ..Default::default()
        };
        Av { env, f, dir }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_compare_paths_loosely_and_keep_the_last_ones() {
        let o = Outputs::default();
        o.remember(Path::new("C:\\Users\\me\\Видео\\x (сжато).mp4"));
        assert!(o.contains(Path::new("c:/users/ME/видео/X (сжато).MP4")));
        assert!(!o.contains(Path::new("C:\\Users\\me\\Видео\\x.mp4")));
        for i in 0..250 {
            o.remember(Path::new(&format!("C:\\t\\{i}.mp4")));
        }
        assert!(!o.contains(Path::new("C:\\Users\\me\\Видео\\x (сжато).mp4")));
        assert!(o.contains(Path::new("C:\\t\\249.mp4")));
        assert_eq!(o.0.lock().len(), 200);
    }

    #[test]
    fn only_local_files_are_taken() {
        for raw in [
            "http://x/a.mp4",
            "https://x/a.mp4",
            "concat:a.mp4|b.mp4",
            "pipe:0",
            "C:\\v\\*.mp4",
            "C:\\v\\a?.mp4",
            "\\\\.\\pipe\\x",
            "//./pipe/x",
            "file:C:\\a.mp4",
            "C:\\v\\a.mp4:script.avs",
        ] {
            assert!(local_only(raw).is_err(), "{raw}");
        }
        for raw in [
            "C:\\v\\a.mp4",
            "\\\\server\\share\\a.mp4",
            "~\\a.mp4",
            "a.mp4",
            "%USERPROFILE%\\a.mp4",
        ] {
            assert!(local_only(raw).is_ok(), "{raw}");
        }
    }

    #[test]
    fn scripts_and_playlists_are_not_opened() {
        let (env, _) = crate::testing::env();
        let av = AvEnv::new(&env);
        let d = tempfile::tempdir().unwrap();
        for name in ["clip.avs", "clip.VPY", "list.m3u8", "x.mpd"] {
            let p = d.path().join(name);
            std::fs::write(&p, b"x").unwrap();
            assert!(
                matches!(
                    av.input_raw(&p.display().to_string()),
                    Err(ToolError::BadArguments(m)) if m.contains("скрипт или плейлист")
                ),
                "{name}"
            );
        }
    }

    #[test]
    fn scope_names_the_folders_read_and_written() {
        let s = io_scope(
            &[
                Path::new("C:\\Видео\\a.mp4"),
                Path::new("D:\\Звук\\b.mp3"),
                Path::new("C:\\Видео\\c.mp4"),
            ],
            Path::new("E:\\Готово\\x.mp4"),
        );
        assert_eq!(s, "c:\\видео + d:\\звук -> e:\\готово");
    }

    #[test]
    fn sizes_and_codecs_read_naturally() {
        assert_eq!(size_text(51_066_000), "48,7 МБ");
        assert_eq!(size_text(256_901_120), "245 МБ");
        assert_eq!(size_text(300 * 1024), "300 КБ");
        assert_eq!(size_text(1_288_490_189), "1,2 ГБ");
        assert_eq!(codec_name("h264"), "H.264");
        assert_eq!(codec_name("pcm_s16le"), "PCM");
        assert_eq!(codec_name("wmv2"), "WMV2");
    }

    #[test]
    fn card_lists_files_with_size_and_length() {
        let d = tempfile::tempdir().unwrap();
        let v = d.path().join("x.mp4");
        let t = d.path().join("x.txt");
        std::fs::write(&v, b"12345").unwrap();
        std::fs::write(&t, b"ab").unwrap();
        let card = files_card(&[(v.clone(), Some(75.23)), (t, None)]);
        assert_eq!(card["kind"], "av_files");
        assert_eq!(card["files"][0]["path"], v.display().to_string());
        assert_eq!(card["files"][0]["bytes"], 5);
        assert_eq!(card["files"][0]["duration_s"], 75.2);
        assert!(card["files"][1].get("duration_s").is_none());
    }

    #[test]
    fn missing_ffmpeg_is_not_ready_with_the_download_card() {
        let (env, _) = crate::testing::env();
        *env.shared.ffmpeg_path.write() = "C:\\нет\\такой\\папки".into();
        let av = AvEnv::new(&env);
        // PATH or the download folder of this machine may hold one.
        if locate("C:\\нет\\такой\\папки").is_some() {
            return;
        }
        match av.ffmpeg() {
            Err(ToolError::NotReady { message, data }) => {
                assert_eq!(message, MISSING_TEXT);
                assert_eq!(data, json!({ "kind": "ffmpeg_missing" }));
            }
            other => panic!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod live {
    //! Every tool that needs no provider, on files FFmpeg makes itself.
    //! `cargo test -p mika-tools live_ffmpeg_ops -- --ignored --nocapture`;
    //! `MIKA_FFMPEG` names FFmpeg when it is neither in PATH nor downloaded.

    use std::time::Duration;

    use mika_core::tools::Tool;
    use serde_json::json;

    use super::*;
    use crate::sys::Runner;
    use crate::testing::run;

    /// Makes a file from FFmpeg's own test sources.
    async fn make(runner: &dyn Runner, paths: &FfmpegPaths, args: &[&str], out: &Path) {
        let mut argv: Vec<String> = ["-hide_banner", "-nostdin", "-v", "error"]
            .iter()
            .chain(args)
            .map(|s| s.to_string())
            .collect();
        argv.push(out.display().to_string());
        let r = runner
            .run(
                &paths.ffmpeg.to_string_lossy(),
                &argv,
                Duration::from_secs(60),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(r.code, Some(0), "{}", r.stderr);
    }

    async fn step(tool: &dyn Tool, args: Value) -> String {
        let name = tool.spec().name;
        let out = run(tool, args)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        println!("{name}: {}", out.text);
        out.text
    }

    #[tokio::test]
    #[ignore = "needs FFmpeg on this computer"]
    async fn live_ffmpeg_ops() {
        let setting = std::env::var("MIKA_FFMPEG").unwrap_or_default();
        let Some(paths) = locate(&setting) else {
            println!("FFmpeg is not found: nothing to check");
            return;
        };
        println!("{paths:?}: {:?}", version(&paths));
        let (mut env, _) = crate::testing::env();
        env.runner = Arc::new(crate::win::ProcessRunner);
        *env.shared.ffmpeg_path.write() = paths.ffmpeg.display().to_string();
        let av = AvEnv::new(&env);
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("тест кадры.mkv");
        let tone = dir.path().join("тон.wav");
        make(
            av.runner.as_ref(),
            &paths,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30:duration=6",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=6",
                "-c:v",
                "mpeg4",
                "-q:v",
                "5",
                "-c:a",
                "flac",
                "-shortest",
            ],
            &video,
        )
        .await;
        make(
            av.runner.as_ref(),
            &paths,
            &[
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=16000:cl=mono:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=300:duration=3",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=16000:cl=mono:d=1",
                "-filter_complex",
                "[0][1][2]concat=n=3:v=0:a=1",
                "-c:a",
                "pcm_s16le",
            ],
            &tone,
        )
        .await;
        let v = video.display().to_string();
        let a = tone.display().to_string();

        let info = step(&AvInfoTool(av.clone()), json!({ "path": v })).await;
        assert!(info.contains("640×360"), "{info}");
        step(
            &AvConvertTool(av.clone()),
            json!({ "path": v, "format": "mp4" }),
        )
        .await;
        step(
            &AvConvertTool(av.clone()),
            json!({ "path": v, "format": "mp3" }),
        )
        .await;
        step(
            &VideoCompressTool(av.clone()),
            json!({ "path": v, "quality": "strong", "height": 240 }),
        )
        .await;
        step(
            &VideoCompressTool(av.clone()),
            json!({ "path": v, "target_mb": 1 }),
        )
        .await;
        step(
            &AvCutTool(av.clone()),
            json!({ "path": v, "start": "1", "end": "0:03" }),
        )
        .await;
        step(
            &AvCutTool(av.clone()),
            json!({ "path": v, "start": 2, "fast": true }),
        )
        .await;
        step(&AvJoinTool(av.clone()), json!({ "paths": [v, v] })).await;
        step(
            &VideoGifTool(av.clone()),
            json!({ "path": v, "duration": 2, "width": 320 }),
        )
        .await;
        step(
            &VideoRotateTool(av.clone()),
            json!({ "path": v, "angle": 90 }),
        )
        .await;
        step(&AvSpeedTool(av.clone()), json!({ "path": v, "factor": 2 })).await;
        step(
            &AvSpeedTool(av.clone()),
            json!({ "path": a, "factor": 0.5 }),
        )
        .await;
        step(
            &AudioProcessTool(av.clone()),
            json!({ "path": a, "trim_silence": true, "normalize": true, "denoise": "normal" }),
        )
        .await;
        step(
            &AudioProcessTool(av.clone()),
            json!({ "path": v, "volume": "-6dB" }),
        )
        .await;
        let srt = dir.path().join("титры.srt");
        std::fs::write(
            &srt,
            "1\r\n00:00:00,500 --> 00:00:03,000\r\nПривет, мир\r\n\r\n",
        )
        .unwrap();
        step(
            &VideoSubtitlesTool(av.clone()),
            json!({ "path": v, "subtitles": srt.display().to_string() }),
        )
        .await;

        // A folder of three files as one background job.
        let folder = dir.path().join("папка");
        std::fs::create_dir(&folder).unwrap();
        for name in ["a.mkv", "b.mkv", "c.mkv"] {
            std::fs::copy(&video, folder.join(name)).unwrap();
        }
        let started = run(
            &AvFolderTool::new(av.clone()),
            json!({
                "folder": folder.display().to_string(),
                "operation": "convert",
                "format": "mp4",
                "subfolder": "готово",
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("av_folder: {e}"));
        println!("av_folder: {}", started.text);
        let id = started.data.unwrap()["job"]["id"].as_u64().unwrap();
        let job = av
            .jobs
            .wait(id, Duration::from_secs(300))
            .await
            .expect("the folder job ends");
        println!("av_folder: {:?} {}", job.state, job.text);
        assert_eq!(job.state, mika_core::bgjobs::JobState::Done, "{}", job.text);
        assert_eq!(job.data.unwrap()["files"].as_array().unwrap().len(), 3);
        let results: Vec<_> = std::fs::read_dir(folder.join("готово"))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        assert_eq!(results.len(), 3, "{results:?}");
        assert!(
            !results.iter().any(|n| n.contains(".mika-part.")),
            "no half-written files"
        );

        let made: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        println!("{made:#?}");
        assert!(
            !made.iter().any(|n| n.contains(".mika-part.")),
            "no half-written files"
        );
    }
}
