//! Fakes of the system interfaces for tool tests.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::bgjobs::JobBoard;
use mika_core::events::EventBus;
use mika_core::gate::Approval;
use mika_core::llm::{TextAnswer, TextFailure};
use mika_core::memory::Memory;
use mika_core::sound_capture::{SoundCapture, SoundSource, SoundStats, SoundTake};
use mika_core::storage::Db;
use mika_core::tools::{Tool, ToolError, ToolOutput};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::observe::{AudioDevice, Proc, Sensors, SessionState};
use crate::sys::*;

#[derive(Default)]
pub struct Fakes {
    pub opened: Mutex<Vec<(String, Option<String>)>>,
    pub volume: Mutex<(u8, bool)>,
    pub app_volumes: Mutex<Vec<AppVolume>>,
    pub power: Mutex<Vec<PowerAction>>,
    pub windows: Mutex<Vec<WindowInfo>>,
    pub focused: Mutex<Vec<u64>>,
    pub closed: Mutex<Vec<u64>>,
    pub clipboard: Mutex<String>,
    /// The clipboard text carries the marks of a password manager.
    pub clip_private: Mutex<bool>,
    /// Gets every text the clipboard takes, as the app's watch does, unless
    /// the text is marked.
    pub clip_watch: Mutex<Option<crate::cliphist::ClipHistory>>,
    /// A key chord that fails.
    pub chord_error: Mutex<Option<Vec<u16>>>,
    /// A real picture for every capture: PNG, width, height.
    pub picture: Mutex<Option<(Vec<u8>, u32, u32)>>,
    /// Windows moved to a monitor.
    pub moved: Mutex<Vec<(u64, usize)>>,
    /// Files put into the clipboard.
    pub clip_files: Mutex<Vec<PathBuf>>,
    /// Vision answers in turn; when used up, a description of the question.
    pub vision_seq: Mutex<Vec<String>>,
    pub start_apps: Mutex<Vec<(String, String)>>,
    pub ocr_text: Mutex<String>,
    /// OCR answers in turn; when used up, `ocr_text`.
    pub ocr_seq: Mutex<Vec<String>>,
    /// What `Screen::ocr_words` finds.
    pub ocr_words: Mutex<Vec<OcrWord>>,
    pub trashed: Mutex<Vec<PathBuf>>,
    pub hosts: Mutex<Vec<String>>,
    pub rdp_files: Mutex<Vec<PathBuf>>,
    pub runs: Mutex<Vec<(String, Vec<String>, Duration)>>,
    pub run_output: Mutex<RunOutput>,
    /// Answers in turn, before `run_output`.
    pub run_outputs: Mutex<VecDeque<RunOutput>>,
    /// The folder of every streamed run.
    pub run_cwds: Mutex<Vec<Option<PathBuf>>>,
    /// What a program other than ffprobe writes to its output file.
    pub run_write: Mutex<Option<Vec<u8>>>,
    /// What speech recognition hears in every piece.
    pub stt_text: Mutex<String>,
    /// Phrases with times for every piece; `None`: the provider cannot tell times.
    pub stt_segments: Mutex<Option<Vec<SpeechSegment>>>,
    /// So many recognitions fail first.
    pub stt_errors: Mutex<usize>,
    /// Recognition of a WAV of this many bytes always fails.
    pub stt_fail_len: Mutex<Option<usize>>,
    /// Length of every WAV given to speech recognition.
    pub stt_wavs: Mutex<Vec<usize>>,
    /// Number of frames of every `ask_frames`.
    pub vision_frames: Mutex<Vec<usize>>,
    /// URL prefix -> body.
    pub http: Mutex<HashMap<String, String>>,
    pub http_calls: Mutex<Vec<String>>,
    pub foreground: Mutex<Option<WindowInfo>>,
    pub placed: Mutex<Vec<(u64, Placement)>>,
    /// What `Ui::tree` returns for any window.
    pub tree: Mutex<Vec<UiNode>>,
    /// What `Ui::invoke` returns; `None` sends the tool to the mouse.
    pub invoke_result: Mutex<Option<&'static str>>,
    pub set_text_ok: Mutex<bool>,
    pub ui_calls: Mutex<Vec<String>>,
    pub chords: Mutex<Vec<Vec<u16>>>,
    pub typed: Mutex<Vec<String>>,
    pub clicks: Mutex<Vec<(i32, i32, MouseButton, bool)>>,
    pub wheels: Mutex<Vec<(i32, i32, i32)>>,
    pub captures: Mutex<Vec<Option<u64>>>,
    pub media: Mutex<Vec<MediaAction>>,
    pub track: Mutex<Option<Track>>,
    /// (Wi-Fi, Bluetooth); `None` means the radio is absent.
    pub radios: Mutex<(Option<bool>, Option<bool>)>,
    /// What the fake browser shows; every action returns it.
    pub page: Mutex<Option<Page>>,
    pub browser_log: Mutex<Vec<String>>,
    /// `Input::modifiers_down` says "held" this many more times.
    pub modifiers_down: Mutex<u32>,
    /// Tabs of a window by its id; a window that is not here shows none.
    pub tabs: Mutex<HashMap<u64, Vec<TabInfo>>>,
    /// The window of every `Ui::tabs` call.
    pub tab_reads: Mutex<Vec<u64>>,
    /// Every `Ui::select_tab` call: the window and the index.
    pub tab_selects: Mutex<Vec<(u64, usize)>>,
    /// The program takes the choice of a tab and opens nothing.
    pub tab_stuck: Mutex<bool>,
    /// Why `Ui::select_tab` fails.
    pub tab_select_error: Mutex<Option<String>>,
    /// Why `Ui::tabs` fails, and how long it takes to say so.
    pub tab_read_error: Mutex<Option<(String, Duration)>>,
    /// Windows whose tab list was cut: `Ui::tabs_cut` says so.
    pub tabs_cut: Mutex<Vec<u64>>,
    /// The title a window gets when this tab of it is chosen: (window, tab
    /// index, new title). A browser window is titled by its open tab.
    pub tab_titles: Mutex<Vec<(u64, usize, String)>>,
    /// Why `Windows::focus` fails: the window does not come forward.
    pub focus_error: Mutex<Option<String>>,
    /// What `Ui::focused_password` returns.
    pub focused_password: Mutex<bool>,
    /// `Ui::focused_password` cannot tell: it returns `None`.
    pub password_unknown: Mutex<bool>,
    /// What the fake sensors of the event watcher see.
    pub audio_devices: Mutex<Vec<AudioDevice>>,
    pub procs: Mutex<Vec<Proc>>,
    pub session_state: Mutex<SessionState>,
    pub idle_s: Mutex<u64>,
    /// Text of a window by its id; a window that is not here cannot be read.
    pub window_texts: Mutex<HashMap<u64, String>>,
    /// The method name of every sensor call.
    pub sensor_calls: Mutex<Vec<&'static str>>,
    /// A sensor method that fails.
    pub sensor_error: Mutex<Option<&'static str>>,
    /// Where each window stands; a window that is not here is minimized.
    pub window_bounds: Mutex<HashMap<u64, Rect>>,
    /// Whole areas of the monitors; empty means one monitor 0,0 1920x1080.
    pub monitor_bounds: Mutex<Vec<Rect>>,
    /// While true, a controlled run goes on until it is told to quit or is
    /// cancelled; false lets it end by itself.
    pub run_hold: Mutex<bool>,
    /// How long a controlled run goes on after it was told to finish.
    pub run_linger: Mutex<Duration>,
    /// How long every streamed run takes; a stop ends it at once.
    pub run_delay: Mutex<Duration>,
    /// Every sound take that started: its source and its file.
    pub sound_takes: Mutex<Vec<(SoundSource, PathBuf)>>,
    /// Why no sound take starts.
    pub sound_error: Mutex<Option<String>>,
    /// The "Microphone" switch.
    pub mic_allowed: Arc<AtomicBool>,
    /// Answers of the text model: the first whose needle is in the system or user text wins.
    pub llm: Mutex<Vec<(String, Result<String, String>)>>,
    /// Characters of reasoning the text model streams beside every answer.
    pub llm_reasoning: Mutex<usize>,
    /// Every request to the text model: (model, system, user, max_chars).
    pub llm_calls: Mutex<Vec<(String, String, String, usize)>>,
}

