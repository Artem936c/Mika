//! `screen_record` and `screen_record_stop`: the screen or one window into a
//! video file, with the sound of the computer and, when asked, the
//! microphone (decision 0032).
//!
//! The picture is taken by FFmpeg (`gdigrab`, an area of the desktop), the
//! sound by `SoundCapture` into WAV files beside it. The recording is a
//! background job that runs beside the queue. It is cut into pieces: a piece
//! ends whenever recording may not go on (a blind-zone window in front, the
//! recorded window behind another program) and the next one starts when it
//! may again. At the end FFmpeg puts the pieces and their sound into one MP4.

mod args;
mod plan;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mika_core::bgjobs::{
    job_card, JobBoard, JobCtx, JobHandle, JobSpec, JobState, JobView, JobWork, STOPPED_TEXT,
};
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::blind::is_blind_title;
use mika_core::sound_capture::{SoundSource, SoundTake};
use mika_core::tools::{
    arg_str_opt, spec, Assessment, Progress, RiskLevel, Tool, ToolError, ToolOutput,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use self::args::{capture_args, devices, mux_args};
use self::plan::{
    desktop_of, done_text, even_region, keep_seconds, minutes_text, nothing_text, output_path,
    overlaps, sound_offset, stop_grace, videos_dir, work_dir, RecPlan, Source, Stop,
    DEFAULT_MINUTES, KIND, MAX_MINUTES, MIN_FREE_RUN_GB, MIN_FREE_START_GB, PAUSE_CAP_S, POLL,
    QUIT_WAIT,
};
use super::args::{concat_line, join_list_args, number};
use super::encoders::{pick_h264, H264};
use super::job::{copy_timeout, run_ffmpeg};
use super::output::{remove_later, PartFile};
use super::time::show_time;
use super::{bad, files_card, AvEnv, FfmpegPaths};
use crate::screen::SCREEN_DISABLED;
use crate::sys::{Rect, SystemProbe, WindowInfo, Windows};
use crate::ui::UiEnv;
use crate::util::{arg_bool, failed, schema, truncate};

/// Why a recording does not start while a blind-zone window is in front or
/// shows inside the area to record. The title of that window is never told.
const BLIND_START: &str = "На экране окно из «слепой зоны» (пароли, вход, банк, оплата): запись не начинаю. Закрой или сверни его и повтори.";
const MIC_DISABLED: &str = "Микрофон выключен. Его включает пользователь переключателем «Микрофон» в шапке чата. Без микрофона запись можно начать, если не указывать microphone.";
const NO_GDIGRAB: &str = "в этой сборке FFmpeg нет захвата экрана (gdigrab): нужна сборка для Windows; копию Мики можно скачать в Настройки → Инструменты";
const REASON: &str = "запись экрана и звука включается только с твоего подтверждения, каждый раз заново; на окнах с паролями и оплатой запись встаёт на паузу";
/// Added for a window: when it is recorded.
const REASON_WINDOW: &str = "окно пишется, пока его программа на переднем плане";
/// The outcome of a recording that was stopped and whose file was not put
/// together in the time it had.
const NOT_SAVED: &str = "Запись остановлена, но собрать файл не удалось: не хватило времени.";
const NOTE_BLIND: &str = "пауза: на экране окно из слепой зоны";
const NOTE_BEHIND: &str = "пауза: окно не на переднем плане";
const NOTE_SAVING: &str = "сохраняю запись…";

/// Longest wait of `screen_record_stop` for the file: a little more than
/// the recording gets to put it together.
const STOP_WAIT: Duration = Duration::from_secs(185);
/// How often the free space is looked at while recording, in polls.
const DISK_EVERY: u32 = 120;
/// The note with the recorded time is renewed once in so many polls: a second.
const NOTE_EVERY: u32 = 4;
/// How long the file name shown on a card is kept for the run that follows:
/// longer than a confirmation waits.
const PLAN_KEPT: Duration = Duration::from_secs(120);

#[cfg(test)]
thread_local! {
    /// A test's own time for the work after a stop, in place of `stop_grace`.
    static TEST_GRACE: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

/// How long the work of a recording of at most `limit_s` gets after a stop.
fn grace_for(limit_s: f64) -> Duration {
    #[cfg(test)]
    if let Some(grace) = TEST_GRACE.with(|g| g.get()) {
        return grace;
    }
    stop_grace(limit_s)
}

/// The recording that runs now, if any.
fn recording(jobs: &JobBoard) -> Option<JobView> {
    jobs.active().into_iter().find(|j| j.kind == KIND)
}

/// File names shown on confirm cards and not used yet.
static PLANNED: Mutex<Vec<(PathBuf, Instant)>> = Mutex::new(Vec::new());

/// The file of the next recording. The card and the run that follows it get
/// the same name, though the clock has moved on between them.
fn planned_output(profile: &str) -> PathBuf {
    let folder = videos_dir(profile);
    let mut planned = PLANNED.lock();
    planned.retain(|(_, at)| at.elapsed() < PLAN_KEPT);
    if let Some((path, _)) = planned
        .iter()
        .find(|(p, _)| p.parent() == Some(folder.as_path()) && !p.exists())
    {
        return path.clone();
    }
    let path = output_path(profile, chrono::Local::now().naive_local());
    planned.push((path.clone(), Instant::now()));
    path
}

/// The name is taken by a recording: the next card gets a new one.
fn forget_planned(output: &Path) {
    PLANNED.lock().retain(|(p, _)| p != output);
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

/// A number that may come as a string.
fn number_arg(args: &Value, key: &str) -> Result<Option<f64>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(v) => v
            .as_f64()
            .or_else(|| {
                v.as_str()
                    .and_then(|s| s.trim().replace(',', ".").parse().ok())
            })
            .filter(|n: &f64| n.is_finite())
            .map(Some)
            .ok_or_else(|| bad(format!("`{key}` — число"))),
    }
}

/// `chrome` of `chrome.exe`.
fn app_name(process: &str) -> &str {
    process.strip_suffix(".exe").unwrap_or(process)
}

/// Free gigabytes on the drive of `path`: the longest mount that holds it.
fn free_on(disks: &[(String, f64, f64)], path: &Path) -> Option<(String, f64)> {
    let p = path.to_string_lossy().to_lowercase();
    disks
        .iter()
        .filter(|(mount, _, _)| !mount.is_empty() && p.starts_with(&mount.to_lowercase()))
        .max_by_key(|(mount, _, _)| mount.len())
        .map(|(mount, free, _)| (mount.clone(), *free))
}

/// The last lines FFmpeg complained with, at most 300 characters.
fn complaint(stderr: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(3)..].join("; ");
    if tail.is_empty() {
        "захват экрана завершился с ошибкой".into()
    } else {
        truncate(&tail, 300)
    }
}

/// Seconds FFmpeg has written, from a line of `-progress`.
fn out_time_us(line: &str) -> Option<u64> {
    let (key, value) = line.trim().split_once('=')?;
    // Both are microseconds: `out_time_ms` is misnamed.
    matches!(key.trim(), "out_time_us" | "out_time_ms")
        .then(|| value.trim().parse().ok())
        .flatten()
}

/// A blind-zone window that is on the screen inside `region`: in front or
/// not, covered by another window or not, a main window or a dialog. `shown`
/// is `Windows::on_screen`: a minimized or hidden window is not in it.
fn blind_shows(shown: &[(String, Rect)], region: Rect) -> bool {
    shown
        .iter()
        .any(|(title, place)| is_blind_title(title) && overlaps(*place, region))
}

/// The mark beside a piece that a blind-zone window ended: until the file is
/// put together the piece holds a moment of that window in its tail.
fn blind_mark(work: &Path, index: usize) -> PathBuf {
    work.join(format!("seg-{index}.blind"))
}

/// Everything recorded as piece `index`: the picture and its sound.
fn piece_files(work: &Path, index: usize) -> [PathBuf; 3] {
    [
        work.join(format!("seg-{index}.mkv")),
        work.join(format!("seg-{index}-sys.wav")),
        work.join(format!("seg-{index}-mic.wav")),
    ]
}

/// Removes the pieces of `work` that carry a blind mark, with the mark.
/// Returns how many pieces went.
fn drop_marked(work: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(work) else {
        return 0;
    };
    let mut dropped = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(index) = name
            .strip_prefix("seg-")
            .and_then(|n| n.strip_suffix(".blind"))
            .and_then(|n| n.parse::<usize>().ok())
        else {
            continue;
        };
        for file in piece_files(work, index) {
            let _ = std::fs::remove_file(file);
        }
        let _ = std::fs::remove_file(entry.path());
        dropped += 1;
    }
    dropped
}

/// Folders of pieces that earlier recordings left in `folder` (the app was
/// closed by force, or a file could not be put together): pieces a blind-zone
/// window ended are removed from them, the rest stays for the user. Called
/// when no recording runs.
fn sweep_blind(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if !name.ends_with(".mika-part") || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if drop_marked(&entry.path()) > 0 {
            // Nothing else was in it: the empty folder goes too.
            let _ = std::fs::remove_dir(entry.path());
        }
    }
}

/// Owns the folder of the pieces while a recording works. However the work
/// ends (the file is ready, it could not be made, the work was dropped), no
/// piece that a blind-zone window ended is left on the disk.
struct RawPieces(PathBuf);

impl Drop for RawPieces {
    fn drop(&mut self) {
        drop_marked(&self.0);
    }
}

pub struct ScreenRecordTool {
    pub av: AvEnv,
    pub ui: UiEnv,
    /// The «Экран» switch: nothing is recorded while it is off.
    pub screen_on: Arc<AtomicBool>,
    pub probe: Arc<dyn SystemProbe>,
}

