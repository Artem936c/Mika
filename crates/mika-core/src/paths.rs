//! Data folders.

use std::ffi::OsString;
use std::path::PathBuf;

pub fn data_dir_from(override_dir: Option<OsString>, appdata: Option<OsString>) -> PathBuf {
    if let Some(dir) = override_dir.filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    match appdata.filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("Mika"),
        None => PathBuf::from("Mika"),
    }
}

/// `%APPDATA%\Mika`, or `MIKA_DATA_DIR` when set.
pub fn data_dir() -> PathBuf {
    data_dir_from(
        std::env::var_os("MIKA_DATA_DIR"),
        std::env::var_os("APPDATA"),
    )
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

/// `%LOCALAPPDATA%\Mika\logs`, or `logs` inside `MIKA_DATA_DIR` when set.
pub fn logs_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MIKA_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("logs");
    }
    match std::env::var_os("LOCALAPPDATA").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("Mika").join("logs"),
        None => data_dir().join("logs"),
    }
}

/// Sound of phrases already synthesized: `%LOCALAPPDATA%\Mika\speech-cache`,
/// or inside `MIKA_DATA_DIR` when set. Machine-local, like the logs.
pub fn speech_cache_dir() -> PathBuf {
    match logs_dir().parent() {
        Some(parent) => parent.join("speech-cache"),
        None => data_dir().join("speech-cache"),
    }
}

/// Vosk for the wake phrase: `lib` with the library, `model` with the model.
/// Machine-local, next to the logs.
pub fn vosk_dir() -> PathBuf {
    match logs_dir().parent() {
        Some(parent) => parent.join("vosk"),
        None => data_dir().join("vosk"),
    }
}

pub fn database_path() -> PathBuf {
    data_dir().join("mika.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_prefers_override() {
        assert_eq!(
            data_dir_from(Some("D:\\x".into()), Some("C:\\A".into())),
            PathBuf::from("D:\\x")
        );
    }

    #[test]
    fn data_dir_uses_appdata_mika() {
        assert_eq!(
            data_dir_from(None, Some("C:\\A".into())),
            PathBuf::from("C:\\A").join("Mika")
        );
    }

    #[test]
    fn data_dir_falls_back_to_relative_mika() {
        assert_eq!(data_dir_from(None, None), PathBuf::from("Mika"));
    }
}
