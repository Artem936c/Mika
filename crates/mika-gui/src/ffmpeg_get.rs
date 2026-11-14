//! FFmpeg for the video and sound tools (decision 0027): downloaded only after
//! the user's press, from one pinned LGPL build, checked against its size and
//! SHA-256, and unpacked flat into `ffmpeg_dir`: ffmpeg.exe, ffprobe.exe and
//! the licence. A copy of FFmpeg the user has is never touched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::strings::tr;

/// FFmpeg 9.0.2 from FFmpeg-Builds, win64, LGPL, shared nothing.
const URL: &str = "https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/download/autobuild-2026-10-31-12-59/ffmpeg-n9.0.2-39-g7adfbe40dc-win64-lgpl-9.0.zip";
const SIZE: u64 = 168_715_328;
const SHA256: &str = "9d37c564e0d01c18cde184a079fa6ef04d7d2783ada41da81d04165cc8f1a650";
/// Folder inside the archive.
const INNER: &str = "ffmpeg-n9.0.2-39-g7adfbe40dc-win64-lgpl-9.0";
/// Files taken from the archive and the names they get in `ffmpeg_dir`.
const FILES: [(&str, &str); 3] = [
    ("bin/ffmpeg.exe", "ffmpeg.exe"),
    ("bin/ffprobe.exe", "ffprobe.exe"),
    ("LICENSE.txt", "LICENSE.txt"),
];
/// The archive in megabytes, for the offer in the window and the chat.
pub const DOWNLOAD_MB: u32 = 161;
/// GitHub sends release files through its storage hosts; no other host is followed.
const HOSTS: [&str; 3] = [
    "github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
];
const MAX_REDIRECTS: usize = 5;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Set while a download runs, so a second press does nothing.
static BUSY: AtomicBool = AtomicBool::new(false);

pub fn downloading() -> bool {
    BUSY.load(Ordering::Relaxed)
}

/// True when the downloaded copy is in place.
pub fn installed() -> bool {
    let dir = mika_core::paths::ffmpeg_dir();
    dir.join("ffmpeg.exe").is_file() && dir.join("ffprobe.exe").is_file()
}

/// Bytes the downloaded copy takes on disk.
pub fn size() -> u64 {
    std::fs::read_dir(mika_core::paths::ffmpeg_dir())
        .map(|list| {
            list.filter_map(Result::ok)
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Where the archive is downloaded and unpacked: next to `ffmpeg_dir`, so the
/// final rename stays on one disk.
fn work_dir() -> PathBuf {
    mika_core::paths::ffmpeg_dir().with_file_name("ffmpeg.download")
}

/// Only https and the hosts GitHub serves release files from.
fn allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https" && url.host_str().is_some_and(|h| HOSTS.contains(&h))
}

/// Clears the busy mark and the work folder however the download ends.
struct Busy;

impl Drop for Busy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(work_dir());
        BUSY.store(false, Ordering::Relaxed);
    }
}

/// Downloads the archive, checks it, unpacks the three files and puts them in
/// `ffmpeg_dir`. `progress` gets the share done, 0..=100.
pub async fn install(progress: impl Fn(u8) + Send + Sync) -> Result<(), String> {
    if BUSY.swap(true, Ordering::Relaxed) {
        return Err(tr("error.ffmpeg_busy"));
    }
    let _busy = Busy;
    let began = std::time::Instant::now();
    log::info!(target: "jobs", "ffmpeg download started");
    let step = Step::default();
    let result = fetch(progress, &step).await;
    match &result {
        Ok(()) => log::info!(
            target: "jobs",
            "ffmpeg downloaded and unpacked: ms={} bytes={}",
            began.elapsed().as_millis(),
            size()
        ),
        Err(e) => log::error!(
            target: "jobs",
            "ffmpeg not downloaded: step={} ms={} error={}",
            step.name(),
            began.elapsed().as_millis(),
            mika_core::journal::error_text(e)
        ),
    }
    result
}

