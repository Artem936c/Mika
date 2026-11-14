//! Where a result goes, and the temporary files on the way.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mika_core::tools::ToolError;

use super::locate::path_key;
use crate::batch::free_name;
use crate::util::resolve_path;

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError::BadArguments(msg.into())
}

fn shown_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The file stem of `path`: `x` of `C:\v\x.mp4`.
pub(crate) fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "файл".into())
}

/// Lower-case extension without the dot, or "".
pub(crate) fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Path of the result: next to the input by default, a bare name in the
/// input's folder, anything else as a path. An existing file, an input or a
/// missing folder is refused; a default name is counted up instead.
pub(crate) fn resolve_output(
    input: &Path,
    output_arg: Option<&str>,
    default_name: &str,
    ext: &str,
    inputs: &[PathBuf],
    profile: &str,
) -> Result<PathBuf, ToolError> {
    let folder = input.parent().unwrap_or(Path::new("")).to_path_buf();
    let raw = output_arg.map(str::trim).filter(|s| !s.is_empty());
    let Some(raw) = raw else {
        return Ok(free_name(&folder.join(default_name)));
    };
    let raw = raw.trim_matches('"');
    let mut path = if raw.contains(['\\', '/']) || raw.contains(':') {
        resolve_path(raw, profile)
    } else {
        folder.join(raw)
    };
    // An existing folder: the result goes into it under the default name.
    if path.is_dir() {
        return Ok(free_name(&path.join(default_name)));
    }
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
        None => {
            let name = format!("{}.{ext}", shown_name(&path));
            path.set_file_name(name);
        }
        Some(e) if e == ext => {}
        Some(_) => return Err(bad(format!("нужно расширение .{ext}"))),
    }
    check_free(&path, inputs)?;
    Ok(path)
}

/// The result may not overwrite a file, be an input or lack its folder.
pub(crate) fn check_free(path: &Path, inputs: &[PathBuf]) -> Result<(), ToolError> {
    let key = path_key(path);
    if inputs.iter().any(|i| path_key(i) == key) {
        return Err(bad("результат нельзя записать на место исходного файла"));
    }
    if path.exists() {
        return Err(bad(format!(
            "Файл «{}» уже есть, назови другое имя",
            shown_name(path)
        )));
    }
    match path.parent() {
        Some(dir) if dir.is_dir() => Ok(()),
        _ => Err(bad(format!(
            "нет папки {}",
            path.parent().unwrap_or(Path::new("")).display()
        ))),
    }
}

/// Removal threads that have not ended yet.
static PENDING: AtomicUsize = AtomicUsize::new(0);

/// Temporary files and folders that still wait for removal.
pub(crate) fn pending_removals() -> usize {
    PENDING.load(Ordering::SeqCst)
}

/// Counts one removal as pending until dropped: also when its thread panics.
struct Pending;