impl Fake {
    /// Notes a sensor call; an error when this sensor is set to fail.
    fn sensor(&self, method: &'static str) -> Result<(), String> {
        self.0.sensor_calls.lock().push(method);
        if *self.0.sensor_error.lock() == Some(method) {
            return Err("датчик не ответил".into());
        }
        Ok(())
    }
}

impl Sensors for Fake {
    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        self.sensor("audio_devices")?;
        Ok(self.0.audio_devices.lock().clone())
    }
    fn processes(&self) -> Result<Vec<Proc>, String> {
        self.sensor("processes")?;
        Ok(self.0.procs.lock().clone())
    }
    fn session(&self) -> Result<SessionState, String> {
        self.sensor("session")?;
        Ok(*self.0.session_state.lock())
    }
    fn idle_seconds(&self) -> u64 {
        let _ = self.sensor("idle_seconds");
        *self.0.idle_s.lock()
    }
    fn window_text(&self, window: u64) -> Result<String, String> {
        self.sensor("window_text")?;
        self.0
            .window_texts
            .lock()
            .get(&window)
            .cloned()
            .ok_or_else(|| "окно не читается".to_string())
    }
}

#[async_trait]
impl Browser for Fake {
    fn last(&self) -> Option<Page> {
        self.0.page.lock().clone()
    }
    async fn open(&self, url: &str) -> Result<Page, String> {
        self.0.browser_log.lock().push(format!("open {url}"));
        self.0.page.lock().clone().ok_or("no page".into())
    }
    async fn read(&self, wait_for: Option<&str>, _wait: Duration) -> Result<Page, String> {
        if let Some(w) = wait_for {
            self.0.browser_log.lock().push(format!("wait {w}"));
        }
        self.0.page.lock().clone().ok_or("no page".into())
    }
    async fn back(&self) -> Result<Page, String> {
        self.0.browser_log.lock().push("back".into());
        self.0.page.lock().clone().ok_or("no page".into())
    }
    async fn click(&self, n: u32) -> Result<Page, String> {
        self.0.browser_log.lock().push(format!("click {n}"));
        self.0.page.lock().clone().ok_or("no page".into())
    }
    async fn type_into(&self, n: u32, text: &str, submit: bool) -> Result<Page, String> {
        self.0
            .browser_log
            .lock()
            .push(format!("type {n} {text} {submit}"));
        self.0.page.lock().clone().ok_or("no page".into())
    }
    async fn scroll(&self, down: bool) -> Result<Page, String> {
        self.0.browser_log.lock().push(format!("scroll {down}"));
        self.0.page.lock().clone().ok_or("no page".into())
    }
}