/// How far `fetch` got: the same "access denied" means different things
/// while downloading, unpacking and putting the files in place.
#[derive(Default)]
struct Step(std::sync::atomic::AtomicUsize);

const STEPS: [&str; 5] = ["prepare", "download", "unpack", "pick_files", "replace"];

impl Step {
    fn set(&self, name: &str) {
        if let Some(at) = STEPS.iter().position(|step| *step == name) {
            self.0.store(at, Ordering::Relaxed);
        }
    }

    fn name(&self) -> &'static str {
        STEPS[self.0.load(Ordering::Relaxed).min(STEPS.len() - 1)]
    }
}

/// `install` without its records; the caller holds the busy mark.
async fn fetch(progress: impl Fn(u8) + Send + Sync, step: &Step) -> Result<(), String> {
    step.set("prepare");
    let work = work_dir();
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;

    let refused = Arc::new(AtomicBool::new(false));
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .redirect(redirect_policy(refused.clone()))
        .build()
        .map_err(|e| e.to_string())?;
    let zip = work.join("ffmpeg.zip");
    step.set("download");
    download(&client, &refused, &zip, &progress).await?;

    step.set("unpack");
    let out = work.join("out");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    unzip(&zip, &out).await?;
    let _ = std::fs::remove_file(&zip);

    step.set("pick_files");
    let unpacked = out.join(INNER);
    let fresh = work.join("fresh");
    std::fs::create_dir_all(&fresh).map_err(|e| e.to_string())?;
    for (from, to) in FILES {
        let source = from
            .split('/')
            .fold(unpacked.clone(), |p, part| p.join(part));
        if !source.is_file() {
            return Err(tr("error.ffmpeg_incomplete"));
        }
        std::fs::rename(&source, fresh.join(to)).map_err(|e| e.to_string())?;
    }

    step.set("replace");
    let target = mika_core::paths::ffmpeg_dir();
    match std::fs::remove_dir_all(&target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            log::warn!(target: "jobs", "the downloaded ffmpeg cannot be replaced, it is in use: {e}");
            return Err(tr("error.ffmpeg_in_use"));
        }
    }
    std::fs::rename(&fresh, &target).map_err(|e| e.to_string())?;
    progress(100);
    if installed() {
        Ok(())
    } else {
        Err(tr("error.ffmpeg_incomplete"))
    }
}

/// Follows at most five redirects, each to https on a GitHub host; anything
/// else stops the download and marks `refused`.
fn redirect_policy(refused: Arc<AtomicBool>) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > MAX_REDIRECTS || !allowed(attempt.url()) {
            refused.store(true, Ordering::Relaxed);
            attempt.error("redirect refused")
        } else {
            attempt.follow()
        }
    })
}

/// Streams the archive to `path`, hashing as it goes. The file is deleted on
/// a wrong size or sum.
async fn download(
    client: &reqwest::Client,
    refused: &AtomicBool,
    path: &Path,
    progress: &(impl Fn(u8) + Send + Sync),
) -> Result<(), String> {
    let failed = |e: reqwest::Error| {
        if refused.load(Ordering::Relaxed) {
            tr("error.ffmpeg_host")
        } else {
            tr("error.ffmpeg_download").replace("{why}", &e.without_url().to_string())
        }
    };
    let mut resp = client
        .get(URL)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(failed)?;
    if !allowed(resp.url()) {
        return Err(tr("error.ffmpeg_host"));
    }
    if resp.content_length().is_some_and(|n| n != SIZE) {
        return Err(tr("error.ffmpeg_size"));
    }
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut done = 0u64;
    let streamed: Result<(), String> = async {
        while let Some(chunk) = resp.chunk().await.map_err(failed)? {
            done += chunk.len() as u64;
            if done > SIZE {
                return Err(tr("error.ffmpeg_size"));
            }
            hash.update(&chunk);
            file.write_all(&chunk).map_err(|e| e.to_string())?;
            // The last share is left for unpacking.
            progress((done * 99 / SIZE) as u8);
        }
        file.flush().map_err(|e| e.to_string())?;
        if done != SIZE {
            return Err(tr("error.ffmpeg_size"));
        }
        let sum: String = hash
            .clone()
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if sum != SHA256 {
            return Err(tr("error.ffmpeg_checksum"));
        }
        Ok(())
    }
    .await;
    if streamed.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    streamed
}