impl ScreenRecordTool {
    /// Checks everything a recording needs and says what would be recorded.
    fn prepare(&self, args: &Value) -> Result<RecPlan, ToolError> {
        self.av.ffmpeg()?;
        if !self.screen_on.load(Ordering::Relaxed) {
            return Err(failed(SCREEN_DISABLED));
        }
        if recording(&self.av.jobs).is_some() {
            return Err(bad(
                "запись уже идёт: сначала останови её (screen_record_stop)",
            ));
        }
        let windows = &self.ui.windows;
        if windows
            .foreground()
            .is_some_and(|w| is_blind_title(&w.title))
        {
            return Err(failed(BLIND_START));
        }
        let monitors = windows.monitor_bounds();
        let named = arg_str_opt(args, "window").is_some_and(|w| !w.trim().is_empty());
        let (source, region) = if named {
            // Refuses a blind-zone window and Mika's own.
            let w = self.ui.target(args)?;
            let region = windows
                .bounds(w.id)
                .ok_or_else(|| bad("окно свёрнуто или скрыто"))?;
            (
                Source::Window {
                    id: w.id,
                    title: w.title,
                    process: w.process,
                },
                region,
            )
        } else {
            let n = match number_arg(args, "monitor")? {
                None => 1,
                Some(n) if n >= 1.0 && n.fract() == 0.0 => n as usize,
                Some(_) => return Err(bad("monitor — номер монитора, от 1")),
            };
            let region = n
                .checked_sub(1)
                .and_then(|i| monitors.get(i))
                .copied()
                .ok_or_else(|| bad(format!("монитора {n} нет: их {}", monitors.len())))?;
            (Source::Screen { monitor: n }, region)
        };
        let region = even_region(region, desktop_of(&monitors))
            .ok_or_else(|| bad("область записи слишком мала"))?;
        // Not in front, but on the screen where the recording looks.
        if blind_shows(&windows.on_screen(), region) {
            return Err(failed(BLIND_START));
        }
        let minutes = number_arg(args, "minutes")?.unwrap_or(DEFAULT_MINUTES);
        if !(1.0..=MAX_MINUTES).contains(&minutes) {
            return Err(bad("minutes — от 1 до 120"));
        }
        let microphone = arg_bool(args, "microphone");
        if microphone && !(self.av.rec.mic_allowed)() {
            return Err(failed(MIC_DISABLED));
        }
        Ok(RecPlan {
            source,
            region,
            limit_s: minutes * 60.0,
            sound: flag(args, "sound") != Some(false),
            microphone,
            output: planned_output(&self.av.user_profile),
        })
    }

    /// The confirm card: always a high risk, with every thing that will be
    /// recorded on a line of its own. The microphone and the sound come
    /// first: the title of a window is the page's own text and may be long,
    /// and nothing it does may push those two lines out of sight.
    fn assessment(plan: &RecPlan) -> Assessment {
        let (w, h) = (plan.region.w, plan.region.h);
        // The summary is short: a high risk is confirmed by voice by saying it.
        let (summary, what, scope, reason) = match &plan.source {
            Source::Screen { monitor } => (
                "Записать экран".to_string(),
                format!("Экран: монитор {monitor}, {w}×{h}"),
                "экран".to_string(),
                REASON.to_string(),
            ),
            Source::Window { title, process, .. } => (
                format!("Записать окно {}", app_name(process))
                    .trim_end()
                    .to_string(),
                format!(
                    "Окно: «{}» ({process}), {w}×{h}",
                    truncate(&title.replace(['\n', '\r'], " "), 60)
                ),
                if process.is_empty() {
                    "экран".to_string()
                } else {
                    process.clone()
                },
                format!("{REASON}; {REASON_WINDOW}"),
            ),
        };
        let yes_no = |on: bool| {
            if on {
                "записывается"
            } else {
                "не записывается"
            }
        };
        let target = [
            format!("Микрофон: {}", yes_no(plan.microphone)),
            format!("Звук компьютера: {}", yes_no(plan.sound)),
            format!("Длина: до {}", minutes_text(plan.limit_s / 60.0)),
            what,
            format!("Файл: {}", plan.output.display()),
        ]
        .join("\n");
        Assessment::new(RiskLevel::High, summary, target, reason).with_scope(scope)
    }
}

/// One piece that goes into the file.
struct Piece {
    video: PathBuf,
    /// Seconds of it that are kept.
    keep: f64,
    /// Its sound files, each with its offset against the first frame.
    sounds: Vec<(PathBuf, f64)>,
}

/// What a recording has gathered so far.
#[derive(Default)]
struct Gathered {
    pieces: Vec<Piece>,
    /// Seconds recorded: the limit counts these, not the pauses.
    recorded: f64,
    /// How many times a blind-zone window stopped a piece.
    pauses: u32,
    /// The first sound take that did not work, in words.
    sound_note: Option<String>,
}

/// Why recording may not run now.
enum Hold {
    Blind,
    Behind,
    Gone,
}

/// Why a running piece is told to finish.
enum Quit {
    /// The recording ends.
    Stop(Stop),
    /// A blind-zone window came to the front: the tail of the piece is cut.
    Blind,
    /// The recorded window is not in front, has moved, or the microphone
    /// was switched off: the next piece starts when it may.
    Next,
}

/// A sound take that runs beside a piece.
struct Running {
    take: Box<dyn SoundTake>,
    wav: PathBuf,
    /// When its file's time began, in seconds of the piece.
    at: f64,
    source: SoundSource,
}

fn sound_name(source: SoundSource) -> &'static str {
    match source {
        SoundSource::System => "звук компьютера",
        SoundSource::Microphone => "микрофон",
    }
}

/// A recording at work.
struct Recorder {
    av: AvEnv,
    windows: Arc<dyn Windows>,
    screen_on: Arc<AtomicBool>,
    probe: Arc<dyn SystemProbe>,
    paths: FfmpegPaths,
    h264: H264,
    plan: RecPlan,
    /// The folder of the pieces.
    work: PathBuf,
    /// Removes what a blind-zone window left in the pieces when the work ends.
    _raw: RawPieces,
}

/// The windows as one look found them.
struct Seen {
    front: Option<WindowInfo>,
    /// Every window on the screen: its title and its place.
    shown: Vec<(String, Rect)>,
    /// The recorded window still exists; true for a recording of the screen.
    there: bool,
}

impl Recorder {
    fn desktop(&self) -> Rect {
        desktop_of(&self.windows.monitor_bounds())
    }

    fn see(&self) -> Seen {
        Seen {
            front: self.windows.foreground(),
            shown: self.windows.on_screen(),
            there: match &self.plan.source {
                Source::Screen { .. } => true,
                Source::Window { id, .. } => self.windows.list().iter().any(|w| w.id == *id),
            },
        }
    }

    /// A blind-zone window is in front, or shows anywhere inside `region`:
    /// behind the window in front it is on the screen all the same. For a
    /// recorded window this holds its own title too, which a page may change.
    fn blind_in(&self, seen: &Seen, region: Rect) -> bool {
        seen.front
            .as_ref()
            .is_some_and(|w| is_blind_title(&w.title))
            || blind_shows(&seen.shown, region)
    }

    /// The area to record now, or why recording must wait.
    fn region_now(&self, seen: &Seen) -> Result<Rect, Hold> {
        if seen
            .front
            .as_ref()
            .is_some_and(|w| is_blind_title(&w.title))
        {
            return Err(Hold::Blind);
        }
        let region = match &self.plan.source {
            Source::Screen { .. } => self.plan.region,
            Source::Window { id, process, .. } => {
                if !seen.there {
                    return Err(Hold::Gone);
                }
                // Another program in front may cover the window: its picture
                // would get into the recording.
                if !seen.front.as_ref().is_some_and(|w| &w.process == process) {
                    return Err(Hold::Behind);
                }
                self.windows
                    .bounds(*id)
                    .and_then(|r| even_region(r, self.desktop()))
                    .ok_or(Hold::Behind)?
            }
        };
        if blind_shows(&seen.shown, region) {
            return Err(Hold::Blind);
        }
        Ok(region)
    }

    /// Looks whether the running piece may go on. A blind-zone window is
    /// looked for by the caller, before this.
    fn look(
        &self,
        seen: &Seen,
        region: Rect,
        mic_runs: bool,
        low_disk: &AtomicBool,
    ) -> Option<Quit> {
        match self.region_now(seen) {
            Err(Hold::Blind) => return Some(Quit::Blind),
            Err(Hold::Behind | Hold::Gone) => return Some(Quit::Next),
            // The window moved or changed its size: a new piece takes its new place.
            Ok(now) if now != region => return Some(Quit::Next),
            Ok(_) => {}
        }
        if !self.screen_on.load(Ordering::Relaxed) {
            return Some(Quit::Stop(Stop::ScreenOff));
        }
        if mic_runs && !(self.av.rec.mic_allowed)() {
            return Some(Quit::Next);
        }
        if low_disk.load(Ordering::Relaxed) {
            return Some(Quit::Stop(Stop::Disk));
        }
        None
    }

    /// Sets `low` when the drive of the recording runs out of space.
    fn watch_disk(&self, low: &Arc<AtomicBool>) {
        let (probe, path, low) = (self.probe.clone(), self.plan.output.clone(), low.clone());
        tokio::task::spawn_blocking(move || {
            if let Some((_, free)) = free_on(&probe.facts().disks, &path) {
                if free < MIN_FREE_RUN_GB {
                    low.store(true, Ordering::Relaxed);
                }
            }
        });
    }