/// Every fake element sits here on the screen.
pub const FAKE_RECT: Rect = Rect {
    x: 100,
    y: 200,
    w: 80,
    h: 30,
};

pub struct Fake(pub Arc<Fakes>);

impl Launcher for Fake {
    fn open(&self, target: &str, args: Option<&str>) -> Result<(), String> {
        self.0
            .opened
            .lock()
            .push((target.into(), args.map(str::to_string)));
        Ok(())
    }
    fn start_apps(&self) -> Vec<(String, String)> {
        self.0.start_apps.lock().clone()
    }
}

impl Volume for Fake {
    fn get(&self) -> Result<(u8, bool), String> {
        Ok(*self.0.volume.lock())
    }
    fn set(&self, level: u8) -> Result<(), String> {
        self.0.volume.lock().0 = level;
        Ok(())
    }
    fn set_mute(&self, mute: bool) -> Result<(), String> {
        self.0.volume.lock().1 = mute;
        Ok(())
    }
    fn apps(&self) -> Result<Vec<AppVolume>, String> {
        Ok(self.0.app_volumes.lock().clone())
    }
    fn set_app(&self, pid: u32, level: Option<u8>, mute: Option<bool>) -> Result<(), String> {
        let mut apps = self.0.app_volumes.lock();
        let a = apps
            .iter_mut()
            .find(|a| a.pid == pid)
            .ok_or("no such app")?;
        if let Some(l) = level {
            a.level = l;
        }
        if let Some(m) = mute {
            a.muted = m;
        }
        Ok(())
    }
}

impl Power for Fake {
    fn act(&self, action: PowerAction) -> Result<(), String> {
        self.0.power.lock().push(action);
        Ok(())
    }
}

