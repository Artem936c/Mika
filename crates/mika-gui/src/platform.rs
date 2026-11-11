//! Process-level Windows glue: log file, single instance, autostart, wiping data.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use mika_core::config::ProviderKind;
use mika_core::paths::{data_dir, logs_dir};
use mika_core::secrets::SecretStore;

const LOG_LIMIT: u64 = 5 * 1024 * 1024;
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "Mika";

/// Writes every log line to stderr and to `mika.log`.
struct Tee(Option<File>);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        if let Some(f) = &mut self.0 {
            let _ = f.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(f) = &mut self.0 {
            f.flush()?;
        }
        Ok(())
    }
}

/// Opens `mika.log`; a file over 5 MB becomes `mika.old.log`.
fn open_log(dir: &Path) -> Option<File> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join("mika.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_LIMIT) {
        let _ = std::fs::rename(&path, dir.join("mika.old.log"));
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

pub fn init_logging() {
    let file = open_log(&logs_dir());
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(Tee(file))))
        .init();
}

/// Holds the named mutex for the life of the process.
pub struct InstanceGuard(#[allow(dead_code)] isize);

/// `None` when another copy is already running.
pub fn single_instance() -> Option<InstanceGuard> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\MikaAssistantSingleInstance\0"
        .encode_utf16()
        .collect();
    // SAFETY: valid zero-terminated name; the handle lives until the process exits.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    // SAFETY: plain call right after CreateMutexW.
    if handle.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return None;
    }
    Some(InstanceGuard(handle as isize))
}

fn run_key() -> std::io::Result<winreg::RegKey> {
    winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .create_subkey(RUN_KEY)
        .map(|(k, _)| k)
}

pub fn autostart_enabled() -> bool {
    run_key()
        .and_then(|k| k.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let key = run_key().map_err(|e| e.to_string())?;
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        key.set_value(RUN_VALUE, &format!("\"{}\"", exe.display()))
            .map_err(|e| e.to_string())
    } else {
        match key.delete_value(RUN_VALUE) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}

/// Folders removed by "delete all data".
pub fn data_folders() -> Vec<PathBuf> {
    let mut v = vec![data_dir()];
    let logs = logs_dir();
    if !logs.starts_with(data_dir()) {
        v.push(logs.parent().map(Path::to_path_buf).unwrap_or(logs));
    }
    v
}

/// Deletes keys and autostart now, and the folders right after the app exits
/// (the database file is open until then).
pub fn wipe_all_data(secrets: &dyn SecretStore) -> Result<(), String> {
    for kind in [
        ProviderKind::Openai,
        ProviderKind::Anthropic,
        ProviderKind::Ollama,
    ] {
        secrets
            .delete(&kind.secret_key())
            .map_err(|e| e.to_string())?;
    }
    set_autostart(false)?;
    let mut script = String::from("ping -n 3 127.0.0.1 >nul");
    for dir in data_folders() {
        script.push_str(&format!(" & rmdir /s /q \"{}\"", dir.display()));
    }
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd.exe")
        .raw_arg("/D /C")
        .raw_arg(&script)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot schedule folder removal: {e}"))
}