    /// Records one piece into `seg-{index}`. Returns why the recording ends,
    /// or `None` when the next piece may follow.
    async fn piece(
        &self,
        index: usize,
        region: Rect,
        frame: (u32, u32),
        got: &mut Gathered,
        ctx: &JobCtx,
    ) -> Option<Stop> {
        let left = self.plan.limit_s - got.recorded;
        let video = self.work.join(format!("seg-{index}.mkv"));
        let origin = Instant::now();

        // The sound first: its files begin before the picture and are cut to it.
        let mut wanted = Vec::new();
        if self.plan.sound {
            wanted.push((SoundSource::System, "sys"));
        }
        if self.plan.microphone && (self.av.rec.mic_allowed)() {
            wanted.push((SoundSource::Microphone, "mic"));
        }
        let mut takes: Vec<Running> = Vec::new();
        for (source, tag) in wanted {
            let wav = self.work.join(format!("seg-{index}-{tag}.wav"));
            let (sound, path) = (self.av.rec.sound.clone(), wav.clone());
            let started = tokio::task::spawn_blocking(move || sound.start(source, &path))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            match started {
                Ok(take) => takes.push(Running {
                    take,
                    wav,
                    at: origin.elapsed().as_secs_f64(),
                    source,
                }),
                Err(why) => {
                    log::warn!("sound take did not start: {why}");
                    got.sound_note.get_or_insert(format!(
                        "{} записать не удалось: {why}",
                        sound_name(source)
                    ));
                }
            }
        }
        let mic_runs = takes.iter().any(|t| t.source == SoundSource::Microphone);

        // What FFmpeg has written, and when its first frame was.
        let written = AtomicU64::new(0);
        let first: Mutex<Option<f64>> = Mutex::new(None);
        let on_line = |line: &str| {
            let Some(us) = out_time_us(line) else {
                return;
            };
            written.store(us, Ordering::Relaxed);
            if us > 0 {
                first
                    .lock()
                    .get_or_insert(origin.elapsed().as_secs_f64() - us as f64 / 1e6);
            }
        };
        let launched = origin.elapsed().as_secs_f64();
        let quit = CancellationToken::new();
        let argv = capture_args(region, frame, left, self.h264, &video);
        let ffmpeg = self.paths.ffmpeg.to_string_lossy().into_owned();
        let run = self.av.runner.run_controlled(
            &ffmpeg,
            &argv,
            None,
            Duration::from_secs_f64(left + 60.0),
            // Never fires: a recording is not thrown away, it is told to finish.
            CancellationToken::new(),
            quit.clone(),
            QUIT_WAIT,
            &on_line,
        );
        tokio::pin!(run);
        let low_disk = Arc::new(AtomicBool::new(false));
        let mut tick = tokio::time::interval(POLL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut polls = 0u32;
        let mut why: Option<Quit> = None;
        // How much FFmpeg had written when a blind-zone window was noticed.
        let mut blind_at: Option<f64> = None;
        let out = loop {
            tokio::select! {
                r = &mut run => break r,
                _ = ctx.cancel.cancelled(), if why.is_none() => {
                    why = Some(Quit::Stop(Stop::User));
                    quit.cancel();
                }
                _ = tick.tick() => {
                    polls += 1;
                    let seen = self.see();
                    // Looked for until FFmpeg has really ended: it goes on
                    // writing for a while after it was told to finish.
                    if blind_at.is_none() && self.blind_in(&seen, region) {
                        blind_at = Some(written.load(Ordering::Relaxed) as f64 / 1e6);
                        // From now on the piece is not kept as it is.
                        let _ = std::fs::write(blind_mark(&self.work, index), b"");
                        if why.is_none() {
                            why = Some(Quit::Blind);
                            quit.cancel();
                        }
                    }
                    if why.is_none() {
                        if polls.is_multiple_of(DISK_EVERY) {
                            self.watch_disk(&low_disk);
                        }
                        why = self.look(&seen, region, mic_runs, &low_disk);
                        if matches!(why, Some(Quit::Blind)) {
                            blind_at = Some(written.load(Ordering::Relaxed) as f64 / 1e6);
                            let _ = std::fs::write(blind_mark(&self.work, index), b"");
                        }
                        if why.is_some() {
                            quit.cancel();
                        }
                    }
                    if why.is_none() && polls.is_multiple_of(NOTE_EVERY) {
                        let now = got.recorded + written.load(Ordering::Relaxed) as f64 / 1e6;
                        ctx.handle.set_note(&format!(
                            "{} из {}",
                            show_time(now),
                            show_time(self.plan.limit_s)
                        ));
                        let share = now / self.plan.limit_s.max(1.0) * 100.0;
                        ctx.handle.set_share(share.clamp(0.0, 100.0) as u8);
                    }
                }
            }
        };

        let mut sounds = Vec::new();
        for running in takes {
            let Running {
                take,
                wav,
                at,
                source,
            } = running;
            let stopped = tokio::task::spawn_blocking(move || take.stop())
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            match stopped {
                Ok(stats) if stats.seconds > 0.0 => sounds.push((wav, at)),
                Ok(_) => {}
                Err(why) => {
                    log::warn!("sound take did not end well: {why}");
                    got.sound_note.get_or_insert(format!(
                        "{} записать не удалось: {why}",
                        sound_name(source)
                    ));
                }
            }
        }

        let mut length = written.load(Ordering::Relaxed) as f64 / 1e6;
        if length <= 0.0 && video.is_file() {
            // FFmpeg told nothing about its progress: the file itself knows.
            length = self
                .av
                .probe(&self.paths, &video, &CancellationToken::new())
                .await
                .ok()
                .map(|i| i.duration())
                .unwrap_or(0.0);
        }
        if blind_at.is_some() {
            got.pauses += 1;
        }
        match (video.is_file(), keep_seconds(length, blind_at)) {
            (true, Some(keep)) => {
                let first = first.lock().unwrap_or(launched);
                got.pieces.push(Piece {
                    video,
                    keep,
                    sounds: sounds
                        .into_iter()
                        .map(|(wav, at)| (wav, sound_offset(first, at)))
                        .collect(),
                });
            }
            // Nothing of it goes into the file. What a blind-zone window
            // ended is removed at once, with its sound.
            _ if blind_at.is_some() => {
                for file in piece_files(&self.work, index) {
                    let _ = std::fs::remove_file(file);
                }
                let _ = std::fs::remove_file(blind_mark(&self.work, index));
            }
            _ => {}
        }
        got.recorded += length;

        match why {
            Some(Quit::Stop(stop)) => Some(stop),
            Some(Quit::Blind | Quit::Next) => None,
            // FFmpeg ended by itself: at the limit, or something broke. A
            // piece that broke is never started again.
            None => Some(match out {
                Err(e) => Stop::Broke(e),
                Ok(o) if o.timed_out => Stop::Broke("FFmpeg не уложился в срок".into()),
                Ok(o) if o.code == Some(0) && length >= left - 1.0 => Stop::Limit,
                Ok(o) if o.code == Some(0) => Stop::Broke("захват экрана прервался".into()),
                Ok(o) => Stop::Broke(complaint(&o.stderr)),
            }),
        }
    }

    /// The whole recording: pieces until something ends it, then the file.
    async fn record(self, ctx: JobCtx) -> Result<ToolOutput, ToolError> {
        let frame = (self.plan.region.w as u32, self.plan.region.h as u32);
        let began = Instant::now();
        let mut got = Gathered::default();
        let mut index = 0;
        let stop = 'rec: loop {
            // Wait until recording may run.
            let region = loop {
                if ctx.cancel.is_cancelled() {
                    break 'rec Stop::User;
                }
                if !self.screen_on.load(Ordering::Relaxed) {
                    break 'rec Stop::ScreenOff;
                }
                if began.elapsed().as_secs_f64() > self.plan.limit_s + PAUSE_CAP_S {
                    break 'rec Stop::PausedTooLong;
                }
                match self.region_now(&self.see()) {
                    Ok(region) => break region,
                    Err(Hold::Gone) => break 'rec Stop::WindowGone,
                    Err(Hold::Blind) => ctx.handle.set_note(NOTE_BLIND),
                    Err(Hold::Behind) => ctx.handle.set_note(NOTE_BEHIND),
                }
                tokio::select! {
                    _ = tokio::time::sleep(POLL) => {}
                    _ = ctx.cancel.cancelled() => {}
                }
            };
            if self.plan.limit_s - got.recorded < 1.0 {
                break 'rec Stop::Limit;
            }
            if let Some(stop) = self.piece(index, region, frame, &mut got, &ctx).await {
                break 'rec stop;
            }
            index += 1;
        };
        // A stop is not an abort: what was recorded becomes a file.
        ctx.handle.set_note(NOTE_SAVING);
        self.save(got, stop, &ctx.handle).await
    }

    /// Where the pieces stay when the file was not made, in words. Pieces a
    /// blind-zone window ended are not among them: `RawPieces` removes them.
    fn left_text(&self, pauses: u32) -> String {
        let mut text = format!("Куски остались в {}", self.work.display());
        if pauses > 0 {
            text.push_str("; куски, которые оборвало окно из слепой зоны, удалены");
        }
        text
    }

    /// Puts the pieces and their sound into the result file.
    async fn save(
        &self,
        got: Gathered,
        stop: Stop,
        handle: &JobHandle,
    ) -> Result<ToolOutput, ToolError> {
        if got.pieces.is_empty() {
            remove_later(self.work.clone(), true);
            return Err(failed(format!(
                "ничего не записалось: {}",
                nothing_text(&stop)
            )));
        }
        // If this work is dropped before the file is ready (it had
        // `stop_grace` after the stop), the job ends with these words.
        handle.set_partial(
            &format!("{NOT_SAVED} {}.", self.left_text(got.pauses)),
            None,
        );
        let broke = |e: ToolError| {
            let why = match e {
                ToolError::Failed(m) | ToolError::BadArguments(m) => m,
                other => other.to_string(),
            };
            // The error tells it all: nothing is added to it.
            handle.set_partial("", None);
            // The pieces are the user's recording: they stay where they are.
            failed(format!(
                "не удалось собрать запись: {}. {}",
                why.trim_end_matches('.'),
                self.left_text(got.pauses)
            ))
        };
        // The job was told to stop long ago: the putting together has a
        // token of its own.
        let cancel = CancellationToken::new();
        let runner = self.av.runner.as_ref();
        let quiet = Progress::default();
        let total: f64 = got.pieces.iter().map(|p| p.keep).sum();
        let part = PartFile::new(&self.plan.output);
        if let [one] = got.pieces.as_slice() {
            let argv = mux_args(&one.video, one.keep, &one.sounds, part.path());
            run_ffmpeg(
                runner,
                &self.paths,
                &argv,
                None,
                one.keep,
                (0, 100),
                &quiet,
                &cancel,
                copy_timeout(one.keep),
            )
            .await
            .map_err(broke)?;
        } else {
            let mut list = String::new();
            for (i, piece) in got.pieces.iter().enumerate() {
                let out = self.work.join(format!("part-{i}.mkv"));
                let argv = mux_args(&piece.video, piece.keep, &piece.sounds, &out);
                run_ffmpeg(
                    runner,
                    &self.paths,
                    &argv,
                    None,
                    piece.keep,
                    (0, 100),
                    &quiet,
                    &cancel,
                    copy_timeout(piece.keep),
                )
                .await
                .map_err(broke)?;
                list.push_str(&concat_line(&out));
                list.push('\n');
            }
            let list_path = self.work.join("parts.txt");
            std::fs::write(&list_path, list)
                .map_err(|e| broke(failed(format!("не удалось записать список кусков: {e}"))))?;
            let argv = join_list_args(&list_path, "mp4", part.path());
            run_ffmpeg(
                runner,
                &self.paths,
                &argv,
                None,
                total,
                (0, 100),
                &quiet,
                &cancel,
                copy_timeout(total),
            )
            .await
            .map_err(broke)?;
        }
        let (done, info) = self
            .av
            .finish(&self.paths, part, &self.plan.output, &cancel)
            .await
            .map_err(broke)?;
        remove_later(self.work.clone(), true);
        let bytes = std::fs::metadata(&done).map(|m| m.len()).unwrap_or(0);
        let length = info.and_then(|i| i.duration_s).unwrap_or(total);
        let text = done_text(
            &done,
            bytes,
            length,
            self.plan.limit_s,
            got.pauses,
            &stop,
            got.sound_note.as_deref(),
        );
        Ok(ToolOutput::new(text).with_data(files_card(&[(done, Some(length))])))
    }
}