impl Windows for Fake {
    fn list(&self) -> Vec<WindowInfo> {
        self.0.windows.lock().clone()
    }
    fn focus(&self, id: u64) -> Result<(), String> {
        if let Some(why) = self.0.focus_error.lock().clone() {
            return Err(why);
        }
        self.0.focused.lock().push(id);
        Ok(())
    }
    fn close(&self, id: u64) -> Result<(), String> {
        self.0.closed.lock().push(id);
        Ok(())
    }
    fn foreground(&self) -> Option<WindowInfo> {
        self.0.foreground.lock().clone()
    }
    fn place(&self, id: u64, placement: Placement) -> Result<(), String> {
        self.0.placed.lock().push((id, placement));
        Ok(())
    }
    fn monitors(&self) -> usize {
        2
    }
    fn to_monitor(&self, id: u64, n: usize) -> Result<(), String> {
        if n == 0 || n > 2 {
            return Err(format!("монитора {n} нет: их 2"));
        }
        self.0.moved.lock().push((id, n));
        Ok(())
    }
    fn bounds(&self, id: u64) -> Option<Rect> {
        self.0.window_bounds.lock().get(&id).copied()
    }
    fn monitor_bounds(&self) -> Vec<Rect> {
        let set = self.0.monitor_bounds.lock().clone();
        if set.is_empty() {
            vec![Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            }]
        } else {
            set
        }
    }
}

impl Ui for Fake {
    fn tree(&self, _window: u64) -> Result<Vec<UiNode>, String> {
        Ok(self.0.tree.lock().clone())
    }
    fn rect(&self, _window: u64, _n: u32) -> Result<Rect, String> {
        Ok(FAKE_RECT)
    }
    fn invoke(&self, _window: u64, _n: u32) -> Result<Option<&'static str>, String> {
        Ok(*self.0.invoke_result.lock())
    }
    fn set_text(&self, window: u64, n: u32, text: &str) -> Result<bool, String> {
        let ok = *self.0.set_text_ok.lock();
        if ok {
            self.0
                .ui_calls
                .lock()
                .push(format!("set_text {window} {n} {text}"));
        }
        Ok(ok)
    }
    fn focus(&self, window: u64, n: u32) -> Result<(), String> {
        self.0.ui_calls.lock().push(format!("focus {window} {n}"));
        Ok(())
    }
    fn focused_password(&self) -> Option<bool> {
        if *self.0.password_unknown.lock() {
            return None;
        }
        Some(*self.0.focused_password.lock())
    }
    fn tabs(&self, window: u64) -> Result<Vec<TabInfo>, String> {
        self.0.tab_reads.lock().push(window);
        if let Some((why, after)) = self.0.tab_read_error.lock().clone() {
            std::thread::sleep(after);
            return Err(why);
        }
        Ok(self.0.tabs.lock().get(&window).cloned().unwrap_or_default())
    }
    fn tabs_cut(&self, window: u64) -> bool {
        self.0.tabs_cut.lock().contains(&window)
    }
    fn select_tab(&self, window: u64, index: usize) -> Result<(), String> {
        // Like the real one: only a tab of the window that was read last.
        let read_last = self.0.tab_reads.lock().last() == Some(&window);
        let mut all = self.0.tabs.lock();
        let tabs = all
            .get_mut(&window)
            .filter(|tabs| read_last && index < tabs.len())
            .ok_or("список вкладок устарел")?;
        if let Some(why) = self.0.tab_select_error.lock().clone() {
            return Err(why);
        }
        self.0.tab_selects.lock().push((window, index));
        if !*self.0.tab_stuck.lock() {
            for (i, tab) in tabs.iter_mut().enumerate() {
                if tab.selected.is_some() {
                    tab.selected = Some(i == index);
                }
            }
            let titles = self.0.tab_titles.lock();
            if let Some((_, _, title)) = titles.iter().find(|t| t.0 == window && t.1 == index) {
                if let Some(w) = self.0.windows.lock().iter_mut().find(|w| w.id == window) {
                    w.title = title.clone();
                }
            }
        }
        Ok(())
    }
}

