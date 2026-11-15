//! Process-level Windows glue: the journal, single instance, autostart, wiping data.

use std::path::PathBuf;
use std::sync::Arc;

use mika_core::config::Config;
use mika_core::journal::{self, Journal};
use mika_core::paths::{config_path, data_dir, logs_dir, models_dir};
use mika_core::secrets::SecretStore;

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "Mika";
const WINDOWS_KEY: &str = "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion";

/// Opens the journal of the app in the logs folder and makes it the sink of
/// the `log` macros (decision 0036). Its settings are the `[log]` section of
/// `config.toml`, read here because the journal starts before everything
/// else; a file that cannot be read gives the defaults, and `Ctx::build`
/// reports the error. A panic leaves a record, and the run is marked as
/// started. Every record also goes to stderr, which a console build shows.
///
/// Call it only in the single instance: two processes cannot write and
/// rotate the files of one folder.
pub fn init_logging() -> Option<Arc<Journal>> {
    let config = Config::load(&config_path())
        .map(|c| c.log)
        .unwrap_or_default();
    journal::install_panic_hook();
    let installed = journal::install(journal::Options {
        dir: Some(logs_dir()),
        stderr: true,
        config,
        ..Default::default()
    });
    match installed {
        Ok(journal) => {
            journal.begin_run();
            Some(journal)
        }
        Err(e) => {
            eprintln!("the journal is not installed: {e}");
            None
        }
    }
}

/// The run ends in an orderly way: the marker of the run is removed and what
/// is buffered reaches the file. Records written later are still kept; call
/// `mika_core::journal::flush` after the last of them.
pub fn end_logging() {
    if let Some(journal) = journal::global() {
        journal.end_run();
    }
}

/// `Windows 11 Pro 24H2 (build 26100.2314)` from what the registry tells.
/// Parts that are missing are left out.
fn windows_text(product: &str, display: &str, build: &str, revision: Option<u32>) -> String {
    let mut name = product.trim().to_string();
    // The registry of Windows 11 still says "Windows 10": builds from 22000 on are 11.
    if build.trim().parse::<u32>().is_ok_and(|b| b >= 22000) {
        name = name.replacen("Windows 10", "Windows 11", 1);
    }
    if name.is_empty() {
        name = "Windows".into();
    }
    if !display.trim().is_empty() {
        name = format!("{name} {}", display.trim());
    }
    match (build.trim(), revision) {
        ("", _) => name,
        (build, Some(revision)) => format!("{name} (build {build}.{revision})"),
        (build, None) => format!("{name} (build {build})"),
    }
}

/// The edition and build of Windows, for the journal and the problem report.
pub fn windows_build() -> String {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(WINDOWS_KEY, winreg::enums::KEY_READ);
    let Ok(key) = key else {
        return "Windows".into();
    };
    let text = |name: &str| key.get_value::<String, _>(name).unwrap_or_default();
    windows_text(
        &text("ProductName"),
        &text("DisplayVersion"),
        &text("CurrentBuild"),
        key.get_value::<u32, _>("UBR").ok(),
    )
}

/// Holds the named mutex for the life of the process.
pub struct InstanceGuard(#[allow(dead_code)] isize);

/// Name of the single-instance mutex. A separate data folder (`MIKA_DATA_DIR`, used
/// for test profiles) is a separate assistant and gets its own name.
pub fn instance_name(data_dir: Option<&str>) -> String {
    match data_dir.filter(|d| !d.is_empty()) {
        Some(dir) => {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            dir.to_lowercase().hash(&mut h);
            format!("Local\\MikaAssistantSingleInstance-{:016x}", h.finish())
        }
        None => "Local\\MikaAssistantSingleInstance".into(),
    }
}

/// `None` when another copy is already running.
pub fn single_instance() -> Option<InstanceGuard> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let dir = std::env::var("MIKA_DATA_DIR").ok();
    let name: Vec<u16> = format!("{}\0", instance_name(dir.as_deref()))
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
    folders_for(data_dir(), vec![logs_dir(), models_dir()])
}

