//! Narrow interfaces to the operating system. Tools use only these,
//! so tests replace them with fakes.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::events::EventBus;
use mika_core::memory::Memory;
use mika_core::storage::Db;
use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;

pub trait Launcher: Send + Sync {
    /// Opens a program, file, folder or URL with the shell, like double-click.
    fn open(&self, target: &str, args: Option<&str>) -> Result<(), String>;
    /// Programs of the Start menu, Store apps included: (name, AppID). Opened as
    /// `shell:AppsFolder\<AppID>`.
    fn start_apps(&self) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// Sound of one program in the Windows mixer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppVolume {
    pub pid: u32,
    /// Executable name, lower case: `chrome.exe`.
    pub process: String,
    /// 0..=100, relative to the system volume.
    pub level: u8,
    pub muted: bool,
}

pub trait Volume: Send + Sync {
    /// Level 0..=100 and mute flag.
    fn get(&self) -> Result<(u8, bool), String>;
    fn set(&self, level: u8) -> Result<(), String>;
    fn set_mute(&self, mute: bool) -> Result<(), String>;
    /// Programs that have sound in the mixer now.
    fn apps(&self) -> Result<Vec<AppVolume>, String> {
        Err("громкость программ здесь недоступна".into())
    }
    /// Changes the level or the mute flag of every sound of the process.
    fn set_app(&self, _pid: u32, _level: Option<u8>, _mute: Option<bool>) -> Result<(), String> {
        Err("громкость программ здесь недоступна".into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Lock,
    Sleep,
    Restart,
    Shutdown,
    Logoff,
}

pub trait Power: Send + Sync {
    fn act(&self, action: PowerAction) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: u64,
    pub title: String,
    /// Executable name without path, lower case: `notepad.exe`.
    pub process: String,
}

/// Where `arrange_window` puts a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Left,
    Right,
    Top,
    Bottom,
    Maximize,
    Minimize,
    Restore,
}

/// `Windows::focus` off the async threads: a window that does not come
/// forward at once is waited for, up to a second and a half.
pub(crate) async fn focus(windows: &Arc<dyn Windows>, id: u64) -> Result<(), String> {
    let windows = windows.clone();
    tokio::task::spawn_blocking(move || windows.focus(id))
        .await
        .map_err(|e| e.to_string())?
}

pub trait Windows: Send + Sync {
    fn list(&self) -> Vec<WindowInfo>;
    /// Brings the window to the front and checks that it came. It may wait:
    /// async code calls it through `focus` above.
    fn focus(&self, id: u64) -> Result<(), String>;
    /// Asks the window to close, like the close button.
    fn close(&self, id: u64) -> Result<(), String>;
    /// The window the user works in now.
    fn foreground(&self) -> Option<WindowInfo>;
    fn place(&self, id: u64, placement: Placement) -> Result<(), String>;
    /// How many monitors there are.
    fn monitors(&self) -> usize {
        1
    }
    /// Moves the window to monitor `n` (from 1: the main one, then from left to
    /// right), keeping its place and size there as far as they fit.
    fn to_monitor(&self, _id: u64, _n: usize) -> Result<(), String> {
        Err("перенос между мониторами здесь недоступен".into())
    }
    /// Where the window is, physical pixels; `None` when it is minimized,
    /// hidden or closed.
    fn bounds(&self, _id: u64) -> Option<Rect> {
        None
    }
    /// Whole areas of the monitors (not work areas), the main one first, then
    /// left to right.
    fn monitor_bounds(&self) -> Vec<Rect> {
        Vec::new()
    }
    /// Every window that is on the screen now, with its title and its place:
    /// main windows and the dialogs they own alike, in front or under other
    /// windows. Minimized and hidden ones are left out.
    fn on_screen(&self) -> Vec<(String, Rect)> {
        self.list()
            .into_iter()
            .filter_map(|w| self.bounds(w.id).map(|r| (w.title, r)))
            .collect()
    }
}

/// Screen rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn center(self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// One element of a window as UI Automation sees it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UiNode {
    /// Number for `ui_click` and `ui_type`; valid until the window is read again.
    pub n: u32,
    pub depth: u8,
    /// Control type in English: Button, Edit, MenuItem.
    pub role: String,
    pub name: String,
    /// Text of an edit field or state of a checkbox.
    pub value: String,
    pub enabled: bool,
}

/// One tab of a window's own tab strip: a page of a browser, a file of an
/// editor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TabInfo {
    pub name: String,
    /// Whether it is the open one; `None` when the program does not tell.
    pub selected: Option<bool>,
}