impl Input for Fake {
    fn chord(&self, keys: &[u16]) -> Result<(), String> {
        if self.0.chord_error.lock().as_deref() == Some(keys) {
            return Err("клавиши не нажались".into());
        }
        self.0.chords.lock().push(keys.to_vec());
        Ok(())
    }
    fn text(&self, text: &str) -> Result<(), String> {
        self.0.typed.lock().push(text.into());
        Ok(())
    }
    fn click(&self, x: i32, y: i32, button: MouseButton, double: bool) -> Result<(), String> {
        self.0.clicks.lock().push((x, y, button, double));
        Ok(())
    }
    fn wheel(&self, x: i32, y: i32, lines: i32) -> Result<(), String> {
        self.0.wheels.lock().push((x, y, lines));
        Ok(())
    }
    fn modifiers_down(&self) -> bool {
        let mut left = self.0.modifiers_down.lock();
        if *left == 0 {
            return false;
        }
        *left -= 1;
        true
    }
}

impl Screen for Fake {
    fn capture(&self, window: Option<u64>, max_side: u32) -> Result<Shot, String> {
        self.0.captures.lock().push(window);
        if let Some((png, width, height)) = self.0.picture.lock().clone() {
            return Ok(Shot {
                png,
                width,
                height,
                factor: 1.0,
                origin: (10, 20),
            });
        }
        // A 3200 px wide screen: shrinks to the limit when one is given.
        let factor = if max_side == 0 || max_side >= 3200 {
            1.0
        } else {
            3200.0 / max_side as f32
        };
        Ok(Shot {
            png: b"\x89PNG".to_vec(),
            width: (3200.0 / factor) as u32,
            height: (1800.0 / factor) as u32,
            factor,
            origin: (10, 20),
        })
    }
    fn ocr_words(&self, _png: &[u8]) -> Result<Vec<OcrWord>, String> {
        Ok(self.0.ocr_words.lock().clone())
    }
    fn ocr(&self, _png: &[u8]) -> Result<String, String> {
        let mut seq = self.0.ocr_seq.lock();
        if !seq.is_empty() {
            return Ok(seq.remove(0));
        }
        let set = self.0.ocr_text.lock().clone();
        Ok(if set.is_empty() {
            "текст с экрана".into()
        } else {
            set
        })
    }
}

#[async_trait]
impl Vision for Fake {
    async fn ask(&self, png: &[u8], question: &str) -> Result<String, String> {
        let mut seq = self.0.vision_seq.lock();
        if !seq.is_empty() {
            return Ok(seq.remove(0));
        }
        Ok(format!("вижу {} байт, вопрос: {question}", png.len()))
    }

    async fn ask_frames(&self, pngs: &[Vec<u8>], question: &str) -> Result<String, String> {
        self.0.vision_frames.lock().push(pngs.len());
        Ok(format!("вижу {} кадров, вопрос: {question}", pngs.len()))
    }
}

#[async_trait]
impl ImageGen for Fake {
    async fn generate(&self, _prompt: &str) -> Result<Vec<u8>, String> {
        Ok(vec![0xFF, 0xD8, 0xFF, 0xE0])
    }
}

impl Media for Fake {
    fn control(&self, action: MediaAction) -> Result<(), String> {
        self.0.media.lock().push(action);
        Ok(())
    }
    fn now_playing(&self) -> Result<Option<Track>, String> {
        Ok(self.0.track.lock().clone())
    }
}

impl Radios for Fake {
    fn get(&self, kind: RadioKind) -> Result<Option<bool>, String> {
        let r = self.0.radios.lock();
        Ok(match kind {
            RadioKind::Wifi => r.0,
            RadioKind::Bluetooth => r.1,
        })
    }
    fn set(&self, kind: RadioKind, on: bool) -> Result<(), String> {
        let mut r = self.0.radios.lock();
        let slot = match kind {
            RadioKind::Wifi => &mut r.0,
            RadioKind::Bluetooth => &mut r.1,
        };
        if slot.is_none() {
            return Err("нет такого радиомодуля".into());
        }
        *slot = Some(on);
        Ok(())
    }
}

/// Text in the fake's saved clipboard.
const SAVED_TEXT: u32 = 13;
/// A password manager's mark in the fake's saved clipboard.
const SAVED_MARK: u32 = 0xC100;

