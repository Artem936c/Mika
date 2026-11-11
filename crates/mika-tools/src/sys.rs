//! Narrow interfaces to the operating system. Tools use only these,
//! so tests replace them with fakes.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::storage::Db;
use tokio_util::sync::CancellationToken;

pub trait Launcher: Send + Sync {
    /// Opens a program, file, folder or URL with the shell, like double-click.
    fn open(&self, target: &str, args: Option<&str>) -> Result<(), String>;
}

pub trait Volume: Send + Sync {
    /// Level 0..=100 and mute flag.
    fn get(&self) -> Result<(u8, bool), String>;
    fn set(&self, level: u8) -> Result<(), String>;
    fn set_mute(&self, mute: bool) -> Result<(), String>;
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

pub trait Windows: Send + Sync {
    fn list(&self) -> Vec<WindowInfo>;
    fn focus(&self, id: u64) -> Result<(), String>;
    /// Asks the window to close, like the close button.
    fn close(&self, id: u64) -> Result<(), String>;
}

pub trait Clipboard: Send + Sync {
    fn get(&self) -> Result<String, String>;
    fn set(&self, text: &str) -> Result<(), String>;
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
    pub db: Arc<Db>,
    /// `tools.web_enabled` from the config; changes take effect at once.
    pub web_enabled: Arc<AtomicBool>,
    /// `%USERPROFILE%`.
    pub user_profile: String,
}