/// UI Automation: the tree of a window and actions on its elements.
/// Element numbers come from the last `tree` call for that window.
pub trait Ui: Send + Sync {
    fn tree(&self, window: u64) -> Result<Vec<UiNode>, String>;
    fn rect(&self, window: u64, n: u32) -> Result<Rect, String>;
    /// Presses, toggles, selects or expands the element. Returns what it did,
    /// or `Ok(None)` when the element has no such action and needs a mouse click.
    fn invoke(&self, window: u64, n: u32) -> Result<Option<&'static str>, String>;
    /// Replaces the text of an edit field. `Ok(false)` when the element takes
    /// no text this way and needs focus plus typing.
    fn set_text(&self, window: u64, n: u32, text: &str) -> Result<bool, String>;
    fn focus(&self, window: u64, n: u32) -> Result<(), String>;
    /// Whether the keyboard focus is in a password field. `None` when it
    /// cannot be told (no answer in time, an error): the caller must then
    /// treat the field as one it may not type into.
    fn focused_password(&self) -> Option<bool> {
        None
    }
    /// Tabs of the window's own interface, in their order; what a web page
    /// draws inside is not looked at. Empty when the program shows none.
    fn tabs(&self, _window: u64) -> Result<Vec<TabInfo>, String> {
        Ok(Vec::new())
    }
    /// Whether the last `tabs` answer for the window left tabs out: there
    /// were more of them than one reading takes.
    fn tabs_cut(&self, _window: u64) -> bool {
        false
    }
    /// Opens the tab with this index in the last `tabs` answer for the window.
    fn select_tab(&self, _window: u64, _index: usize) -> Result<(), String> {
        Err("переключение вкладок здесь недоступно".into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Keyboard and mouse emulation.
pub trait Input: Send + Sync {
    /// Presses virtual keys together: modifiers first, the main key last.
    fn chord(&self, keys: &[u16]) -> Result<(), String>;
    fn text(&self, text: &str) -> Result<(), String>;
    fn click(&self, x: i32, y: i32, button: MouseButton, double: bool) -> Result<(), String>;
    /// Positive `lines` scroll down.
    fn wheel(&self, x: i32, y: i32, lines: i32) -> Result<(), String>;
    /// Whether Ctrl, Alt, Shift or Win is held now.
    fn modifiers_down(&self) -> bool {
        false
    }
}

/// One screenshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Screen pixels per picture pixel: 1.0 unless the picture was shrunk.
    pub factor: f32,
    /// Screen position of the top left corner, physical pixels.
    pub origin: (i32, i32),
}

/// One word found on a picture, with its box in picture pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrWord {
    pub text: String,
    /// Words of one line share it.
    pub line: u32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub trait Screen: Send + Sync {
    /// A window, or the whole primary screen. `max_side` shrinks the picture
    /// so that its longer side fits; 0 keeps the real size.
    fn capture(&self, window: Option<u64>, max_side: u32) -> Result<Shot, String>;
    /// The window as it is seen on the screen now, pixel for pixel where a
    /// click lands. For a window brought to the front before clicking.
    fn capture_visible(&self, window: u64, max_side: u32) -> Result<Shot, String> {
        self.capture(Some(window), max_side)
    }
    /// Text found on a PNG picture.
    fn ocr(&self, png: &[u8]) -> Result<String, String>;
    /// Words found on a PNG picture with their boxes.
    fn ocr_words(&self, _png: &[u8]) -> Result<Vec<OcrWord>, String> {
        Err("распознавание слов с их местом здесь недоступно".into())
    }
}

pub use mika_core::llm::{ImageGen, SpeechSegment, SpeechToText, TextModel, Vision};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAction {
    PlayPause,
    Next,
    Previous,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Track {
    pub title: String,
    pub artist: String,
    /// Program that plays it.
    pub app: String,
    pub playing: bool,
}

pub trait Media: Send + Sync {
    fn control(&self, action: MediaAction) -> Result<(), String>;
    fn now_playing(&self) -> Result<Option<Track>, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioKind {
    Wifi,
    Bluetooth,
}

pub trait Radios: Send + Sync {
    /// `None` when the computer has no such radio.
    fn get(&self, kind: RadioKind) -> Result<Option<bool>, String>;
    fn set(&self, kind: RadioKind, on: bool) -> Result<(), String>;
}

/// What the clipboard held before she used it, to be put back afterwards.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedClip {
    /// Clipboard formats with their bytes, in the order the clipboard listed
    /// them: text, pictures, files and the marks password managers put on
    /// their copies.
    pub formats: Vec<(u32, Vec<u8>)>,
}

pub trait Clipboard: Send + Sync {
    fn get(&self) -> Result<String, String>;
    fn set(&self, text: &str) -> Result<(), String>;
    /// Files, as Explorer copies them: a program pastes them as attachments.
    fn set_files(&self, _paths: &[PathBuf]) -> Result<(), String> {
        Err("копирование файлов в буфер здесь недоступно".into())
    }
    /// What the clipboard holds now, not only text: marks of password
    /// managers too, so that what is put back stays out of clipboard history.
    fn save(&self) -> Result<SavedClip, String>;
    /// Puts back what `save` returned, in place of anything there now.
    fn restore(&self, saved: &SavedClip) -> Result<(), String>;
}

/// One clickable or editable element of a web page, numbered for the model.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
pub struct PageItem {
    pub n: u32,
    /// `ссылка`, `кнопка`, `поле`, `список`, `флажок` or an ARIA role.
    pub kind: String,
    pub label: String,
}

/// What the browser shows now. Values of password fields are never read.
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub text: String,
    pub items: Vec<PageItem>,
    /// The page has a password field: it is in the blind zone.
    pub password: bool,
}

/// The assistant's own browser window, driven over the DevTools protocol.
/// Every action returns the page as it is after the action.
#[async_trait]
pub trait Browser: Send + Sync {
    /// Last page seen, for risk checks before an action.
    fn last(&self) -> Option<Page>;
    async fn open(&self, url: &str) -> Result<Page, String>;
    /// With `wait_for`, waits up to `wait` until the page text contains it.
    async fn read(&self, wait_for: Option<&str>, wait: Duration) -> Result<Page, String>;
    /// A click that opens a new tab moves the browser to that tab.
    async fn click(&self, n: u32) -> Result<Page, String>;
    /// For a drop-down list `text` names the option to choose.
    async fn type_into(&self, n: u32, text: &str, submit: bool) -> Result<Page, String>;
    async fn scroll(&self, down: bool) -> Result<Page, String>;
    async fn back(&self) -> Result<Page, String>;
}

pub trait Trash: Send + Sync {
    fn trash(&self, path: &Path) -> Result<(), String>;
}

pub trait RdpSource: Send + Sync {
    /// Hosts from the Remote Desktop history.
    fn saved_hosts(&self) -> Vec<String>;
    /// `.rdp` files in Documents and on the Desktop.
    fn rdp_files(&self) -> Vec<PathBuf>;
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<RunOutput, String>;

    /// Like `run`, in the folder `cwd`, and gives every line of the output to
    /// `on_stdout` as it comes. Keeps only the tail of long output.
    async fn run_streaming(
        &self,
        program: &str,
        args: &[String],
        _cwd: Option<&Path>,
        timeout: Duration,
        cancel: CancellationToken,
        on_stdout: &(dyn for<'l> Fn(&'l str) + Send + Sync),
    ) -> Result<RunOutput, String> {
        let out = self.run(program, args, timeout, cancel).await?;
        for line in out.stdout.lines() {
            on_stdout(line);
        }
        Ok(out)
    }

    /// Like `run_streaming`, for a program that is told when to finish: when `quit` fires,
    /// `q` is written to its input and it gets `quit_wait` to end by itself, then it is killed.
    #[allow(clippy::too_many_arguments)]
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
        // Without an input to write to, the run is just left when asked to finish.
        tokio::select! {
            r = self.run_streaming(program, args, cwd, timeout, cancel, on_stdout) => r,
            _ = quit.cancelled() => Ok(RunOutput {
                code: Some(0),
                ..Default::default()
            }),
        }
    }