#[async_trait]
impl Tool for ScreenRecordTool {
    fn spec(&self) -> ToolSpec {
        spec(
            KIND,
            "Начать запись экрана или одного окна в видеофайл, со звуком компьютера. Запись идёт в фоне, пока её не остановят (screen_record_stop) или не кончится время. microphone ставь, только если пользователь в этой просьбе прямо попросил записать микрофон или свой голос.",
            schema(
                &[
                    (
                        "window",
                        "string",
                        "Часть заголовка окна или имя программы; без него — весь экран",
                    ),
                    (
                        "monitor",
                        "integer",
                        "Какой монитор писать, когда окно не названо: 1 — основной, дальше слева направо",
                    ),
                    (
                        "minutes",
                        "number",
                        "Предел длины в минутах, от 1 до 120; без него 30",
                    ),
                    ("sound", "boolean", "false — без звука компьютера"),
                    (
                        "microphone",
                        "boolean",
                        "true — писать ещё и микрофон; только по прямой просьбе пользователя",
                    ),
                ],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        Ok(Self::assessment(&self.prepare(args)?))
    }

    async fn run(
        &self,
        args: Value,
        approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let plan = self.prepare(&args)?;
        let summary = Self::assessment(&plan).summary;
        let paths = self.av.ffmpeg()?;
        let probe = self.probe.clone();
        let disks = tokio::task::spawn_blocking(move || probe.facts().disks)
            .await
            .unwrap_or_default();
        if let Some((mount, free)) = free_on(&disks, &plan.output) {
            if free < MIN_FREE_START_GB {
                return Err(failed(format!(
                    "на диске {mount} меньше 2 ГБ свободного места: запись не начинаю"
                )));
            }
        }
        let runner = self.av.runner.as_ref();
        if !devices(runner, &paths, &cancel).await?.contains("gdigrab") {
            return Err(failed(NO_GDIGRAB));
        }
        let h264 = pick_h264(runner, &paths, &cancel).await?;
        let folder = videos_dir(&self.av.user_profile);
        std::fs::create_dir_all(&folder).map_err(|e| {
            failed(format!(
                "не удалось создать папку {}: {e}",
                folder.display()
            ))
        })?;
        // No recording runs now: what an earlier one left behind by force
        // is looked over before a new folder of pieces appears.
        sweep_blind(&folder);
        let work = work_dir(&plan.output);
        std::fs::create_dir(&work)
            .map_err(|e| failed(format!("не удалось создать папку {}: {e}", work.display())))?;

        let what = match &plan.source {
            Source::Screen { monitor } => format!("монитор {monitor}"),
            Source::Window { process, .. } => format!("окно {}", app_name(process)),
        };
        let minutes = number(plan.limit_s / 60.0).replace('.', ",");
        let output = plan.output.clone();
        let grace = grace_for(plan.limit_s);
        let recorder = Recorder {
            av: self.av.clone(),
            windows: self.ui.windows.clone(),
            screen_on: self.screen_on.clone(),
            probe: self.probe.clone(),
            paths,
            h264,
            plan,
            work: work.clone(),
            _raw: RawPieces(work.clone()),
        };
        let job: JobWork = Box::new(move |ctx: JobCtx| {
            Box::pin(async move {
                // The approval of this very recording; it ends with it.
                let _approval = approval;
                recorder.record(ctx).await
            })
        });
        let submitted = match self
            .av
            .jobs
            .submit(JobSpec::live(KIND, summary, grace), job)
        {
            Ok(s) => s,
            Err(e) => {
                let _ = std::fs::remove_dir(&work);
                return Err(failed(e.to_string()));
            }
        };
        forget_planned(&output);
        // Starts like every background job: the agent then does not take
        // «скажу, когда закончится» for a stalled turn.
        let text = format!(
            "{} запись экрана ({what}), до {minutes} мин; файл будет в {}. Остановить — screen_record_stop или кнопка на карточке. Не проси подождать: скажи пользователю коротко, что запись началась.",
            mika_core::bgjobs::STARTED_PREFIX,
            folder.display()
        );
        Ok(ToolOutput::new(text).with_data(job_card(&submitted.job)))
    }
}

/// Tool `screen_record_stop`: ends the recording; the file is saved.
pub struct ScreenRecordStopTool(pub AvEnv);

#[async_trait]
impl Tool for ScreenRecordStopTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "screen_record_stop",
            "Остановить запись экрана и сохранить файл.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        let job = recording(&self.0.jobs).ok_or_else(|| bad("запись сейчас не идёт"))?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Остановить запись экрана",
            job.title,
            "запись сохранится в файл",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let jobs = &self.0.jobs;
        let id = recording(jobs)
            .ok_or_else(|| bad("запись сейчас не идёт"))?
            .id;
        // This call tells how it ended: the app keeps quiet about it.
        jobs.set_silent(id, true);
        if let Err(e) = jobs.stop(id) {
            return Err(failed(e.to_string()));
        }
        let ended = tokio::select! {
            v = jobs.wait(id, STOP_WAIT) => v,
            _ = cancel.cancelled() => None,
        };
        let Some(ended) = ended else {
            jobs.set_silent(id, false);
            return Ok(ToolOutput::new(
                "Запись остановлена, файл ещё собирается: скажу, когда он будет готов.",
            ));
        };
        match ended.state {
            JobState::Failed => return Err(failed(ended.text)),
            // Its work was dropped before the file was ready: no file, and
            // the model must not say there is one.
            JobState::Stopped if ended.text.trim() == STOPPED_TEXT => {
                return Err(failed(NOT_SAVED));
            }
            JobState::Stopped => return Err(failed(ended.text)),
            _ => {}
        }
        let mut out = ToolOutput::new(ended.text);
        out.data = ended.data;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use mika_core::bgjobs::STARTED_PREFIX;
    use mika_core::confirm::{voice_answer, Answer, VoiceAnswer};
    use mika_core::policy::blind::BLIND_REFUSAL;
    use mika_core::policy::{forever_block, HIGH_RISK_FOREVER};

    use super::super::testkit::{av, Av, X264_LIST};
    use super::*;
    use crate::sys::{RunOutput, SystemFacts, WindowInfo};
    use crate::testing::AFTER_QUIT;

    const WAIT: Duration = Duration::from_secs(20);
    const DEVICES: &str = "Devices:\n D. = Demuxing supported\n .E = Muxing supported\n ---\n D  dshow  DirectShow capture\n D  gdigrab  GDI API Windows frame grabber\n";

    fn window(id: u64, title: &str, process: &str) -> WindowInfo {
        WindowInfo {
            id,
            title: title.into(),
            process: process.into(),
        }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    /// A window whose title holds line breaks, as a page may set it.
    fn window_of_lines() -> WindowInfo {
        window(
            5,
            "Отчёт\nМикрофон: не записывается\r\nЗвук компьютера: не записывается",
            "chrome.exe",
        )
    }

    /// The tool with the «Экран» switch on and the user's folder inside the test's own.
    fn tool(t: &Av) -> ScreenRecordTool {
        let mut av = t.av();
        av.user_profile = t.dir.path().join("home").display().to_string();
        t.env.shared.screen_enabled.store(true, Ordering::Relaxed);
        ScreenRecordTool {
            av,
            ui: UiEnv {
                windows: t.env.windows.clone(),
                ui: t.env.ui.clone(),
                input: t.env.input.clone(),
                bus: t.env.shared.bus.clone(),
                own_process: "mika.exe".into(),
                seen: Default::default(),
                clipboard: None,
                sent: Default::default(),
                clips: Default::default(),
            },
            screen_on: t.env.shared.screen_enabled.clone(),
            probe: t.env.probe.clone(),
        }
    }

    /// Answers of FFmpeg to the questions before a recording, then one
    /// answer per piece: how many seconds it wrote.
    fn ready(t: &Av, pieces: &[f64]) {
        t.answer(DEVICES);
        t.answer(X264_LIST);
        for seconds in pieces {
            t.answer(&format!(
                "out_time_us={}\nprogress=continue",
                (seconds * 1e6) as u64
            ));
        }
    }

    /// Runs of FFmpeg that capture the screen.
    fn captures(t: &Av) -> Vec<Vec<String>> {
        t.ffmpeg_runs()
            .into_iter()
            .filter(|a| a.contains(&"gdigrab".to_string()))
            .collect()
    }

    /// Runs of FFmpeg that put pieces together.
    fn muxes(t: &Av) -> Vec<String> {
        t.ffmpeg_runs()
            .into_iter()
            .filter(|a| a.contains(&"-nostdin".to_string()) && a.contains(&"copy".to_string()))
            .map(|a| a.join(" "))
            .collect()
    }

    async fn until(what: &str, done: impl Fn() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < WAIT, "waited too long for {what}");
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    async fn start(tool: &ScreenRecordTool, args: Value) -> u64 {
        tool.assess(&args).unwrap();
        let out = tool
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        assert!(out.text.starts_with(STARTED_PREFIX), "{}", out.text);
        let card = out.data.unwrap();
        assert_eq!(card["kind"], "bg_job");
        assert_eq!(card["job"]["kind"], "screen_record");
        assert_eq!(card["job"]["live"], true);
        assert_eq!(card["job"]["state"], "running");
        card["job"]["id"].as_u64().unwrap()
    }

    async fn stop(tool: &ScreenRecordTool) -> Result<ToolOutput, ToolError> {
        let stopper = ScreenRecordStopTool(tool.av.clone());
        stopper.assess(&json!({}))?;
        stopper
            .run(json!({}), Approval::for_tests(), CancellationToken::new())
            .await
    }

    fn refusal(tool: &ScreenRecordTool, args: Value) -> ToolError {
        match tool.assess(&args) {
            Err(e) => e,
            Ok(a) => panic!("not refused: {a:?}"),
        }
    }

    #[test]
    fn nothing_is_recorded_while_the_switch_is_off() {
        let t = av();
        let rec = tool(&t);
        t.env.shared.screen_enabled.store(false, Ordering::Relaxed);
        assert_eq!(
            refusal(&rec, json!({})),
            ToolError::Failed(SCREEN_DISABLED.into())
        );
        t.env.shared.screen_enabled.store(true, Ordering::Relaxed);
        assert!(rec.assess(&json!({})).is_ok());
    }

    #[test]
    fn a_blind_window_in_front_or_as_the_target_refuses_the_start() {
        let t = av();
        let rec = tool(&t);
        let bank = window(7, "Сбербанк Онлайн — вход в личный кабинет", "chrome.exe");
        let notes = window(8, "Заметки — Блокнот", "notepad.exe");
        *t.f.windows.lock() = vec![bank.clone(), notes.clone()];
        t.f.window_bounds.lock().insert(7, rect(0, 0, 800, 600));
        t.f.window_bounds.lock().insert(8, rect(0, 0, 800, 600));

        // In front: the whole screen is not recorded, nor any other window.
        *t.f.foreground.lock() = Some(bank);
        for args in [json!({}), json!({ "window": "Блокнот" })] {
            match refusal(&rec, args) {
                ToolError::Failed(m) => {
                    assert_eq!(m, BLIND_START);
                    assert!(!m.contains("Сбербанк"), "the title is not told");
                }
                other => panic!("{other:?}"),
            }
        }
        // As the target, with another window in front.
        *t.f.foreground.lock() = Some(notes);
        assert_eq!(
            refusal(&rec, json!({ "window": "Сбербанк" })),
            ToolError::Failed(BLIND_REFUSAL.into())
        );
        // Not in front, but on the screen: it would be in the picture of
        // the whole screen and of the window it lies under.
        for args in [json!({}), json!({ "window": "Блокнот" })] {
            assert_eq!(refusal(&rec, args), ToolError::Failed(BLIND_START.into()));
        }
        // Beside the recorded window it is not in its picture.
        t.f.window_bounds.lock().insert(7, rect(900, 0, 800, 600));
        assert!(rec.assess(&json!({ "window": "Блокнот" })).is_ok());
        assert_eq!(
            refusal(&rec, json!({})),
            ToolError::Failed(BLIND_START.into())
        );
        // On the other monitor the first one may be recorded.
        *t.f.monitor_bounds.lock() = vec![rect(0, 0, 1920, 1080), rect(1920, 0, 1920, 1080)];
        t.f.window_bounds.lock().insert(7, rect(2000, 0, 800, 600));
        assert!(rec.assess(&json!({})).is_ok());
        assert_eq!(
            refusal(&rec, json!({ "monitor": 2 })),
            ToolError::Failed(BLIND_START.into())
        );
        // Minimized, it is nowhere on the screen.
        t.f.window_bounds.lock().remove(&7);
        assert!(rec.assess(&json!({ "monitor": 2 })).is_ok());
        assert!(rec.assess(&json!({ "window": "Блокнот" })).is_ok());
    }

    #[test]
    fn arguments_and_switches_are_checked() {
        let t = av();
        let rec = tool(&t);
        let bad_args = |args: Value| match refusal(&rec, args) {
            ToolError::BadArguments(m) => m,
            other => panic!("{other:?}"),
        };
        for minutes in [json!(0), json!(0.5), json!(121), json!("много"), json!(-3)] {
            let m = bad_args(json!({ "minutes": minutes }));
            assert!(
                m == "minutes — от 1 до 120" || m == "`minutes` — число",
                "{m}"
            );
        }
        assert!(rec.assess(&json!({ "minutes": "1,5" })).is_ok());
        assert!(rec.assess(&json!({ "minutes": 120 })).is_ok());
        assert_eq!(bad_args(json!({ "monitor": 3 })), "монитора 3 нет: их 1");
        assert_eq!(
            bad_args(json!({ "monitor": 0 })),
            "monitor — номер монитора, от 1"
        );
        *t.f.monitor_bounds.lock() = vec![rect(0, 0, 1920, 1080), rect(1920, 0, 2560, 1440)];
        assert!(rec.assess(&json!({ "monitor": 2 })).is_ok());

        // The microphone needs its own switch.
        match refusal(&rec, json!({ "microphone": true })) {
            ToolError::Failed(m) => assert_eq!(m, MIC_DISABLED),
            other => panic!("{other:?}"),
        }
        assert!(rec.assess(&json!({ "microphone": false })).is_ok());
        t.f.mic_allowed.store(true, Ordering::Relaxed);
        assert!(rec.assess(&json!({ "microphone": true })).is_ok());

        // Windows: a tiny one, a minimized one, Mika's own, an unknown one.
        *t.f.windows.lock() = vec![
            window(1, "Крошка", "tiny.exe"),
            window(2, "Свёрнутое", "min.exe"),
            window(3, "Мика", "mika.exe"),
        ];
        t.f.window_bounds.lock().insert(1, rect(10, 10, 40, 40));
        t.f.window_bounds.lock().insert(3, rect(0, 0, 500, 500));
        assert_eq!(
            bad_args(json!({ "window": "Крошка" })),
            "область записи слишком мала"
        );
        assert_eq!(
            bad_args(json!({ "window": "Свёрнутое" })),
            "окно свёрнуто или скрыто"
        );
        assert!(matches!(
            refusal(&rec, json!({ "window": "Мика" })),
            ToolError::Failed(m) if m.starts_with("нет окна «Мика»")
        ));
    }

    #[test]
    fn the_card_is_always_a_high_risk_and_names_everything_recorded() {
        let t = av();
        let rec = tool(&t);
        let a = rec.assess(&json!({})).unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(a.summary, "Записать экран");
        assert_eq!(a.scope, "экран");
        assert_eq!(a.reason, REASON);
        assert_eq!(forever_block(a.level, &a.scope), Some(HIGH_RISK_FOREVER));
        let lines: Vec<&str> = a.target.lines().collect();
        assert_eq!(lines.len(), 5);
        // What is taken from the user comes first, whatever follows.
        assert_eq!(lines[0], "Микрофон: не записывается");
        assert_eq!(lines[1], "Звук компьютера: записывается");
        assert_eq!(lines[2], "Длина: до 30 минут");
        assert_eq!(lines[3], "Экран: монитор 1, 1920×1080");
        let folder = t.dir.path().join("home").join("Videos").join("Mika");
        assert!(
            lines[4].starts_with(&format!("Файл: {}\\Запись экрана 20", folder.display()))
                && lines[4].ends_with(".mp4"),
            "{}",
            lines[4]
        );
        // The card and the run that follows name the same file.
        assert_eq!(rec.assess(&json!({})).unwrap().target, a.target);
        assert!(!folder.exists(), "the card makes nothing");

        // Said aloud, the name of the action approves it; «да» does not.
        assert_eq!(
            voice_answer("записать экран", &a),
            VoiceAnswer::Answer(Answer::Once)
        );
        assert_eq!(
            voice_answer("да", &a),
            VoiceAnswer::NeedName("Записать экран".into())
        );

        // With the microphone it is the same risk, told on its own line.
        t.f.mic_allowed.store(true, Ordering::Relaxed);
        let a = rec
            .assess(&json!({ "microphone": true, "sound": false, "minutes": 1 }))
            .unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(a.summary, "Записать экран");
        let lines: Vec<&str> = a.target.lines().collect();
        assert_eq!(
            &lines[..3],
            [
                "Микрофон: записывается",
                "Звук компьютера: не записывается",
                "Длина: до 1 минуты"
            ]
        );

        // A window: its program is the name and the scope.
        let long = "Отчёт за третий квартал — очень длинное название вкладки, которое не влезает в строку — Google Chrome";
        *t.f.windows.lock() = vec![window(5, long, "chrome.exe")];
        t.f.window_bounds.lock().insert(5, rect(101, 51, 1001, 701));
        let a = rec
            .assess(&json!({ "window": "chrome", "microphone": true }))
            .unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(a.summary, "Записать окно chrome");
        assert_eq!(a.scope, "chrome.exe");
        assert_eq!(a.reason, format!("{REASON}; {REASON_WINDOW}"));
        let lines: Vec<&str> = a.target.lines().collect();
        assert_eq!(lines.len(), 5, "{}", a.target);
        // The title is the page's own text: it stands below the lines about
        // the microphone and the sound and cannot push them away.
        assert_eq!(lines[0], "Микрофон: записывается");
        assert_eq!(lines[1], "Звук компьютера: записывается");
        let shown = lines[3];
        assert!(
            shown.starts_with("Окно: «Отчёт за третий квартал"),
            "{shown}"
        );
        assert!(shown.ends_with("» (chrome.exe), 1000×700"), "{shown}");
        assert!(
            !shown.contains("Google Chrome"),
            "the title is cut: {shown}"
        );
        assert!(shown.chars().count() <= 100, "{shown}");
        // A title with line breaks stays one line of the card.
        *t.f.windows.lock() = vec![window_of_lines()];
        let a = rec.assess(&json!({ "window": "chrome" })).unwrap();
        assert_eq!(a.target.lines().count(), 5, "{}", a.target);
        *t.f.windows.lock() = vec![window(5, long, "chrome.exe")];
        let a = rec.assess(&json!({ "window": "chrome" })).unwrap();
        assert_eq!(
            voice_answer("записать окно chrome", &a),
            VoiceAnswer::Answer(Answer::Once)
        );
    }

    #[test]
    fn helpers_read_progress_disks_and_complaints() {
        assert_eq!(out_time_us("out_time_us=5000000"), Some(5_000_000));
        assert_eq!(out_time_us(" out_time_ms=42 "), Some(42));
        assert_eq!(out_time_us("out_time_us=N/A"), None);
        assert_eq!(out_time_us("out_time=00:00:05.000000"), None);
        assert_eq!(out_time_us("progress=continue"), None);

        let disks = vec![
            ("C:\\".to_string(), 100.0, 476.0),
            ("C:\\Mount\\Video\\".to_string(), 1.0, 2000.0),
            ("D:\\".to_string(), 0.2, 931.0),
        ];
        let free = |p: &str| free_on(&disks, Path::new(p));
        assert_eq!(
            free("C:\\Users\\me\\Videos\\x.mp4"),
            Some(("C:\\".into(), 100.0))
        );
        assert_eq!(
            free("c:\\mount\\video\\x.mp4"),
            Some(("C:\\Mount\\Video\\".into(), 1.0)),
            "the longest mount wins"
        );
        assert_eq!(free("D:\\x.mp4"), Some(("D:\\".into(), 0.2)));
        assert_eq!(
            free("\\\\server\\share\\x.mp4"),
            None,
            "unknown is not a refusal"
        );

        assert_eq!(complaint("a\n\nb\nc\nd\n"), "b; c; d");
        assert_eq!(complaint("  \n"), "захват экрана завершился с ошибкой");
        assert_eq!(app_name("chrome.exe"), "chrome");
        assert_eq!(app_name("code"), "code");
    }

    #[tokio::test]
    async fn a_recording_runs_until_it_is_stopped_and_becomes_a_file() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        ready(&t, &[5.0]);
        let shown = rec.assess(&json!({})).unwrap().target;
        let id = start(&rec, json!({})).await;
        until("the capture", || captures(&t).len() == 1).await;
        let job = rec.av.jobs.get(id).unwrap();
        assert!(job.live && job.state == JobState::Running);
        assert_eq!(job.title, "Записать экран");

        // A second recording is refused while this one runs.
        assert_eq!(
            refusal(&rec, json!({})),
            ToolError::BadArguments(
                "запись уже идёт: сначала останови её (screen_record_stop)".into()
            )
        );
        let argv = captures(&t).remove(0);
        let line = argv.join(" ");
        assert!(
            line.contains("-f gdigrab -framerate 30 -draw_mouse 1 -offset_x 0 -offset_y 0 -video_size 1920x1080 -i desktop -t 1800.000"),
            "{line}"
        );
        assert!(line.contains("seg-0.mkv"), "{line}");
        // Only the sound of the computer: nobody asked for the microphone.
        let takes = t.f.sound_takes.lock().clone();
        assert_eq!(takes.len(), 1);
        assert_eq!(takes[0].0, SoundSource::System);
        assert!(takes[0].1.ends_with("seg-0-sys.wav"));
        let work = takes[0].1.parent().unwrap().to_path_buf();
        assert!(work.to_string_lossy().ends_with(".mika-part") && work.is_dir());

        let stopper = ScreenRecordStopTool(rec.av.clone());
        let a = stopper.assess(&json!({})).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert_eq!(a.summary, "Остановить запись экрана");
        assert_eq!(a.target, "Записать экран");
        assert_eq!(a.reason, "запись сохранится в файл");

        let out = stop(&rec).await.unwrap();
        let file = PathBuf::from(shown.lines().last().unwrap().trim_start_matches("Файл: "));
        assert!(
            file.is_file(),
            "the name the card showed: {}",
            file.display()
        );
        assert_eq!(
            out.text,
            format!("Запись сохранена: {}, 1 КБ, 0:05.", file.display())
        );
        let card = out.data.unwrap();
        assert_eq!(card["kind"], "av_files");
        assert_eq!(card["files"][0]["path"], file.display().to_string());
        assert_eq!(card["files"][0]["duration_s"], 5.0);
        assert!(rec.av.outputs.contains(&file), "it opens from the card");
        let job = rec.av.jobs.get(id).unwrap();
        assert_eq!(job.state, JobState::Done);
        assert!(job.silent, "the stop call told about the end itself");
        assert_eq!(job.data, Some(card));

        // One piece goes straight into the MP4, with its sound.
        let muxes = muxes(&t);
        assert_eq!(muxes.len(), 1, "{muxes:?}");
        assert!(muxes[0].contains("seg-0.mkv") && muxes[0].contains("seg-0-sys.wav"));
        assert!(muxes[0].contains("-t 5.000 -map 0:v:0 -c:v copy -map 1:a:0 -c:a aac"));
        assert!(muxes[0].contains(".mika-part.mp4"), "{}", muxes[0]);
        // The pieces are gone and nothing is left half-written.
        super::super::wait_cleanup(Duration::from_secs(5));
        assert!(!work.exists());
        assert!(
            rec.assess(&json!({})).is_ok(),
            "a new recording may be asked for again"
        );
        assert_eq!(
            stopper.assess(&json!({})),
            Err(ToolError::BadArguments("запись сейчас не идёт".into()))
        );
    }

    #[tokio::test]
    async fn a_blind_window_pauses_the_recording_and_its_tail_is_cut() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        t.answer(DEVICES);
        t.answer(X264_LIST);
        // FFmpeg is slow to end: told to finish at 5 s, it writes nine
        // seconds more, and all of them show the window.
        t.answer(&format!(
            "out_time_us=5000000\nprogress=continue\n{AFTER_QUIT}out_time_us=14000000\nprogress=end"
        ));
        t.answer("out_time_us=3000000\nprogress=continue");
        let id = start(&rec, json!({})).await;
        until("the first piece", || captures(&t).len() == 1).await;

        *t.f.foreground.lock() = Some(window(9, "KeePass — пароли", "keepass.exe"));
        until("the pause", || {
            rec.av.jobs.get(id).unwrap().note == NOTE_BLIND
        })
        .await;
        assert_eq!(captures(&t).len(), 1, "nothing is recorded meanwhile");
        assert_eq!(t.f.sound_takes.lock().len(), 1, "the sound waits too");
        tokio::time::sleep(POLL * 2).await;
        assert_eq!(captures(&t).len(), 1);

        *t.f.foreground.lock() = Some(window(4, "Блокнот", "notepad.exe"));
        until("the second piece", || captures(&t).len() == 2).await;
        assert!(captures(&t)[1].join(" ").contains("seg-1.mkv"));
        // The limit counts what FFmpeg wrote: 30 minutes less the 14 seconds.
        assert!(captures(&t)[1].join(" ").contains("-t 1786.000"));
        // The piece is marked until the file is put together.
        let work = t.f.sound_takes.lock()[0].1.parent().unwrap().to_path_buf();
        assert!(blind_mark(&work, 0).is_file());

        let out = stop(&rec).await.unwrap();
        assert!(
            out.text.contains(
                "Запись 1 раз вставала на паузу из-за окна из слепой зоны: эти места в файл не попали."
            ),
            "{}",
            out.text
        );
        assert!(!out.text.contains("KeePass"), "{}", out.text);
        // 5 s less the cut tail, and 3 s: the pieces are joined without encoding.
        assert!(out.text.contains(", 0:07."), "{}", out.text);
        let muxes = muxes(&t);
        assert_eq!(muxes.len(), 3, "{muxes:?}");
        // Cut from the moment the window was noticed, not from the end of
        // what FFmpeg wrote.
        assert!(
            muxes[0].contains("seg-0.mkv") && muxes[0].contains("-t 4.250"),
            "{}",
            muxes[0]
        );
        assert!(muxes[0].contains("part-0.mkv"));
        assert!(muxes[1].contains("seg-1.mkv") && muxes[1].contains("-t 3.000"));
        assert!(muxes[2].contains("-f concat -safe 0") && muxes[2].contains("parts.txt"));
        assert_eq!(out.data.unwrap()["kind"], "av_files");
        assert_eq!(rec.av.jobs.get(id).unwrap().state, JobState::Done);
    }