impl Pending {
    fn new() -> Self {
        PENDING.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        PENDING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Removes a file or a folder on a separate thread, trying again while a
/// dying process still holds it.
pub(crate) fn remove_later(path: PathBuf, folder: bool) {
    let pending = Pending::new();
    std::thread::spawn(move || {
        let _pending = pending;
        for _ in 0..10 {
            let r = if folder {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match r {
                Ok(()) => return,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
        // The name of the file is the user's.
        log::warn!(
            "a half-written file or folder was not removed, something still holds it: folder={folder} path={}",
            mika_core::journal::content(&path.display().to_string())
        );
    });
}

/// The file FFmpeg writes: `<stem>.mika-part.<ext>` next to the result. It
/// takes the result's name only when the work is done; otherwise it goes away.
pub(crate) struct PartFile {
    path: PathBuf,
    kept: bool,
}

impl PartFile {
    pub(crate) fn new(result: &Path) -> Self {
        let dir = result.parent().unwrap_or(Path::new(""));
        let name = format!("{}.mika-part.{}", stem(result), ext_of(result));
        Self {
            path: free_name(&dir.join(name)),
            kept: false,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Gives the file the result's name, or the next free one.
    pub(crate) fn keep(mut self, result: &Path) -> Result<PathBuf, ToolError> {
        if !self.path.is_file() {
            return Err(ToolError::Failed("FFmpeg не записал файл".into()));
        }
        let to = free_name(result);
        std::fs::rename(&self.path, &to).map_err(|e| {
            ToolError::Failed(format!("не удалось назвать файл {}: {e}", to.display()))
        })?;
        self.kept = true;
        Ok(to)
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if !self.kept {
            remove_later(self.path.clone(), false);
        }
    }
}

/// A temporary folder `%TEMP%\mika-av-…` for one job.
pub(crate) struct WorkDir(Option<tempfile::TempDir>);

impl WorkDir {
    pub(crate) fn new() -> Result<Self, ToolError> {
        tempfile::Builder::new()
            .prefix("mika-av-")
            .tempdir()
            .map(|d| Self(Some(d)))
            .map_err(|e| ToolError::Failed(format!("не удалось создать временную папку: {e}")))
    }

    pub(crate) fn path(&self) -> &Path {
        self.0.as_ref().map(|d| d.path()).unwrap_or(Path::new(""))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            remove_later(dir.keep(), true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_goes_next_to_the_input_and_refuses_clashes() {
        let d = tempfile::tempdir().unwrap();
        let input = d.path().join("x.mp4");
        std::fs::write(&input, b"v").unwrap();
        let inputs = vec![input.clone()];
        let resolve = |out: Option<&str>| {
            resolve_output(
                &input,
                out,
                "x (сжато).mp4",
                "mp4",
                &inputs,
                "C:\\Users\\me",
            )
        };

        assert_eq!(resolve(None).unwrap(), d.path().join("x (сжато).mp4"));
        std::fs::write(d.path().join("x (сжато).mp4"), b"").unwrap();
        assert_eq!(
            resolve(None).unwrap(),
            d.path().join("x (сжато) (2).mp4"),
            "a default name counts up"
        );
        assert_eq!(resolve(Some("клип")).unwrap(), d.path().join("клип.mp4"));
        assert_eq!(
            resolve(Some("клип.MP4")).unwrap(),
            d.path().join("клип.MP4")
        );
        assert!(matches!(
            resolve(Some("клип.avi")),
            Err(ToolError::BadArguments(m)) if m == "нужно расширение .mp4"
        ));
        assert!(matches!(
            resolve(Some("x (сжато).mp4")),
            Err(ToolError::BadArguments(m)) if m.contains("уже есть")
        ));
        let same = input.display().to_string().to_uppercase();
        assert!(matches!(
            resolve(Some(&same)),
            Err(ToolError::BadArguments(m)) if m.contains("исходного")
        ));
        let elsewhere = d.path().join("нет").join("y.mp4");
        assert!(matches!(
            resolve(Some(&elsewhere.display().to_string())),
            Err(ToolError::BadArguments(m)) if m.starts_with("нет папки")
        ));
        let full = d.path().join("y.mp4");
        assert_eq!(resolve(Some(&full.display().to_string())).unwrap(), full);

        // A folder takes the result inside, under the default name.
        let ready = d.path().join("Готовое");
        std::fs::create_dir(&ready).unwrap();
        assert_eq!(
            resolve(Some(&ready.display().to_string())).unwrap(),
            ready.join("x (сжато).mp4")
        );
        assert_eq!(
            resolve(Some("Готовое")).unwrap(),
            ready.join("x (сжато).mp4")
        );
        std::fs::write(ready.join("x (сжато).mp4"), b"").unwrap();
        assert_eq!(
            resolve(Some(&ready.display().to_string())).unwrap(),
            ready.join("x (сжато) (2).mp4")
        );
    }

    #[test]
    fn part_file_is_renamed_when_kept_and_removed_otherwise() {
        let d = tempfile::tempdir().unwrap();
        let result = d.path().join("r.mp4");
        let part = PartFile::new(&result);
        assert_eq!(part.path(), d.path().join("r.mika-part.mp4"));
        std::fs::write(part.path(), b"data").unwrap();
        std::fs::write(&result, b"taken").unwrap();
        let kept = part.keep(&result).unwrap();
        assert_eq!(kept, d.path().join("r (2).mp4"));
        assert_eq!(std::fs::read(&kept).unwrap(), b"data");

        let dropped = PartFile::new(&d.path().join("s.mp4"));
        let path = dropped.path().to_path_buf();
        std::fs::write(&path, b"half").unwrap();
        drop(dropped);
        for _ in 0..50 {
            if !path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!path.exists());
    }

    #[test]
    fn work_dir_goes_away() {
        let w = WorkDir::new().unwrap();
        let p = w.path().to_path_buf();
        assert!(p.is_dir());
        assert!(p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("mika-av-"));
        drop(w);
        for _ in 0..50 {
            if !p.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!p.exists());
    }
}
