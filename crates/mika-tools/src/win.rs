//! Real Windows implementations of the system interfaces.

mod audio;
mod clipboard;
mod http;
mod launcher;
mod power;
mod probe;
mod rdp;
mod runner;
mod trash;
mod wnd;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use mika_core::storage::Db;

use crate::sys::ToolEnv;

pub use runner::ProcessRunner;

impl ToolEnv {
    /// Environment backed by the real system.
    pub fn windows(db: Arc<Db>, web_enabled: Arc<AtomicBool>) -> Self {
        let user_profile = std::env::var("USERPROFILE").unwrap_or_default();
        Self {
            launcher: Arc::new(launcher::ShellLauncher),
            volume: Arc::new(audio::CoreAudioVolume),
            power: Arc::new(power::WinPower),
            windows: Arc::new(wnd::WinWindows),
            clipboard: Arc::new(clipboard::WinClipboard),
            trash: Arc::new(trash::RecycleBin),
            rdp: Arc::new(rdp::WinRdp {
                user_profile: user_profile.clone(),
            }),
            runner: Arc::new(runner::ProcessRunner),
            http: Arc::new(http::ReqwestHttp::new()),
            probe: Arc::new(probe::SysinfoProbe),
            db,
            web_enabled,
            user_profile,
        }
    }
}

/// Runs `f` on a fresh thread with COM initialised. Shell and audio calls need it,
/// and tokio worker threads must not change their COM apartment.
fn with_com<T: Send + 'static>(
    apartment: windows::Win32::System::Com::COINIT,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    std::thread::spawn(move || {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize};
        // SAFETY: initialises COM for this new thread only; balanced below.
        let hr = unsafe { CoInitializeEx(None, apartment) };
        let out = f();
        if hr.is_ok() {
            // SAFETY: matches the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
        out
    })
    .join()
    .map_err(|_| "системный вызов упал".to_string())?
}

fn wide(s: &str) -> windows::core::HSTRING {
    windows::core::HSTRING::from(s)
}