    /// Blocks the calling thread until the program ends or `timeout` passes.
    /// For a quick look at a file from a synchronous risk check.
    fn run_blocking(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<RunOutput, String>;
}

#[async_trait]
pub trait Http: Send + Sync {
    async fn get(&self, url: &str) -> Result<String, String>;
}

/// Short system facts for `system_info`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SystemFacts {
    pub cpu_percent: f32,
    pub memory_used_mb: u64,
    pub memory_total_mb: u64,
    pub uptime_s: u64,
    /// (mount, free GB, total GB)
    pub disks: Vec<(String, f64, f64)>,
    pub os: String,
    pub host: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub memory_mb: u64,
    pub cpu_percent: f32,
}

pub trait SystemProbe: Send + Sync {
    fn facts(&self) -> SystemFacts;
    fn processes(&self) -> Vec<ProcessInfo>;
    /// Charge in percent and whether it charges; `None` without a battery.
    fn battery(&self) -> Option<(u8, bool)> {
        None
    }
}

/// Everything tools need from the outside world.
#[derive(Clone)]
pub struct ToolEnv {
    pub launcher: Arc<dyn Launcher>,
    pub volume: Arc<dyn Volume>,
    pub power: Arc<dyn Power>,
    pub windows: Arc<dyn Windows>,
    pub clipboard: Arc<dyn Clipboard>,
    pub trash: Arc<dyn Trash>,
    pub rdp: Arc<dyn RdpSource>,
    pub runner: Arc<dyn Runner>,
    pub http: Arc<dyn Http>,
    pub probe: Arc<dyn SystemProbe>,
    pub ui: Arc<dyn Ui>,
    pub input: Arc<dyn Input>,
    pub screen: Arc<dyn Screen>,
    pub media: Arc<dyn Media>,
    pub radios: Arc<dyn Radios>,
    pub browser: Arc<dyn Browser>,
    /// What the watcher of events and "wait and tell" asks the system.
    pub sensors: Arc<dyn crate::observe::Sensors>,
    pub db: Arc<Db>,
    /// `%USERPROFILE%`.
    pub user_profile: String,
    pub shared: Shared,
}