    #[tokio::test]
    async fn the_limit_ends_a_recording_by_itself() {
        let t = av();
        let rec = tool(&t);
        // FFmpeg writes its minute and ends.
        ready(&t, &[60.0]);
        let id = start(&rec, json!({ "minutes": 1, "sound": false })).await;
        let job = rec.av.jobs.wait(id, WAIT).await.unwrap();
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        assert!(
            !job.silent,
            "nobody stopped it: the app tells about the end"
        );
        assert!(
            job.text
                .ends_with(", 1:00. Остановлена: достигнут предел 1 минуты."),
            "{}",
            job.text
        );
        assert!(t.f.sound_takes.lock().is_empty(), "sound was not asked for");
        let muxes = muxes(&t);
        assert!(muxes[0].contains("-c:v copy -an"), "{}", muxes[0]);
        assert_eq!(
            stop(&rec).await,
            Err(ToolError::BadArguments("запись сейчас не идёт".into()))
        );
    }

    #[tokio::test]
    async fn the_microphone_is_recorded_only_when_asked_and_while_allowed() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        t.f.mic_allowed.store(true, Ordering::Relaxed);
        ready(&t, &[4.0, 2.0]);
        start(&rec, json!({ "microphone": true })).await;
        until("the first piece", || captures(&t).len() == 1).await;
        let sources = |t: &Av| -> Vec<SoundSource> {
            t.f.sound_takes.lock().iter().map(|(s, _)| *s).collect()
        };
        assert_eq!(sources(&t), [SoundSource::System, SoundSource::Microphone]);

