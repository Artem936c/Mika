use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::LockWorkStation;

use crate::sys::{Power, PowerAction};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct WinPower;

fn shutdown_exe(args: &[&str]) -> Result<(), String> {
    Command::new("shutdown.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("не удалось запустить shutdown.exe: {e}"))
}

impl Power for WinPower {
    fn act(&self, action: PowerAction) -> Result<(), String> {
        match action {
            // SAFETY: plain Win32 calls without pointers.
            PowerAction::Lock => unsafe { LockWorkStation() }
                .map_err(|e| format!("не удалось заблокировать: {}", e.message())),
            PowerAction::Sleep => {
                // SAFETY: plain Win32 call without pointers.
                if unsafe { SetSuspendState(false, false, false) } {
                    Ok(())
                } else {
                    Err("Windows отказался перейти в сон".into())
                }
            }
            // A short delay leaves time to read the answer.
            PowerAction::Restart => shutdown_exe(&["/r", "/t", "5"]),
            PowerAction::Shutdown => shutdown_exe(&["/s", "/t", "5"]),
            PowerAction::Logoff => shutdown_exe(&["/l"]),
        }
    }
}
