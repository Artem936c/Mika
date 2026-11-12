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

    fn start_apps(&self) -> Vec<(String, String)> {
        static APPS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
        APPS.get_or_init(read_start_apps).clone()
    }
}

/// `Get-StartApps`: every program of the Start menu with its AppID, Store apps
/// (Telegram from the Store, for one) included. Read once, about a second.
fn read_start_apps() -> Vec<(String, String)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-StartApps | Select-Object Name,AppID | ConvertTo-Json -Compress",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    parse_start_apps(&String::from_utf8_lossy(&out.stdout))
}

/// One object or a list of `{"Name": …, "AppID": …}`.
pub fn parse_start_apps(json: &str) -> Vec<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).unwrap_or_default();
    let items = match v {
        serde_json::Value::Array(a) => a,
        o @ serde_json::Value::Object(_) => vec![o],
        _ => Vec::new(),
    };
    items
        .iter()
        .filter_map(|i| {
            Some((
                i["Name"].as_str()?.to_string(),
                i["AppID"].as_str()?.to_string(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod start_apps_tests {
    #[test]
    fn start_apps_are_read_from_one_or_many() {
        let many = r#"[{"Name":"Telegram Desktop","AppID":"TelegramMessengerLLP.TelegramDesktop_t4vj0pshhgkwm!Telegram"},{"Name":"Блокнот","AppID":"x"}]"#;
        assert_eq!(super::parse_start_apps(many).len(), 2);
        let one = r#"{"Name":"Telegram","AppID":"a!b"}"#;
        assert_eq!(
            super::parse_start_apps(one),
            vec![("Telegram".into(), "a!b".into())]
        );
        assert!(super::parse_start_apps("").is_empty());
    }
}