/// The data folder and the machine-local folders that hold the user's own
/// things: the journal and the avatar models. Each of them by itself: their
/// parent `%LOCALAPPDATA%\Mika` is also where the installer puts the program.
fn folders_for(data: PathBuf, local: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut v = vec![data.clone()];
    for folder in local {
        if !folder.starts_with(&data) {
            v.push(folder);
        }
    }
    v
}

/// Deletes every key whose name does not depend on the settings. The list is
/// the one a backup collects its keys by, so nothing kept is left behind.
fn wipe_keys(secrets: &dyn SecretStore) -> Result<(), String> {
    for key in mika_core::backup::fixed_secret_keys() {
        secrets.delete(&key).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Longest wait for this process to end before a script that runs after it
/// goes on, seconds. The exit stops the background jobs and saves a screen
/// recording first (`Ctx::shutdown_media`), and that may take three minutes.
const EXIT_WAIT_S: u32 = 240;

/// The start of a `cmd.exe` script whose work must not begin before the
/// process `pid` has ended: it waits for that process, then two more seconds.
///
/// A fixed pause alone is not enough. While this process saves a recording at
/// its exit it still owns the name of the single instance and keeps the
/// database open: a copy of the app started meanwhile would quit at once, and
/// a folder removal would leave the open files behind. Without PowerShell the
/// wait is skipped and only the pause is left, as before.
fn after_exit(pid: u32) -> String {
    format!(
        "%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe -NoProfile -NonInteractive \
         -Command \"Wait-Process -Id {pid} -Timeout {EXIT_WAIT_S} -ErrorAction SilentlyContinue\" \
         & ping -n 3 127.0.0.1 >nul"
    )
}

/// A hidden `cmd.exe` that runs `then` after the process `pid` has ended.
fn after_exit_command(pid: u32, then: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = std::process::Command::new("cmd.exe");
    // `/S`: the outer quotes are taken off and the rest is run as written.
    cmd.raw_arg("/D /S /C")
        .raw_arg(format!("\"{} & {then}\"", after_exit(pid)))
        .creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Runs the `cmd.exe` commands `then` once this process has ended, however
/// long its exit takes. Call it right before `app.exit`.
pub fn run_after_exit(then: &str) -> std::io::Result<()> {
    after_exit_command(std::process::id(), then)
        .spawn()
        .map(|_| ())
}

/// The commands that remove `folders`: each one again after a pause when
/// something of it is left (a file a child process had not let go of yet).
fn removal_script(folders: &[PathBuf]) -> String {
    folders
        .iter()
        .map(|dir| {
            let dir = dir.display();
            format!(
                "rmdir /s /q \"{dir}\" & if exist \"{dir}\" (ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{dir}\")"
            )
        })
        .collect::<Vec<_>>()
        .join(" & ")
}

/// Deletes keys and autostart now, and the folders once the app has ended:
/// the database file is open until then, and the exit may take a while when
/// a screen recording is being saved.
pub fn wipe_all_data(secrets: &dyn SecretStore) -> Result<(), String> {
    wipe_keys(secrets)?;
    set_autostart(false)?;
    run_after_exit(&removal_script(&data_folders()))
        .map_err(|e| format!("cannot schedule folder removal: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profiles_get_their_own_instance() {
        let main = instance_name(None);
        assert_eq!(main, instance_name(Some("")));
        let a = instance_name(Some(r"C:\tmp\a"));
        assert_ne!(a, main);
        assert_eq!(a, instance_name(Some(r"C:\TMP\A")));
        assert_ne!(a, instance_name(Some(r"C:\tmp\b")));
    }

    #[test]
    fn windows_is_named_by_edition_release_and_build() {
        assert_eq!(
            windows_text("Windows 10 Pro", "24H2", "26100", Some(2314)),
            "Windows 11 Pro 24H2 (build 26100.2314)"
        );
        assert_eq!(
            windows_text("Windows 10 Home", "22H2", "19045", Some(5131)),
            "Windows 10 Home 22H2 (build 19045.5131)"
        );
        assert_eq!(
            windows_text("Windows Server 2025 Standard", "24H2", "26100", None),
            "Windows Server 2025 Standard 24H2 (build 26100)"
        );
        // Whatever is missing is left out.
        assert_eq!(windows_text("", "", "", Some(1)), "Windows");
        assert_eq!(
            windows_text(" ", "", "19045", None),
            "Windows (build 19045)"
        );
        // This machine tells something, and on one line.
        let here = windows_build();
        assert!(
            here.starts_with("Windows") && !here.contains('\n'),
            "{here}"
        );
    }

    #[test]
    fn wipe_never_touches_the_install_folder() {
        let install = PathBuf::from(r"C:\Users\me\AppData\Local\Mika");
        let v = folders_for(
            PathBuf::from(r"C:\Users\me\AppData\Roaming\Mika"),
            vec![install.join("logs"), install.join("models")],
        );
        assert_eq!(
            v,
            vec![
                PathBuf::from(r"C:\Users\me\AppData\Roaming\Mika"),
                install.join("logs"),
                // The avatar models the user brought in are their data too.
                install.join("models")
            ]
        );
        assert!(!v.contains(&install));
    }

    #[test]
    fn wipe_deletes_every_fixed_key_the_calendar_link_included() {
        use mika_core::secrets::{MemorySecretStore, Secret};
        let keys = mika_core::backup::fixed_secret_keys();
        assert!(keys.contains(&mika_core::config::CALENDAR_SECRET_KEY.to_string()));
        assert!(keys.contains(&"calendar.ics_url".to_string()));
        assert_eq!(keys.len(), 8);
        let store = MemorySecretStore::default();
        for key in &keys {
            store.set(key, &Secret::new("value")).unwrap();
        }
        wipe_keys(&store).unwrap();
        for key in &keys {
            assert_eq!(store.get(key).unwrap(), None, "{key}");
        }
    }

    #[test]
    fn what_runs_after_the_exit_waits_for_the_process_itself() {
        let script = after_exit(4242);
        let wait = script.find("Wait-Process -Id 4242 ").unwrap();
        // The pause comes after the wait: it is what is left without PowerShell.
        assert!(wait < script.find("ping -n 3").unwrap());
        assert!(script.contains(&format!("-Timeout {EXIT_WAIT_S} ")));
        // Longer than the longest exit: jobs are given 185 s and the cleanup 3 s.
        const { assert!(EXIT_WAIT_S > 185 + 3) };

        // A process that lives for about five seconds, like an exit that saves
        // a recording. The old script paused for two seconds and went on.
        let mut slow = std::process::Command::new("ping.exe")
            .args(["-n", "6", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mark = dir.path().join("after.txt");
        let then = format!("echo done>\"{}\"", mark.display());
        let status = after_exit_command(slow.id(), &then).status().unwrap();
        assert!(status.success());
        // The work was done, and only after the process had ended.
        assert!(
            slow.try_wait().unwrap().is_some(),
            "the script went on while the process was still alive"
        );
        assert!(mark.exists());
    }

    #[test]
    fn folders_are_removed_again_when_something_is_left() {
        let script = removal_script(&[PathBuf::from(r"C:\a b"), PathBuf::from(r"D:\logs")]);
        assert_eq!(script.matches("rmdir /s /q \"C:\\a b\"").count(), 2);
        assert_eq!(script.matches("rmdir /s /q \"D:\\logs\"").count(), 2);
        assert!(script.contains("if exist \"C:\\a b\" ("));

        // Run for real on a folder of the test: it is gone afterwards.
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("data folder");
        std::fs::create_dir_all(gone.join("inner")).unwrap();
        std::fs::write(gone.join("inner").join("mika.db"), b"x").unwrap();
        // A process that has already ended: nothing to wait for.
        let mut ended = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "exit"])
            .spawn()
            .unwrap();
        ended.wait().unwrap();
        let script = removal_script(std::slice::from_ref(&gone));
        // The script ends with `if exist` of a folder that is gone: its exit
        // code says nothing, the folder does.
        let _ = after_exit_command(ended.id(), &script).status().unwrap();
        assert!(!gone.exists());
        assert!(dir.path().exists());
    }

    #[test]
    fn logs_inside_data_dir_are_not_listed_twice() {
        let data = PathBuf::from(r"D:\mika");
        assert_eq!(
            folders_for(data.clone(), vec![data.join("logs"), data.join("models")]),
            vec![data]
        );
    }
}
