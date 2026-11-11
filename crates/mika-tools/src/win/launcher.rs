use windows::core::PCWSTR;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::{wide, with_com};
use crate::sys::Launcher;

pub struct ShellLauncher;

impl Launcher for ShellLauncher {
    fn open(&self, target: &str, args: Option<&str>) -> Result<(), String> {
        let target = target.to_string();
        let args = args.map(str::to_string);
        with_com(
            COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
            move || {
                let file = wide(&target);
                let params = args.as_deref().map(wide);
                // SAFETY: all strings live until the call returns.
                let code = unsafe {
                    ShellExecuteW(
                        None,
                        &wide("open"),
                        &file,
                        params
                            .as_ref()
                            .map(|p| PCWSTR(p.as_ptr()))
                            .unwrap_or(PCWSTR::null()),
                        PCWSTR::null(),
                        SW_SHOWNORMAL,
                    )
                };
                // Values above 32 mean success.
                if code.0 as isize > 32 {
                    Ok(())
                } else {
                    Err(format!(
                        "Windows не смог открыть «{target}» (код {})",
                        code.0 as isize
                    ))
                }
            },
        )
    }
}