/// Parts the application owns and tools only use. Settings here change at once.
#[derive(Clone)]
pub struct Shared {
    /// `tools.web_enabled` from the config.
    pub web_enabled: Arc<AtomicBool>,
    /// `privacy.screen_enabled` from the config.
    pub screen_enabled: Arc<AtomicBool>,
    /// `tools.news_feed` from the config.
    pub news_feed: Arc<RwLock<String>>,
    /// `island.city` from the config: the user's city for the weather.
    pub home_city: Arc<RwLock<String>>,
    pub memory: Memory,
    pub vision: Arc<dyn Vision>,
    pub images: Arc<dyn ImageGen>,
    pub bus: EventBus,
    /// Plan of the running task; the agent reads the same board.
    pub tasks: mika_core::task::TaskBoard,
    /// People whose messages she answers for the user; the notification reader uses it too.
    pub watches: crate::watch::ChatWatches,
    /// Focus mode: the tool turns it on, the app keeps quiet meanwhile.
    pub focus: crate::focus::Focus,
    /// Copied texts, filled by the app while `clipboard_on` is set.
    pub clips: crate::cliphist::ClipHistory,
    /// `tracking.clipboard` from the config.
    pub clipboard_on: Arc<AtomicBool>,
    /// `tracking.app_time` from the config.
    pub app_time_on: Arc<AtomicBool>,
    /// Meetings read from the calendar link by the app.
    pub calendar: crate::calendar::CalendarState,
    /// Pieces of the user's documents.
    pub docs: crate::docs::DocIndex,
    /// `tools.ffmpeg_path` from the config, read at every call.
    pub ffmpeg_path: Arc<RwLock<String>>,
    /// Speech recognition, the same as for the microphone.
    pub stt: Arc<dyn SpeechToText>,
    /// Files the video and sound tools made: only these open from the chat card.
    pub av_outputs: crate::ffmpeg::Outputs,
    /// Scenarios by event: the one store of the process.
    pub triggers: mika_core::triggers::TriggerStore,
    /// What she was asked to watch and tell about.
    pub waits: crate::observe::Waits,
    /// Background jobs: long work with video and sound, a screen recording.
    pub jobs: mika_core::bgjobs::JobBoard,
    /// What a screen recording needs from the application.
    pub rec: crate::ffmpeg::RecordDeps,
    /// A model for tools that need plain text; it gets no tools.
    pub text_model: Arc<dyn TextModel>,
    /// `[research]` from the config, read at every call.
    pub research: Arc<RwLock<mika_core::config::ResearchConfig>>,
    /// When deep searches ran in this run of the app. It lives here, not in
    /// the tool: the registry is rebuilt, and the record must outlive it.
    pub research_runs: Arc<crate::research::ResearchRuns>,
    /// Where her settings change by conversation (decision 0046). The
    /// application fills it once it runs; empty, the tools say so.
    pub settings: mika_core::settings::SettingsSlot,
}