impl Fake {
    /// The clipboard takes `text`: like every real write, it drops the marks.
    fn clip_write(&self, text: &str, private: bool) {
        *self.0.clipboard.lock() = text.into();
        *self.0.clip_private.lock() = private;
        if let Some(history) = self.0.clip_watch.lock().as_ref() {
            if !private && !text.is_empty() {
                history.push(text, "fake.exe", 0);
            }
        }
    }
}

impl Clipboard for Fake {
    fn get(&self) -> Result<String, String> {
        Ok(self.0.clipboard.lock().clone())
    }
    fn set(&self, text: &str) -> Result<(), String> {
        self.clip_write(text, false);
        Ok(())
    }
    fn set_files(&self, paths: &[PathBuf]) -> Result<(), String> {
        *self.0.clip_files.lock() = paths.to_vec();
        self.clip_write("", false);
        Ok(())
    }
    fn save(&self) -> Result<SavedClip, String> {
        let mut formats = vec![(SAVED_TEXT, self.0.clipboard.lock().as_bytes().to_vec())];
        if *self.0.clip_private.lock() {
            formats.push((SAVED_MARK, Vec::new()));
        }
        Ok(SavedClip { formats })
    }
    fn restore(&self, saved: &SavedClip) -> Result<(), String> {
        let text = saved
            .formats
            .iter()
            .find(|(f, _)| *f == SAVED_TEXT)
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        let private = saved.formats.iter().any(|(f, _)| *f == SAVED_MARK);
        self.clip_write(&text, private);
        Ok(())
    }
}

impl Trash for Fake {
    fn trash(&self, path: &Path) -> Result<(), String> {
        self.0.trashed.lock().push(path.to_path_buf());
        Ok(())
    }
}

impl RdpSource for Fake {
    fn saved_hosts(&self) -> Vec<String> {
        self.0.hosts.lock().clone()
    }
    fn rdp_files(&self) -> Vec<PathBuf> {
        self.0.rdp_files.lock().clone()
    }
}

impl Fake {
    /// Records a run and gives the next answer: the queue first, then
    /// `run_output`. With `run_write` set, a program other than ffprobe
    /// writes those bytes to its last `file:` argument, as FFmpeg would.
    fn answer(&self, program: &str, args: &[String], timeout: Duration) -> RunOutput {
        self.0
            .runs
            .lock()
            .push((program.into(), args.to_vec(), timeout));
        if !program.to_lowercase().contains("ffprobe") {
            if let (Some(bytes), Some(out)) = (
                self.0.run_write.lock().clone(),
                args.last().and_then(|a| a.strip_prefix("file:")),
            ) {
                let _ = std::fs::write(out, bytes);
            }
        }
        let next = self.0.run_outputs.lock().pop_front();
        next.unwrap_or_else(|| self.0.run_output.lock().clone())
    }
}

#[async_trait]
impl Runner for Fake {
    async fn run(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
        _cancel: CancellationToken,
    ) -> Result<RunOutput, String> {
        Ok(self.answer(program, args, timeout))
    }

