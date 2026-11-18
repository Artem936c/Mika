//! Files of sherpa-onnx for the wake phrase (decision 0048): downloaded on
//! the user's request, checked against fixed SHA-256 sums, unpacked next to
//! the logs. The archives are `.tar.bz2`, unpacked here in Rust: the `tar`
//! of older Windows 10 does not read bzip2.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mika_audio::wake::WakeEngine;

use crate::vosk::Part;

/// Libraries of sherpa-onnx 2.0.1 for Windows x64 (Apache 2.0) with
/// onnxruntime (MIT), about 8 MB.
const LIBRARY: Part = Part {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-win-x64-shared-MT-Release-lib.tar.bz2",
    sha256: "afa4de3ca4ab0afd53bb4aab40d28ee15608364b549ffc0fe0caf6489bfc39a5",
    inner: "sherpa-onnx-v2.0.1-win-x64-shared-MT-Release-lib/lib",
    target: "lib",
};

/// Small streaming Russian model of Vosk's authors for sherpa-onnx
/// (Apache 2.0), about 24 MB.
const MODEL: Part = Part {
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16.tar.bz2",
    sha256: "6ba68a01ff3c5445aaf2d61e9b97b026f1149dcc9049d11af3f44f55176341d8",
    inner: "sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16",
    target: "model",
};

/// Files a build needs, or examples, not the running app: they are not kept.
const NOT_NEEDED: [&str; 6] = [
    "onnxruntime.lib",
    "sherpa-onnx-c-api.lib",
    "sherpa-onnx-cxx-api.lib",
    "sherpa-onnx-cxx-api.dll",
    "test_wavs",
    "README.md",
];

/// Set while a download runs, so a second press does nothing.
static BUSY: AtomicBool = AtomicBool::new(false);

fn library_dir() -> PathBuf {
    mika_core::paths::sherpa_dir().join(LIBRARY.target)
}

fn model_dir() -> PathBuf {
    mika_core::paths::sherpa_dir().join(MODEL.target)
}

/// True when both the libraries and the model are in place.
pub fn installed() -> bool {
    library_dir().join(mika_audio::sherpa::LIBRARY).is_file()
        && library_dir().join("onnxruntime.dll").is_file()
        && mika_audio::sherpa::model_complete(&model_dir())
}

/// sherpa-onnx when its files are in place.
pub fn engine() -> Option<WakeEngine> {
    installed().then(|| WakeEngine::Sherpa {
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
    walk(&mika_core::paths::sherpa_dir())
}

/// Downloads both archives, checks them and unpacks. `progress` gets the
/// share done, 0..=100. Does nothing while another download runs.
pub async fn install(progress: impl Fn(u8)) -> Result<(), String> {
    if BUSY.swap(true, Ordering::Relaxed) {
        return Err(crate::strings::tr("error.vosk_busy"));
    }
    let began = std::time::Instant::now();
    log::info!("sherpa download started");
    let result = install_parts(&progress).await;
    match &result {
        Ok(()) => log::info!(
            "sherpa downloaded and unpacked: ms={} bytes={}",
            began.elapsed().as_millis(),
            size()
        ),
        Err(e) => log::error!(
            "sherpa not downloaded: ms={} error={}",
            began.elapsed().as_millis(),
            mika_core::journal::error_text(e)
        ),
    }
    BUSY.store(false, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(mika_core::paths::sherpa_dir().join(".download"));
    result
}

async fn install_parts(progress: &impl Fn(u8)) -> Result<(), String> {
    let root = mika_core::paths::sherpa_dir();
    let work = root.join(".download");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    // The model is three times the libraries: shares of the bar follow that.
    let parts = [(&LIBRARY, 0u8, 25u8), (&MODEL, 25, 75)];
    for (part, from, span) in parts {
        let archive = work.join(format!("{}.tar.bz2", part.target));
        crate::vosk::download(&client, part, &archive, |share| {
            progress(from + (share as u16 * span as u16 / 100) as u8)
        })
        .await?;
        let out = work.join(part.target);
        let unpack_to = out.clone();
        tauri::async_runtime::spawn_blocking(move || untar_bz2(&archive, &unpack_to))
            .await
            .map_err(|e| e.to_string())??;
        let unpacked = out.join(part.inner);
        for extra in NOT_NEEDED {
            let path = unpacked.join(extra);
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_dir_all(&path);
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

/// Unpacks a `.tar.bz2` into `out`. Entries that would leave `out` are
/// refused by `tar` itself.
fn untar_bz2(archive: &Path, out: &Path) -> Result<(), String> {
    let unpack_failed = |e: std::io::Error| {
        log::warn!("sherpa archive did not unpack: {e}");
        crate::strings::tr("error.vosk_unpack")
    };
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(std::io::BufReader::new(file)));
    tar.unpack(out).map_err(unpack_failed)
}

/// Deletes the downloaded files.
pub fn remove() -> Result<(), String> {
    if downloading() {
        return Err(crate::strings::tr("error.vosk_busy"));
    }
    match std::fs::remove_dir_all(mika_core::paths::sherpa_dir()) {
        Ok(()) => {
            log::info!("sherpa removed");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            log::warn!("sherpa not removed: {e}");
            Err(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_tar_bz2_unpacks_into_the_folder() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.bz2");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let bz = bzip2::write::BzEncoder::new(file, bzip2::Compression::fast());
            let mut tar = tar::Builder::new(bz);
            let data = b"hello";
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "pack/lib/x.txt", &data[..])
                .unwrap();
            tar.into_inner().unwrap().finish().unwrap().flush().unwrap();
        }
        let out = dir.path().join("out");
        super::untar_bz2(&archive, &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("pack/lib/x.txt")).unwrap(),
            "hello"
        );
    }

    /// Downloads the real archives into a temporary data folder.
    #[test]
    #[ignore = "downloads about 32 MB"]
    fn install_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("MIKA_DATA_DIR", dir.path());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(super::install(|_| {})).unwrap();
        assert!(super::installed());
        assert!(!super::library_dir().join("sherpa-onnx-c-api.lib").exists());
        assert!(!super::model_dir().join("test_wavs").exists());
        println!("size {} MB", super::size() / 1_000_000);
        super::remove().unwrap();
        assert!(!super::installed());
    }
}
