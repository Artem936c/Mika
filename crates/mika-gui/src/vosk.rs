//! Files of Vosk for the wake phrase: downloaded on the user's request,
//! checked against fixed SHA-256 sums, unpacked next to the logs.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mika_audio::wake::WakeEngine;
use sha2::{Digest, Sha256};

/// One archive to download: where from, its sum, and what to take from it.
struct Part {
    url: &'static str,
    sha256: &'static str,
    /// Folder inside the archive.
    inner: &'static str,
    /// Folder under `vosk_dir` it becomes.
    target: &'static str,
}

/// libvosk for Windows from vosk-api 0.3.45 (Apache 2.0), about 15 MB.
const LIBRARY: Part = Part {
    url: "https://github.com/alphacep/vosk-api/releases/download/v0.3.45/vosk-win64-0.3.45.zip",
    sha256: "f1dcc9cca460630f81ea8f71794f69c80bed6556d2a4e6237b5785e1d2dff34b",
    inner: "vosk-win64-0.3.45",
    target: "lib",
};

/// Small Russian model 0.22 (Apache 2.0), about 45 MB.
const MODEL: Part = Part {
    url: "https://alphacephei.com/vosk/models/vosk-model-small-ru-0.22.zip",
    sha256: "961d5ff98a17f4aa6de69864d0aa71fa5bac682301d2b5d17a3f24c5c99a46d4",
    inner: "vosk-model-small-ru-0.22",
    target: "model",
};

/// Files a build needs, not the running app: they are not kept.
const NOT_NEEDED: [&str; 2] = ["libvosk.lib", "vosk_api.h"];

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Set while a download runs, so a second press does nothing.
static BUSY: AtomicBool = AtomicBool::new(false);

fn library_dir() -> PathBuf {
    mika_core::paths::vosk_dir().join(LIBRARY.target)
}

fn model_dir() -> PathBuf {
    mika_core::paths::vosk_dir().join(MODEL.target)
}

/// True when both the library and the model are in place.
pub fn installed() -> bool {
    library_dir().join(mika_audio::vosk::LIBRARY).is_file()
        && model_dir().join("am").join("final.mdl").is_file()
}

/// Vosk when its files are in place.
pub fn engine() -> Option<WakeEngine> {
    installed().then(|| WakeEngine::Vosk {
        library: library_dir(),
        model: model_dir(),
    })
}

pub fn downloading() -> bool {
    BUSY.load(Ordering::Relaxed)
}

/// Bytes the files take on disk.
pub fn size() -> u64 {
    fn walk(dir: &Path) -> u64 {
        std::fs::read_dir(dir)
            .map(|list| {
                list.filter_map(Result::ok)
                    .map(|e| match e.metadata() {
                        Ok(m) if m.is_dir() => walk(&e.path()),
                        Ok(m) => m.len(),
                        Err(_) => 0,
                    })
                    .sum()
            })
            .unwrap_or(0)
    }
    walk(&mika_core::paths::vosk_dir())
}

/// Downloads both archives, checks them and unpacks. `progress` gets the
/// share done, 0..=100. Does nothing while another download runs.
pub async fn install(progress: impl Fn(u8)) -> Result<(), String> {
    if BUSY.swap(true, Ordering::Relaxed) {
        return Err(crate::strings::tr("error.vosk_busy"));
    }
    let began = std::time::Instant::now();
    log::info!("vosk download started");
    let result = install_parts(&progress).await;
    match &result {
        Ok(()) => log::info!(
            "vosk downloaded and unpacked: ms={} bytes={}",
            began.elapsed().as_millis(),
            size()
        ),
        Err(e) => log::error!(
            "vosk not downloaded, the name is heard by Windows: ms={} error={}",
            began.elapsed().as_millis(),
            mika_core::journal::error_text(e)
        ),
    }
    BUSY.store(false, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(mika_core::paths::vosk_dir().join(".download"));
    result
}

async fn install_parts(progress: &impl Fn(u8)) -> Result<(), String> {
    let root = mika_core::paths::vosk_dir();
    let work = root.join(".download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    // The model is three times the library: shares of the bar follow that.
    let parts = [(&LIBRARY, 0u8, 25u8), (&MODEL, 25, 75)];
    for (part, from, span) in parts {
        let zip = work.join(format!("{}.zip", part.target));
        download(&client, part, &zip, |share| {
            progress(from + (share as u16 * span as u16 / 100) as u8)
        })
        .await?;
        let out = work.join(part.target);
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        unzip(&zip, &out).await?;
        let unpacked = out.join(part.inner);
        for extra in NOT_NEEDED {
            let _ = std::fs::remove_file(unpacked.join(extra));
        }
        let target = root.join(part.target);
        let _ = std::fs::remove_dir_all(&target);
        std::fs::rename(&unpacked, &target).map_err(|e| e.to_string())?;
    }
    progress(100);
    if installed() {
        Ok(())
    } else {
        Err(crate::strings::tr("error.vosk_incomplete"))
    }
}

/// Streams one archive to `path`, hashing as it goes.
async fn download(
    client: &reqwest::Client,
    part: &Part,
    path: &Path,
    progress: impl Fn(u8),
) -> Result<(), String> {
    let failed = |e: reqwest::Error| {
        crate::strings::tr("error.vosk_download").replace("{why}", &e.without_url().to_string())
    };
    let mut resp = client
        .get(part.url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(failed)?;
    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut done = 0u64;
    while let Some(chunk) = resp.chunk().await.map_err(failed)? {
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        if let Some(share) = (done * 100).checked_div(total) {
            progress(share.min(100) as u8);
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    let sum: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if sum != part.sha256 {
        return Err(crate::strings::tr("error.vosk_checksum"));
    }
    Ok(())
}

/// Unpacks a zip with the `tar` of Windows, which refuses paths that leave
/// the target folder.
async fn unzip(zip: &Path, out: &Path) -> Result<(), String> {
    let windows = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let tar = PathBuf::from(windows).join("System32").join("tar.exe");
    let status = tokio::process::Command::new(tar)
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(out)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(crate::strings::tr("error.vosk_unpack"))
    }
}

/// Deletes the downloaded files; the Windows recognizer listens after that.
pub fn remove() -> Result<(), String> {
    if downloading() {
        return Err(crate::strings::tr("error.vosk_busy"));
    }
    match std::fs::remove_dir_all(mika_core::paths::vosk_dir()) {
        Ok(()) => {
            log::info!("vosk removed");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            log::warn!("vosk not removed: {e}");
            Err(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    /// Downloads the real archives into a temporary data folder.
    #[test]
    #[ignore = "downloads about 60 MB"]
    fn install_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIKA_DATA_DIR", dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        rt.block_on(super::install(|p| seen.lock().unwrap().push(p)))
            .unwrap();
        assert!(super::installed());
        assert!(super::engine().is_some());
        assert_eq!(seen.lock().unwrap().last(), Some(&100));
        let lib = super::library_dir();
        assert!(!lib.join("libvosk.lib").exists());
        assert!(!mika_core::paths::vosk_dir().join(".download").exists());
        println!("size {} MB", super::size() / 1_000_000);
        super::remove().unwrap();
        assert!(!super::installed());
    }
}