    async fn run_streaming(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        timeout: Duration,
        cancel: CancellationToken,
        on_stdout: &(dyn for<'l> Fn(&'l str) + Send + Sync),
    ) -> Result<RunOutput, String> {
        self.0.run_cwds.lock().push(cwd.map(Path::to_path_buf));
        let out = self.answer(program, args, timeout);
        let delay = *self.0.run_delay.lock();
        if !delay.is_zero() {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => return Err("остановлено".into()),
            }
        }
        for line in out.stdout.lines() {
            on_stdout(line);
        }
        Ok(out)
    }

    async fn run_controlled(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        timeout: Duration,
        cancel: CancellationToken,
        quit: CancellationToken,
        _quit_wait: Duration,
        on_stdout: &(dyn for<'l> Fn(&'l str) + Send + Sync),
    ) -> Result<RunOutput, String> {
        self.0.run_cwds.lock().push(cwd.map(Path::to_path_buf));
        let mut out = self.answer(program, args, timeout);
        // Lines after `AFTER_QUIT` are what the program still writes between
        // being told to finish and its end.
        let (before, after) = match out.stdout.split_once(AFTER_QUIT) {
            Some((before, after)) => (before.to_string(), after.to_string()),
            None => (out.stdout.clone(), String::new()),
        };
        for line in before.lines() {
            on_stdout(line);
        }
        // The program goes on until it is told to finish.
        while *self.0.run_hold.lock() && !quit.is_cancelled() {
            if cancel.is_cancelled() {
                return Err("остановлено".into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // Told to finish, it may take its time to end.
        let linger = *self.0.run_linger.lock();
        if !linger.is_zero() && quit.is_cancelled() {
            tokio::time::sleep(linger).await;
        }
        for line in after.lines().filter(|l| !l.is_empty()) {
            on_stdout(line);
        }
        out.stdout = format!("{before}{after}");
        Ok(out)
    }

    fn run_blocking(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<RunOutput, String> {
        Ok(self.answer(program, args, timeout))
    }
}

/// In the answer of a controlled run: the lines after this one are written
/// only after the run was told to finish.
pub const AFTER_QUIT: &str = "--after-quit--\n";

/// A sound take of the fake: an empty WAV file appears when it stops.
struct FakeTake(PathBuf);

impl SoundTake for FakeTake {
    fn stop(self: Box<Self>) -> Result<SoundStats, String> {
        let mut wav = Vec::with_capacity(44);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&48_000u32.to_le_bytes());
        wav.extend_from_slice(&192_000u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&0u32.to_le_bytes());
        std::fs::write(&self.0, wav).map_err(|e| e.to_string())?;
        Ok(SoundStats {
            seconds: 1.0,
            silent: false,
        })
    }
}

impl SoundCapture for Fake {
    fn start(&self, source: SoundSource, wav: &Path) -> Result<Box<dyn SoundTake>, String> {
        if let Some(e) = self.0.sound_error.lock().clone() {
            return Err(e);
        }
        self.0.sound_takes.lock().push((source, wav.to_path_buf()));
        Ok(Box::new(FakeTake(wav.to_path_buf())))
    }
}

#[async_trait]
impl SpeechToText for Fake {
    async fn transcribe(&self, wav: &[u8]) -> Result<String, String> {
        self.0.stt_wavs.lock().push(wav.len());
        if *self.0.stt_fail_len.lock() == Some(wav.len()) {
            return Err("сбой распознавания".into());
        }
        let mut errors = self.0.stt_errors.lock();
        if *errors > 0 {
            *errors -= 1;
            return Err("сбой распознавания".into());
        }
        Ok(self.0.stt_text.lock().clone())
    }

    async fn transcribe_segments(&self, wav: &[u8]) -> Result<Option<Vec<SpeechSegment>>, String> {
        let segments = self.0.stt_segments.lock().clone();
        if segments.is_some() {
            self.0.stt_wavs.lock().push(wav.len());
        }
        Ok(segments)
    }
}

#[async_trait]
impl Http for Fake {
    async fn get(&self, url: &str) -> Result<String, String> {
        self.0.http_calls.lock().push(url.into());
        let map = self.0.http.lock();
        map.iter()
            .find(|(prefix, _)| url.starts_with(prefix.as_str()))
            .map(|(_, body)| body.clone())
            .ok_or_else(|| format!("нет ответа для {url}"))
    }
}

#[async_trait]
impl TextModel for Fake {
    fn model_id(&self, model: &str) -> String {
        let model = model.trim();
        if model.is_empty() {
            "fake-model".into()
        } else {
            model.to_string()
        }
    }

    async fn complete(
        &self,
        model: &str,
        system: &str,
        user: &str,
        max_chars: usize,
    ) -> Result<TextAnswer, TextFailure> {
        self.0
            .llm_calls
            .lock()
            .push((model.into(), system.into(), user.into(), max_chars));
        let reasoning = *self.0.llm_reasoning.lock();
        let answers = self.0.llm.lock();
        let answer = answers
            .iter()
            .find(|(needle, _)| system.contains(needle.as_str()) || user.contains(needle.as_str()))
            .map(|(_, answer)| answer.clone())
            .unwrap_or_else(|| Err("нет ответа модели".into()));
        match answer {
            Ok(text) => {
                let text: String = text.chars().take(max_chars).collect();
                let streamed = text.chars().count() + reasoning;
                Ok(TextAnswer { text, streamed })
            }
            Err(reason) => Err(TextFailure::new(reason)),
        }
    }
}

#[async_trait]
impl crate::research::net::Net for Fake {
    async fn get(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<crate::research::net::Fetched, crate::research::net::NetError> {
        use crate::research::net::{check_url, Fetched, NetError};
        self.0.http_calls.lock().push(url.into());
        check_url(url).map_err(NetError::Refused)?;
        let map = self.0.http.lock();
        let body = map
            .iter()
            .find(|(prefix, _)| url.starts_with(prefix.as_str()))
            .map(|(_, body)| body.clone())
            .ok_or(NetError::Status(404))?;
        let mut cut = max_bytes.min(body.len());
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        Ok(Fetched {
            url: url.into(),
            html: body[..cut].to_string(),
            truncated: cut < body.len(),
        })
    }
}

impl SystemProbe for Fake {
    fn facts(&self) -> SystemFacts {
        SystemFacts {
            cpu_percent: 12.5,
            memory_used_mb: 4096,
            memory_total_mb: 16384,
            uptime_s: 3 * 3600 + 120,
            disks: vec![("C:\\".into(), 100.0, 476.0)],
            os: "Windows 11".into(),
            host: "PC".into(),
        }
    }
    fn processes(&self) -> Vec<ProcessInfo> {
        vec![
            ProcessInfo {
                pid: 1,
                name: "chrome.exe".into(),
                memory_mb: 900,
                cpu_percent: 3.0,
            },
            ProcessInfo {
                pid: 2,
                name: "game.exe".into(),
                memory_mb: 300,
                cpu_percent: 40.0,
            },
        ]
    }
}

pub fn env() -> (ToolEnv, Arc<Fakes>) {
    env_with(Arc::new(Db::open_in_memory().unwrap()), "C:\\Users\\me")
}

pub fn env_with(db: Arc<Db>, user_profile: &str) -> (ToolEnv, Arc<Fakes>) {
    let f = Arc::new(Fakes::default());
    *f.radios.lock() = (Some(true), None);
    let fake = || Arc::new(Fake(f.clone()));
    let bus = EventBus::new(256);
    let mic_allowed = f.mic_allowed.clone();
    let env = ToolEnv {
        launcher: fake(),
        volume: fake(),
        power: fake(),
        windows: fake(),
        clipboard: fake(),
        trash: fake(),
        rdp: fake(),
        runner: fake(),
        http: fake(),
        probe: fake(),
        ui: fake(),
        input: fake(),
        screen: fake(),
        media: fake(),
        radios: fake(),
        browser: fake(),
        sensors: fake(),
        user_profile: user_profile.into(),
        shared: Shared {
            web_enabled: Arc::new(AtomicBool::new(true)),
            screen_enabled: Arc::new(AtomicBool::new(false)),
            news_feed: Arc::new(parking_lot::RwLock::new(
                "https://feed.example/rss".to_string(),
            )),
            home_city: Default::default(),
            memory: Memory::new(db.clone(), None),
            vision: fake(),
            images: fake(),
            bus: bus.clone(),
            tasks: mika_core::task::TaskBoard::new(EventBus::new(64)),
            watches: Default::default(),
            focus: Default::default(),
            clips: Default::default(),
            clipboard_on: Default::default(),
            app_time_on: Default::default(),
            calendar: Default::default(),
            docs: crate::docs::DocIndex::new(db.clone(), None),
            ffmpeg_path: Default::default(),
            stt: fake(),
            av_outputs: Default::default(),
            triggers: mika_core::triggers::TriggerStore::new(db.clone()),
            waits: Default::default(),
            jobs: JobBoard::new(bus),
            rec: crate::ffmpeg::RecordDeps {
                sound: fake(),
                mic_allowed: Arc::new(move || mic_allowed.load(Ordering::Relaxed)),
            },
            text_model: fake(),
            research: Default::default(),
            research_runs: Default::default(),
            settings: Default::default(),
            skills: Default::default(),
        },
        db,
    };
    (env, f)
}

pub async fn run(tool: &dyn Tool, args: Value) -> Result<ToolOutput, ToolError> {
    tool.assess(&args)?;
    tool.run(args, Approval::for_tests(), CancellationToken::new())
        .await
}
