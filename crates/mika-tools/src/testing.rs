//! Fakes of the system interfaces for tool tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::events::EventBus;
use mika_core::gate::Approval;
use mika_core::memory::Memory;
use mika_core::storage::Db;
use mika_core::tools::{Tool, ToolError, ToolOutput};
use parking_lot::Mutex;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::*;

#[derive(Default)]
pub struct Fakes {
    pub opened: Mutex<Vec<(String, Option<String>)>>,
    pub volume: Mutex<(u8, bool)>,
    pub power: Mutex<Vec<PowerAction>>,
    pub windows: Mutex<Vec<WindowInfo>>,
    pub focused: Mutex<Vec<u64>>,
    pub closed: Mutex<Vec<u64>>,
    pub clipboard: Mutex<String>,
    pub trashed: Mutex<Vec<PathBuf>>,
    pub hosts: Mutex<Vec<String>>,
    pub rdp_files: Mutex<Vec<PathBuf>>,
    pub runs: Mutex<Vec<(String, Vec<String>, Duration)>>,
    pub run_output: Mutex<RunOutput>,
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
    async fn read(&self) -> Result<Page, String> {
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
}

impl Input for Fake {
    fn chord(&self, keys: &[u16]) -> Result<(), String> {
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
}

impl Screen for Fake {
    fn capture(&self, window: Option<u64>, max_side: u32) -> Result<Shot, String> {
        self.0.captures.lock().push(window);
        // A 3200 px wide screen: shrinks when a limit is given.
        let factor = if max_side == 0 { 1.0 } else { 2.0 };
        Ok(Shot {
            png: b"\x89PNG".to_vec(),
            width: (3200.0 / factor) as u32,
            height: (1800.0 / factor) as u32,
            factor,
        })
    }
    fn ocr(&self, _png: &[u8]) -> Result<String, String> {
        Ok("текст с экрана".into())
    }
}

#[async_trait]
impl Vision for Fake {
    async fn ask(&self, png: &[u8], question: &str) -> Result<String, String> {
        Ok(format!("вижу {} байт, вопрос: {question}", png.len()))
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

impl Clipboard for Fake {
    fn get(&self) -> Result<String, String> {
        Ok(self.0.clipboard.lock().clone())
    }
    fn set(&self, text: &str) -> Result<(), String> {
        *self.0.clipboard.lock() = text.into();
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

#[async_trait]
impl Runner for Fake {
    async fn run(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
        _cancel: CancellationToken,
    ) -> Result<RunOutput, String> {
        self.0
            .runs
            .lock()
            .push((program.into(), args.to_vec(), timeout));
        Ok(self.0.run_output.lock().clone())
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
        user_profile: user_profile.into(),
        shared: Shared {
            web_enabled: Arc::new(AtomicBool::new(true)),
            screen_enabled: Arc::new(AtomicBool::new(false)),
            news_feed: Arc::new(parking_lot::RwLock::new(
                "https://feed.example/rss".to_string(),
            )),
            memory: Memory::new(db.clone(), None),
            vision: fake(),
            images: fake(),
            bus: EventBus::new(64),
            tasks: mika_core::task::TaskBoard::new(EventBus::new(64)),
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
