//! Fakes of the system interfaces for tool tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::gate::Approval;
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
}

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
        db,
        web_enabled: Arc::new(AtomicBool::new(true)),
        user_profile: user_profile.into(),
    };
    (env, f)
}

pub async fn run(tool: &dyn Tool, args: Value) -> Result<ToolOutput, ToolError> {
    tool.assess(&args)?;
    tool.run(args, Approval::for_tests(), CancellationToken::new())
        .await
}