        // The switch goes off: the piece ends and the next one has no microphone.
        t.f.mic_allowed.store(false, Ordering::Relaxed);
        until("the second piece", || captures(&t).len() == 2).await;
        assert_eq!(
            sources(&t),
            [
                SoundSource::System,
                SoundSource::Microphone,
                SoundSource::System
            ]
        );
        let out = stop(&rec).await.unwrap();
        assert!(!out.text.contains("паузу"), "{}", out.text);
        let muxes = muxes(&t);
        assert!(
            muxes[0].contains("seg-0-sys.wav")
                && muxes[0].contains("seg-0-mic.wav")
                && muxes[0].contains("amix=inputs=2"),
            "{}",
            muxes[0]
        );
        assert!(muxes[1].contains("seg-1-sys.wav") && !muxes[1].contains("mic"));
    }

    #[tokio::test]
    async fn a_sound_take_that_fails_still_leaves_a_video() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        *t.f.sound_error.lock() = Some("нет устройства вывода звука".into());
        ready(&t, &[5.0]);
        start(&rec, json!({})).await;
        until("the capture", || captures(&t).len() == 1).await;
        let out = stop(&rec).await.unwrap();
        assert!(
            out.text.ends_with(
                ", 0:05. Звук компьютера записать не удалось: нет устройства вывода звука."
            ),
            "{}",
            out.text
        );
        assert!(muxes(&t)[0].contains("-c:v copy -an"));
        assert_eq!(out.data.unwrap()["kind"], "av_files");
    }

    #[tokio::test]
    async fn switching_the_screen_off_ends_and_saves_the_recording() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        ready(&t, &[8.0]);
        let id = start(&rec, json!({})).await;
        until("the capture", || captures(&t).len() == 1).await;
        t.env.shared.screen_enabled.store(false, Ordering::Relaxed);
        let job = rec.av.jobs.wait(id, WAIT).await.unwrap();
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        assert!(
            job.text
                .ends_with(", 0:08. Остановлена: выключен просмотр экрана."),
            "{}",
            job.text
        );
    }

    #[tokio::test]
    async fn a_window_is_recorded_while_its_program_is_in_front() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        let chrome = window(5, "Отчёт — Google Chrome", "chrome.exe");
        let other = window(6, "Проводник", "explorer.exe");
        *t.f.windows.lock() = vec![chrome.clone(), other.clone()];
        t.f.window_bounds.lock().insert(5, rect(100, 50, 800, 600));
        *t.f.foreground.lock() = Some(chrome.clone());
        ready(&t, &[5.0, 4.0, 3.0]);
        let args = json!({ "window": "chrome", "sound": false });
        let id = start(&rec, args).await;
        assert_eq!(rec.av.jobs.get(id).unwrap().title, "Записать окно chrome");
        until("the first piece", || captures(&t).len() == 1).await;
        let line = captures(&t)[0].join(" ");
        assert!(
            line.contains("-offset_x 100 -offset_y 50 -video_size 800x600 -i desktop"),
            "{line}"
        );
        assert!(
            !line.contains("Отчёт") && !line.contains("Chrome"),
            "{line}"
        );

        // Another program in front: a pause without a cut.
        *t.f.foreground.lock() = Some(other);
        until("the pause", || {
            rec.av.jobs.get(id).unwrap().note == NOTE_BEHIND
        })
        .await;
        // Back in front, moved and grown: the new place, the first picture size.
        t.f.window_bounds
            .lock()
            .insert(5, rect(300, 200, 1000, 700));
        *t.f.foreground.lock() = Some(chrome.clone());
        until("the second piece", || captures(&t).len() == 2).await;
        let line = captures(&t)[1].join(" ");
        assert!(
            line.contains("-offset_x 300 -offset_y 200 -video_size 1000x700 -i desktop")
                && line.contains("-vf scale=800:600:"),
            "{line}"
        );
        // Moved while recording: the piece ends and a new one follows.
        t.f.window_bounds.lock().insert(5, rect(0, 0, 1000, 700));
        until("the third piece", || captures(&t).len() == 3).await;
        assert!(captures(&t)[2]
            .join(" ")
            .contains("-offset_x 0 -offset_y 0"));

        // The window is closed: the recording ends by itself and is saved.
        t.f.windows.lock().retain(|w| w.id != 5);
        t.f.window_bounds.lock().remove(&5);
        let job = rec.av.jobs.wait(id, WAIT).await.unwrap();
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        assert!(
            job.text.ends_with(", 0:12. Остановлена: окно закрыто."),
            "no cut for a pause that was no blind zone: {}",
            job.text
        );
    }

    #[tokio::test]
    async fn a_blind_window_behind_the_one_in_front_pauses_the_recording_too() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        ready(&t, &[6.0, 2.0]);
        let notes = window(4, "Блокнот", "notepad.exe");
        let bank = window(7, "Сбербанк Онлайн — вход", "chrome.exe");
        *t.f.windows.lock() = vec![notes.clone()];
        t.f.window_bounds.lock().insert(4, rect(0, 0, 600, 400));
        *t.f.foreground.lock() = Some(notes.clone());
        let id = start(&rec, json!({ "sound": false })).await;
        until("the first piece", || captures(&t).len() == 1).await;

        // The bank page opens beside the notepad and the user clicks back
        // into the notepad: the page is not in front and is on the screen.
        t.f.windows.lock().push(bank);
        t.f.window_bounds.lock().insert(7, rect(700, 100, 900, 700));
        until("the pause", || {
            rec.av.jobs.get(id).unwrap().note == NOTE_BLIND
        })
        .await;
        tokio::time::sleep(POLL * 3).await;
        assert_eq!(captures(&t).len(), 1, "nothing is recorded while it shows");
        // Mika's own island in front changes nothing either.
        *t.f.foreground.lock() = Some(window(1, "Мика", "mika.exe"));
        tokio::time::sleep(POLL * 3).await;
        assert_eq!(captures(&t).len(), 1);

        // Minimized, it is off the screen: the recording goes on.
        t.f.window_bounds.lock().remove(&7);
        until("the second piece", || captures(&t).len() == 2).await;
        let out = stop(&rec).await.unwrap();
        assert!(
            out.text.contains("Запись 1 раз вставала на паузу"),
            "{}",
            out.text
        );
        assert!(!out.text.contains("Сбербанк"), "{}", out.text);
        let muxes = muxes(&t);
        assert!(
            muxes[0].contains("seg-0.mkv") && muxes[0].contains("-t 5.250"),
            "{}",
            muxes[0]
        );
    }

    #[tokio::test]
    async fn a_recorded_window_whose_title_turns_blind_is_paused() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        ready(&t, &[5.0, 2.0]);
        let report = window(5, "Отчёт — Google Chrome", "chrome.exe");
        // Another window of the same program, beside the recorded one.
        let second = window(6, "Новости — Google Chrome", "chrome.exe");
        *t.f.windows.lock() = vec![report.clone(), second.clone()];
        t.f.window_bounds.lock().insert(5, rect(0, 0, 800, 600));
        t.f.window_bounds.lock().insert(6, rect(900, 0, 800, 600));
        *t.f.foreground.lock() = Some(report);
        let id = start(&rec, json!({ "window": "Отчёт", "sound": false })).await;
        until("the first piece", || captures(&t).len() == 1).await;

        // The page in the recorded window goes to a login form while the
        // other window of the browser is in front.
        *t.f.foreground.lock() = Some(second);
        t.f.windows.lock()[0].title = "Вход в личный кабинет — Google Chrome".into();
        until("the pause", || {
            rec.av.jobs.get(id).unwrap().note == NOTE_BLIND
        })
        .await;
        tokio::time::sleep(POLL * 3).await;
        assert_eq!(captures(&t).len(), 1);

        // Back on an ordinary page, the recording goes on.
        t.f.windows.lock()[0].title = "Отчёт — Google Chrome".into();
        until("the second piece", || captures(&t).len() == 2).await;
        let out = stop(&rec).await.unwrap();
        assert!(
            out.text.contains("Запись 1 раз вставала на паузу"),
            "{}",
            out.text
        );
        assert!(muxes(&t)[0].contains("-t 4.250"), "{}", muxes(&t)[0]);
    }

    #[tokio::test]
    async fn a_blind_window_that_shows_while_ffmpeg_ends_is_cut_too() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        // Told to finish, FFmpeg takes its time and goes on writing.
        *t.f.run_linger.lock() = Duration::from_millis(1500);
        t.answer(DEVICES);
        t.answer(X264_LIST);
        t.answer(&format!(
            "out_time_us=9000000\nprogress=continue\n{AFTER_QUIT}out_time_us=10500000\nprogress=end"
        ));
        let id = start(&rec, json!({ "sound": false })).await;
        until("the capture", || captures(&t).len() == 1).await;
        let jobs = rec.av.jobs.clone();
        jobs.stop(id).unwrap();
        // The user stopped the recording and opens the password manager at once.
        *t.f.foreground.lock() = Some(window(9, "KeePass — пароли", "keepass.exe"));
        let job = jobs.wait(id, WAIT).await.unwrap();
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        let muxes = muxes(&t);
        assert!(muxes[0].contains("-t 8.250"), "{}", muxes[0]);
        assert!(job.text.contains("вставала на паузу"), "{}", job.text);
        assert!(!job.text.contains("KeePass"), "{}", job.text);
    }

    #[tokio::test]
    async fn what_a_blind_window_left_in_the_pieces_never_stays_on_the_disk() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        ready(&t, &[5.0, 3.0]);
        let id = start(&rec, json!({})).await;
        until("the first piece", || captures(&t).len() == 1).await;
        let work = t.f.sound_takes.lock()[0].1.parent().unwrap().to_path_buf();
        *t.f.foreground.lock() = Some(window(9, "KeePass — пароли", "keepass.exe"));
        until("the pause", || {
            rec.av.jobs.get(id).unwrap().note == NOTE_BLIND
        })
        .await;
        *t.f.foreground.lock() = None;
        until("the second piece", || captures(&t).len() == 2).await;
        assert!(work.join("seg-0.mkv").is_file() && work.join("seg-1.mkv").is_file());
        assert!(work.join("seg-0-sys.wav").is_file() && blind_mark(&work, 0).is_file());

        // The file cannot be put together: the first run of it fails.
        t.f.run_outputs.lock().push_back(RunOutput {
            code: Some(1),
            stderr: "No space left on device\n".into(),
            ..Default::default()
        });
        let r = stop(&rec).await;
        assert_eq!(
            r,
            Err(ToolError::Failed(format!(
                "не удалось собрать запись: FFmpeg завершился с ошибкой: No space left on device. Куски остались в {}; куски, которые оборвало окно из слепой зоны, удалены",
                work.display()
            )))
        );
        assert_eq!(rec.av.jobs.get(id).unwrap().state, JobState::Failed);
        // The piece the window ended is gone with its sound and its mark;
        // the other one is the user's and stays.
        assert!(!work.join("seg-0.mkv").exists());
        assert!(!work.join("seg-0-sys.wav").exists());
        assert!(!blind_mark(&work, 0).exists());
        assert!(work.join("seg-1.mkv").is_file());
    }

    #[test]
    fn pieces_left_by_a_recording_that_was_cut_short_are_looked_over() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path();
        // A recording the app was closed in by force: one piece was ended
        // by a blind-zone window, one was not.
        let left = folder.join("Запись экрана 2026-11-14 10-00-00.mika-part");
        std::fs::create_dir(&left).unwrap();
        for name in [
            "seg-0.mkv",
            "seg-0-sys.wav",
            "seg-0-mic.wav",
            "seg-1.mkv",
            "seg-1-sys.wav",
        ] {
            std::fs::write(left.join(name), b"x").unwrap();
        }
        std::fs::write(blind_mark(&left, 0), b"").unwrap();
        // One that held nothing but such a piece.
        let only = folder.join("Запись экрана 2026-11-14 11-00-00.MIKA-PART");
        std::fs::create_dir(&only).unwrap();
        std::fs::write(only.join("seg-3.mkv"), b"x").unwrap();
        std::fs::write(blind_mark(&only, 3), b"").unwrap();
        // The user's own things are not touched.
        let other = folder.join("Отпуск");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("seg-0.mkv"), b"x").unwrap();
        std::fs::write(other.join("seg-0.blind"), b"").unwrap();
        std::fs::write(folder.join("Запись экрана.mp4"), b"x").unwrap();

        sweep_blind(folder);
        let names = |d: &Path| -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(d)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        assert_eq!(names(&left), ["seg-1-sys.wav", "seg-1.mkv"]);
        assert!(!only.exists(), "an emptied folder goes away");
        assert_eq!(names(&other), ["seg-0.blind", "seg-0.mkv"]);
        assert!(folder.join("Запись экрана.mp4").is_file());
        // A folder that is not there is no trouble.
        sweep_blind(&folder.join("нет"));
    }

    #[tokio::test]
    async fn a_file_that_is_not_ready_in_time_is_told_as_not_saved() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        ready(&t, &[5.0]);
        // The work gets two seconds after the stop, and putting the file
        // together takes longer than that.
        TEST_GRACE.with(|g| g.set(Some(Duration::from_secs(2))));
        let id = start(&rec, json!({ "sound": false })).await;
        TEST_GRACE.with(|g| g.set(None));
        until("the capture", || captures(&t).len() == 1).await;
        *t.f.run_delay.lock() = Duration::from_secs(600);
        let r = stop(&rec).await;
        let work = std::fs::read_dir(t.dir.path().join("home").join("Videos").join("Mika"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().ends_with(".mika-part"))
            .expect("the folder of the pieces stays");
        // No file was made, and the call does not say there is one.
        let said = format!("{NOT_SAVED} Куски остались в {}.", work.display());
        assert_eq!(r, Err(ToolError::Failed(said.clone())));
        let job = rec.av.jobs.get(id).unwrap();
        assert_eq!(job.state, JobState::Stopped);
        assert_eq!(job.text, said);
        assert!(work.join("seg-0.mkv").is_file(), "the piece is the user's");
        let folder = work.parent().unwrap();
        super::super::wait_cleanup(Duration::from_secs(5));
        let files: Vec<_> = std::fs::read_dir(folder)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_file())
            .collect();
        assert!(files.is_empty(), "nothing half-written is left: {files:?}");
    }

    #[tokio::test]
    async fn a_stopped_recording_without_a_file_is_an_error_of_the_stop() {
        let t = av();
        let rec = tool(&t);
        // A recording whose work never ends and tells nothing.
        let work: JobWork = Box::new(|_ctx: JobCtx| Box::pin(std::future::pending()));
        let spec = JobSpec::live(KIND, "Записать экран", Duration::from_millis(50));
        rec.av.jobs.submit(spec, work).unwrap();
        assert_eq!(stop(&rec).await, Err(ToolError::Failed(NOT_SAVED.into())));
    }

    #[test]
    fn a_long_recording_is_given_long_enough_to_be_saved() {
        assert_eq!(grace_for(1800.0), stop_grace(1800.0));
        assert!(grace_for(7200.0) > Duration::from_secs(3600));
    }

    /// A drive with hardly any space left.
    struct FullDisk;

    impl SystemProbe for FullDisk {
        fn facts(&self) -> SystemFacts {
            SystemFacts {
                disks: vec![("C:\\".into(), 1.5, 476.0)],
                ..Default::default()
            }
        }
        fn processes(&self) -> Vec<crate::sys::ProcessInfo> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn a_recording_does_not_start_without_space_or_without_capture() {
        let t = av();
        let mut rec = tool(&t);
        // A build that cannot capture the screen.
        t.answer("Devices:\n ---\n D  lavfi  Libavfilter virtual input device\n");
        let r = rec
            .run(json!({}), Approval::for_tests(), CancellationToken::new())
            .await;
        assert_eq!(r, Err(ToolError::Failed(NO_GDIGRAB.into())));
        assert!(rec.av.jobs.list().is_empty());
        let folder = t.dir.path().join("home").join("Videos").join("Mika");
        assert!(!folder.exists(), "nothing was made");

        // Less than 2 ГБ on the drive of the recording.
        let home = t.dir.path().join("home").display().to_string();
        if home.to_lowercase().starts_with("c:\\") {
            rec.probe = Arc::new(FullDisk);
            let r = rec
                .run(json!({}), Approval::for_tests(), CancellationToken::new())
                .await;
            assert_eq!(
                r,
                Err(ToolError::Failed(
                    "на диске C:\\ меньше 2 ГБ свободного места: запись не начинаю".into()
                ))
            );
        }
        assert!(captures(&t).is_empty());
    }

    #[tokio::test]
    async fn a_capture_that_breaks_at_once_leaves_nothing() {
        let t = av();
        let rec = tool(&t);
        t.answer(DEVICES);
        t.answer(X264_LIST);
        t.f.run_outputs.lock().push_back(RunOutput {
            code: Some(1),
            stderr: "[gdigrab] Failed to capture image\n".into(),
            ..Default::default()
        });
        let id = start(&rec, json!({ "sound": false })).await;
        let job = rec.av.jobs.wait(id, WAIT).await.unwrap();
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(
            job.text,
            "ничего не записалось: [gdigrab] Failed to capture image"
        );
        assert_eq!(captures(&t).len(), 1, "a broken piece is not started again");
        super::super::wait_cleanup(Duration::from_secs(5));
        let folder = t.dir.path().join("home").join("Videos").join("Mika");
        let left: Vec<_> = std::fs::read_dir(&folder).unwrap().flatten().collect();
        assert!(left.is_empty(), "{left:?}");
    }

    #[tokio::test]
    async fn pieces_that_cannot_be_put_together_stay_for_the_user() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        ready(&t, &[5.0]);
        let id = start(&rec, json!({ "sound": false })).await;
        until("the capture", || captures(&t).len() == 1).await;
        // The run that puts the file together fails.
        t.f.run_outputs.lock().push_back(RunOutput {
            code: Some(1),
            stderr: "No space left on device\n".into(),
            ..Default::default()
        });
        let r = stop(&rec).await;
        let work = t
            .dir
            .path()
            .join("home")
            .join("Videos")
            .join("Mika")
            .read_dir()
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.to_string_lossy().ends_with(".mika-part"))
            .expect("the folder of the pieces stays");
        assert_eq!(
            r,
            Err(ToolError::Failed(format!(
                "не удалось собрать запись: FFmpeg завершился с ошибкой: No space left on device. Куски остались в {}",
                work.display()
            )))
        );
        assert!(work.join("seg-0.mkv").is_file());
        let job = rec.av.jobs.get(id).unwrap();
        assert_eq!(job.state, JobState::Failed);
        assert!(!job.text.contains(NOT_SAVED), "said once: {}", job.text);
    }

    #[tokio::test]
    async fn the_exit_of_the_app_saves_a_running_recording() {
        let t = av();
        let rec = tool(&t);
        *t.f.run_hold.lock() = true;
        ready(&t, &[6.0]);
        let id = start(&rec, json!({ "sound": false })).await;
        until("the capture", || captures(&t).len() == 1).await;
        rec.av.jobs.shutdown(WAIT).await;
        let job = rec.av.jobs.get(id).unwrap();
        assert_eq!(job.state, JobState::Done, "{}", job.text);
        assert!(job.text.starts_with("Запись сохранена: "), "{}", job.text);
    }

    /// Five seconds of the real screen, the result probed with ffprobe. Without
    /// sound: this crate has no sound devices of its own (`mika-audio` has the
    /// loopback, see its `live_loopback_take`).
    /// `cargo test -p mika-tools live_screen_record -- --ignored --nocapture`;
    /// `MIKA_FFMPEG` names FFmpeg when it is neither in PATH nor downloaded.
    #[tokio::test]
    #[ignore = "records the real screen: needs FFmpeg and a display"]
    async fn live_screen_record() {
        let setting = std::env::var("MIKA_FFMPEG").unwrap_or_default();
        let Some(paths) = super::super::locate(&setting) else {
            println!("FFmpeg is not found: nothing to check");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut env, _) = crate::testing::env();
        env.runner = Arc::new(crate::win::ProcessRunner);
        env.user_profile = dir.path().display().to_string();
        env.shared.screen_enabled.store(true, Ordering::Relaxed);
        *env.shared.ffmpeg_path.write() = paths.ffmpeg.display().to_string();
        let real = crate::ToolEnv::windows(env.db.clone(), env.shared.clone());
        let av = AvEnv::new(&env);
        let rec = ScreenRecordTool {
            av: av.clone(),
            ui: UiEnv {
                windows: real.windows.clone(),
                ui: real.ui.clone(),
                input: real.input.clone(),
                bus: env.shared.bus.clone(),
                own_process: crate::own_process(),
                seen: Default::default(),
                clipboard: None,
                sent: Default::default(),
                clips: Default::default(),
            },
            screen_on: env.shared.screen_enabled.clone(),
            probe: real.probe.clone(),
        };
        let args = json!({ "minutes": 1, "sound": false });
        println!("{:#?}", rec.assess(&args).unwrap());
        let out = rec
            .run(args, Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        println!("{}", out.text);
        tokio::time::sleep(Duration::from_secs(5)).await;
        let out = ScreenRecordStopTool(av.clone())
            .run(json!({}), Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        println!("{}", out.text);
        let file = PathBuf::from(out.data.unwrap()["files"][0]["path"].as_str().unwrap());
        let info = av
            .probe(&paths, &file, &CancellationToken::new())
            .await
            .unwrap();
        println!("{info:?}");
        assert!(info.video.is_some());
        assert!((3.0..9.0).contains(&info.duration()), "{}", info.duration());
    }
}
