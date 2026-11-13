//! Real Windows implementations of the system interfaces.

mod audio;
mod browser;
mod clipboard;
mod http;
mod input;
mod launcher;
mod media;
pub mod pdf;
mod power;
mod probe;
mod radio;
mod rdp;
mod runner;
mod screen;
mod trash;
mod uia;
mod wnd;

use std::sync::Arc;

use mika_core::storage::Db;

use crate::sys::{Shared, ToolEnv};

pub use clipboard::{excluded_from_history, sequence as clipboard_sequence};
pub use runner::ProcessRunner;

impl ToolEnv {
    /// Environment backed by the real system.
    pub fn windows(db: Arc<Db>, shared: Shared) -> Self {
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
            ui: Arc::new(uia::UiaUi::new()),
            input: Arc::new(input::SendInputDevice),
            screen: Arc::new(screen::GdiScreen),
            media: Arc::new(media::WinMedia),
            radios: Arc::new(radio::WinRadios),
            browser: Arc::new(browser::CdpBrowser::new(
                mika_core::paths::data_dir().join("browser"),
            )),
            db,
            user_profile,
            shared,
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

/// This thread sees and sets every coordinate in real screen pixels, on every
/// monitor whatever its scale. Without it a picture and a click may be counted
/// in different pixels when monitors have different scaling.
pub(crate) fn physical_pixels() {
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    // SAFETY: changes only the calling thread's DPI mode.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn wide(s: &str) -> windows::core::HSTRING {
    windows::core::HSTRING::from(s)
}

#[cfg(test)]
mod live {
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::sys::Windows;

    /// Pastes a picture into a real window with the same code that sends one
    /// to Telegram, and presses Enter. `MIKA_PASTE_FILE` is the picture,
    /// `MIKA_PASTE_TITLE` and `MIKA_PASTE_PROCESS` name the window.
    #[tokio::test]
    #[ignore = "pastes into a real window and presses Enter"]
    async fn live_paste_a_picture_into_a_window() {
        let file = PathBuf::from(std::env::var("MIKA_PASTE_FILE").expect("MIKA_PASTE_FILE"));
        let title = std::env::var("MIKA_PASTE_TITLE").expect("MIKA_PASTE_TITLE");
        let process = std::env::var("MIKA_PASTE_PROCESS").expect("MIKA_PASTE_PROCESS");
        let windows: Arc<dyn Windows> = Arc::new(super::wnd::WinWindows);
        let w = windows
            .list()
            .into_iter()
            .find(|w| w.title == title && w.process == process)
            .expect("the window");
        let ui = crate::ui::UiEnv {
            windows,
            ui: Arc::new(super::uia::UiaUi::new()),
            input: Arc::new(super::input::SendInputDevice),
            bus: mika_core::EventBus::new(8),
            own_process: String::new(),
            seen: Default::default(),
            clipboard: Some(Arc::new(super::clipboard::WinClipboard)),
            sent: Default::default(),
            clips: Default::default(),
        };
        let tool = crate::messenger::MessengerSendTool(crate::screen::ScreenEnv {
            ui,
            screen: Arc::new(super::screen::GdiScreen),
            enabled: Default::default(),
            user_profile: String::new(),
        });
        tool.send_here(&w, "Тест Мики: картинка через буфер обмена", Some(&file))
            .await
            .unwrap();
    }
}