/// Unpacks the three files with the `tar` of Windows, which refuses paths
/// that leave the target folder.
async fn unzip(zip: &Path, out: &Path) -> Result<(), String> {
    let windows = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let tar = PathBuf::from(windows).join("System32").join("tar.exe");
    let status = tokio::process::Command::new(tar)
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(out)
        .args(FILES.iter().map(|(from, _)| format!("{INNER}/{from}")))
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
        Err(tr("error.ffmpeg_unpack"))
    }
}

/// Deletes the downloaded copy. A copy the user pointed to or has in PATH lives
/// elsewhere and stays.
pub fn remove() -> Result<(), String> {
    if downloading() {
        return Err(tr("error.ffmpeg_busy"));
    }
    match std::fs::remove_dir_all(mika_core::paths::ffmpeg_dir()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            log::warn!(target: "jobs", "the downloaded ffmpeg cannot be deleted, it is in use: {e}");
            Err(tr("error.ffmpeg_in_use"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_on_github_hosts_is_followed() {
        let ok = |s: &str| allowed(&reqwest::Url::parse(s).unwrap());
        assert!(ok(URL));
        assert!(ok(
            "https://release-assets.githubusercontent.com/x/y.zip?sig=1"
        ));
        assert!(ok("https://objects.githubusercontent.com/x"));
        assert!(!ok("http://github.com/x"));
        assert!(!ok("https://github.com.evil.example/x"));
        assert!(!ok("https://evil.example/github.com"));
        assert!(!ok("https://gyan.dev/ffmpeg.zip"));
    }

    #[test]
    fn the_pinned_build_is_consistent() {
        assert!(URL.ends_with(&format!("/{INNER}.zip")));
        assert!(URL.contains("-lgpl-"));
        assert_eq!(SHA256.len(), 64);
        assert!(SHA256
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(SIZE.div_ceil(1024 * 1024) as u32, DOWNLOAD_MB);
    }

    #[test]
    fn the_work_folder_sits_next_to_the_target() {
        let target = mika_core::paths::ffmpeg_dir();
        assert_eq!(work_dir().parent(), target.parent());
        assert_ne!(work_dir(), target);
    }

    #[test]
    fn every_text_of_the_download_is_in_the_locale_file() {
        for key in [
            "error.ffmpeg_busy",
            "error.ffmpeg_download",
            "error.ffmpeg_host",
            "error.ffmpeg_size",
            "error.ffmpeg_checksum",
            "error.ffmpeg_unpack",
            "error.ffmpeg_incomplete",
            "error.ffmpeg_in_use",
            "error.av_file",
        ] {
            assert_ne!(tr(key), key, "{key} is missing in ru.json");
        }
    }

    /// Downloads the real archive into a temporary data folder.
    #[test]
    #[ignore = "downloads 161 MB"]
    fn live_ffmpeg_download() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIKA_DATA_DIR", dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        rt.block_on(install(|p| seen.lock().unwrap().push(p)))
            .unwrap();
        assert!(installed());
        assert_eq!(seen.lock().unwrap().last(), Some(&100));
        let target = mika_core::paths::ffmpeg_dir();
        for (_, name) in FILES {
            assert!(target.join(name).is_file(), "{name} is missing");
        }
        assert!(!work_dir().exists());
        let found = mika_tools::ffmpeg::locate_in("", None, &target)
            .paths
            .unwrap();
        assert_eq!(found.source, mika_tools::ffmpeg::FfmpegSource::Downloaded);
        let version = mika_tools::ffmpeg::version(&found).unwrap();
        assert!(version.version.starts_with("n9.0.2"), "{}", version.version);
        assert!(!version.gpl);
        println!("size {} MB", size() / 1_000_000);
        remove().unwrap();
        assert!(!installed());
    }
}
