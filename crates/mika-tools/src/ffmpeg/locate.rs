//! Where FFmpeg is: the path from the settings, then `PATH`, then the copy
//! Mika downloaded. Only file checks here, no process.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{FfmpegPaths, FfmpegSource, FfmpegStatus, FfmpegVersion};
use crate::sys::Runner;

const FFMPEG: &str = "ffmpeg.exe";
const FFPROBE: &str = "ffprobe.exe";

/// What the search finds now, with the real `PATH` and download folder.
pub fn status(setting: &str) -> FfmpegStatus {
    locate_in(
        setting,
        std::env::var_os("PATH").as_deref(),
        &mika_core::paths::ffmpeg_dir(),
    )
}

/// FFmpeg to use now, if any.
pub fn locate(setting: &str) -> Option<FfmpegPaths> {
    status(setting).paths
}

/// The search with everything given: for tests.
pub fn locate_in(setting: &str, path_var: Option<&OsStr>, local_dir: &Path) -> FfmpegStatus {
    let setting = setting.trim().trim_matches('"').trim();
    if !setting.is_empty() {
        let profile = std::env::var("USERPROFILE").unwrap_or_default();
        let given = crate::util::resolve_path(setting, &profile);
        if let Some(paths) = from_setting(&given) {
            return FfmpegStatus {
                paths: Some(paths),
                setting_invalid: false,
            };
        }
    }
    let from_path = path_var.and_then(|var| {
        std::env::split_paths(var)
            .filter(|dir| dir.is_absolute())
            .find_map(|dir| pair_in(&dir, FfmpegSource::Path))
    });
    FfmpegStatus {
        paths: from_path.or_else(|| pair_in(local_dir, FfmpegSource::Downloaded)),
        setting_invalid: !setting.is_empty(),
    }
}

/// `ffmpeg.exe` itself, or a folder with it, or with `bin\ffmpeg.exe`.
fn from_setting(given: &Path) -> Option<FfmpegPaths> {
    if given.is_file() {
        let named = given
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(FFMPEG));
        return named
            .then(|| given.parent())
            .flatten()
            .and_then(|dir| pair_in(dir, FfmpegSource::Settings));
    }
    if given.is_dir() {
        return pair_in(given, FfmpegSource::Settings)
            .or_else(|| pair_in(&given.join("bin"), FfmpegSource::Settings));
    }
    None
}

/// Both programs in one folder.
fn pair_in(dir: &Path, source: FfmpegSource) -> Option<FfmpegPaths> {
    let ffmpeg = dir.join(FFMPEG);
    let ffprobe = dir.join(FFPROBE);
    (ffmpeg.is_file() && ffprobe.is_file()).then_some(FfmpegPaths {
        ffmpeg,
        ffprobe,
        source,
    })
}

/// Version and licence of the build: runs `ffmpeg -version`, up to 10 s.
pub fn version(paths: &FfmpegPaths) -> Result<FfmpegVersion, String> {
    let out = crate::win::ProcessRunner.run_blocking(
        &paths.ffmpeg.to_string_lossy(),
        &["-hide_banner".into(), "-version".into()],
        Duration::from_secs(10),
    )?;
    if out.timed_out {
        return Err("FFmpeg не ответил за 10 секунд".into());
    }
    parse_version(&out.stdout).ok_or_else(|| "FFmpeg не назвал свою версию".into())
}

/// `ffmpeg version n9.0.2-… Copyright …` and the `configuration:` line.
pub(crate) fn parse_version(text: &str) -> Option<FfmpegVersion> {
    let first = text.lines().find(|l| !l.trim().is_empty())?;
    let version = first
        .trim()
        .strip_prefix("ffmpeg version ")?
        .split_whitespace()
        .next()?
        .to_string();
    let gpl = text
        .lines()
        .any(|l| l.trim_start().starts_with("configuration:") && l.contains("--enable-gpl"));
    Some(FfmpegVersion { version, gpl })
}

/// Path key for comparing: back slashes, lower case.
pub(crate) fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// A copy of `path` for keys of caches: the file with its size and time.
pub(crate) fn file_key(path: &Path) -> Option<(String, u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((path_key(path), meta.len(), meta.modified().ok()?))
}

/// The folder of a file, for a check that it exists.
pub(crate) fn folder_of(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(FFMPEG), b"").unwrap();
        std::fs::write(dir.join(FFPROBE), b"").unwrap();
    }

    #[test]
    fn search_goes_setting_then_path_then_download() {
        let d = tempfile::tempdir().unwrap();
        let (own, on_path, local) = (
            d.path().join("own"),
            d.path().join("onpath"),
            d.path().join("local"),
        );
        pair(&own.join("bin"));
        pair(&on_path);
        pair(&local);
        let var = std::env::join_paths([PathBuf::from("relative"), on_path.clone()]).unwrap();

        let s = locate_in(&own.to_string_lossy(), Some(&var), &local);
        assert_eq!(s.paths.as_ref().unwrap().source, FfmpegSource::Settings);
        assert_eq!(s.paths.unwrap().ffmpeg, own.join("bin").join(FFMPEG));
        assert!(!s.setting_invalid);

        let exe = own.join("bin").join(FFMPEG);
        let s = locate_in(&format!("\"{}\"", exe.display()), None, &local);
        assert_eq!(s.paths.unwrap().source, FfmpegSource::Settings);

        let s = locate_in("", Some(&var), &local);
        assert_eq!(s.paths.as_ref().unwrap().source, FfmpegSource::Path);
        assert_eq!(s.paths.unwrap().ffprobe, on_path.join(FFPROBE));

        let s = locate_in(&d.path().join("nothing").to_string_lossy(), None, &local);
        assert_eq!(s.paths.unwrap().source, FfmpegSource::Downloaded);
        assert!(s.setting_invalid);

        assert_eq!(
            locate_in("", None, &d.path().join("none")),
            FfmpegStatus {
                paths: None,
                setting_invalid: false
            }
        );
    }

    #[test]
    fn a_folder_without_ffprobe_does_not_count() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(FFMPEG), b"").unwrap();
        let var = d.path().as_os_str().to_owned();
        let s = locate_in(&d.path().to_string_lossy(), Some(&var), &d.path().join("x"));
        assert_eq!(s.paths, None);
        assert!(s.setting_invalid);
        let other = d.path().join("ffmpeg.txt");
        std::fs::write(&other, b"").unwrap();
        assert!(from_setting(&other).is_none(), "only ffmpeg.exe is taken");
    }

    #[test]
    fn version_and_licence_come_from_the_banner() {
        let text = "ffmpeg version n9.0.2-39-g7adfbe40dc-20261031 Copyright (c) 2000-2026 the FFmpeg developers\n\
built with gcc 15.2.0 (crosstool-NG 1.28.0.1_403899e)\n\
configuration: --prefix=/ffbuild/prefix --enable-gpl --enable-version3\n\
libavutil      60.  8.100 / 60.  8.100\n";
        assert_eq!(
            parse_version(text),
            Some(FfmpegVersion {
                version: "n9.0.2-39-g7adfbe40dc-20261031".into(),
                gpl: true
            })
        );
        let lgpl = text.replace("--enable-gpl ", "");
        assert!(!parse_version(&lgpl).unwrap().gpl);
        assert_eq!(parse_version("hello"), None);
    }
}
