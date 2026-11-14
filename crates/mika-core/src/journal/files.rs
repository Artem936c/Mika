//! Files of the journal: the current `mika.log`, the earlier `mika.1.log` …
//! `mika.N.log` (the larger the number, the older), rotation and retention.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The file being written.
pub const CURRENT: &str = "mika.log";
/// What the logger before the journal left behind; removed at the first start.
pub const LEGACY: &str = "mika.old.log";
/// Present while the app runs; see `Journal::begin_run`.
pub const MARKER: &str = "mika.running";

/// `mika.3.log` for 3; 0 is the current file.
pub fn file_name(index: u32) -> String {
    match index {
        0 => CURRENT.to_string(),
        n => format!("mika.{n}.log"),
    }
}

pub(crate) fn path(dir: &Path, index: u32) -> PathBuf {
    dir.join(file_name(index))
}

/// Where the current file waits between the two renames of a rotation.
fn parked(dir: &Path) -> PathBuf {
    dir.join("mika.0.log")
}

/// The number in `mika.<n>.log`; `mika.0.log` is the parked file.
fn index_of(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("mika.")?.strip_suffix(".log")?;
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return None;
    }
    digits.parse().ok()
}

/// A name the journal uses for a file of its own. The letter case does not
/// count: for Windows `Mika.log` is `mika.log`.
pub(crate) fn reserved(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == CURRENT || name == LEGACY || name == MARKER || index_of(&name).is_some()
}

/// Whether the current file is where it is expected to be.
pub(crate) fn has_current(dir: &Path) -> bool {
    dir.join(CURRENT).exists()
}

/// How far into the current file its first line is looked for.
const FIRST_LINE_WITHIN: u64 = 64 * 1024;

/// Removes a current file that the logger before the journal wrote: its
/// first line starts with `[` and is not a record. That logger wrote
/// recognised speech, and the journal can neither show nor export its lines,
/// so they would only lie in the folder unseen.
pub(crate) fn remove_old_logger_file(dir: &Path) {
    let path = dir.join(CURRENT);
    let old = File::open(&path).is_ok_and(|file| {
        let mut first = Vec::new();
        let read = io::BufReader::new(file.take(FIRST_LINE_WITHIN)).read_until(b'\n', &mut first);
        read.is_ok()
            && first.starts_with(b"[")
            && first.ends_with(b"\n")
            && std::str::from_utf8(&first[..first.len() - 1])
                .map_or(true, |line| super::line::split_line(line).is_none())
    });
    if old {
        let _ = std::fs::remove_file(&path);
    }
}

/// Earlier files that exist, the newest (`mika.1.log`) first.
pub(crate) fn earlier(dir: &Path) -> Vec<(u32, PathBuf)> {
    let mut found: Vec<(u32, PathBuf)> = numbered(dir)
        .into_iter()
        .filter(|(index, _)| *index > 0)
        .collect();
    found.sort_unstable_by_key(|(index, _)| *index);
    found
}

fn numbered(dir: &Path) -> Vec<(u32, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let index = index_of(entry.file_name().to_str()?)?;
            Some((index, entry.path()))
        })
        .collect()
}

/// Every file with records: the current one, the earlier ones and what a
/// failed rotation or the old logger may have left.
pub(crate) fn family(dir: &Path) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = numbered(dir).into_iter().map(|(_, path)| path).collect();
    for name in [CURRENT, LEGACY] {
        let path = dir.join(name);
        if path.exists() {
            all.push(path);
        }
    }
    all
}

/// Opens the current file for appending and says how long it is. A file whose
/// last line was never finished (a crash in the middle of a write) gets a line
/// feed first, so the next record starts a line of its own.
pub(crate) fn open_current(dir: &Path) -> io::Result<(File, u64)> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(CURRENT);
    let torn = match File::open(&path) {
        Ok(mut file) => {
            let mut last = [0u8; 1];
            file.seek(SeekFrom::End(-1)).is_ok()
                && file.read_exact(&mut last).is_ok()
                && last[0] != b'\n'
        }
        Err(_) => false,
    };
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    if torn {
        file.write_all(b"\n")?;
    }
    let size = file.metadata()?.len();
    Ok((file, size))
}

/// Turns the current file into `mika.1.log` and moves the earlier ones up by
/// one. The current file must be closed. `false` when the current file could
/// not be moved: then nothing has changed.
///
/// The current file is moved aside first. It is the one most likely to be
/// held by somebody, and when it cannot be moved the earlier files must stay
/// where they are, or every attempt would push them one step further out.
///
/// No step replaces a file that is there. An earlier file may be held by
/// another program (a viewer, a spreadsheet) so that it can be neither moved
/// nor replaced; then the files moved so far go back, the current file goes
/// back too, and the answer is `false`: the caller tries again later and no
/// record is lost.
pub(crate) fn rotate(dir: &Path) -> bool {
    if !recover(dir) {
        return false;
    }
    let parked = parked(dir);
    if move_to_free(&dir.join(CURRENT), &parked).is_err() {
        return false;
    }
    if move_up(dir) && move_to_free(&parked, &path(dir, 1)).is_ok() {
        return true;
    }
    // Back where it was. When even that fails the file stays moved aside,
    // and `recover` finishes the rotation later.
    let _ = move_to_free(&parked, &dir.join(CURRENT));
    false
}

/// Renames a file to a name that nobody has. `std::fs::rename` alone would
/// replace a file that is there.
fn move_to_free(from: &Path, to: &Path) -> io::Result<()> {
    if to.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the name is taken",
        ));
    }
    std::fs::rename(from, to)
}

/// Moves every earlier file up by one, the oldest first. `false` when one of
/// them cannot be moved: then those moved before it are back in their places.
fn move_up(dir: &Path) -> bool {
    let mut indexes: Vec<u32> = earlier(dir).into_iter().map(|(index, _)| index).collect();
    indexes.reverse();
    let mut moved = Vec::new();
    for index in indexes {
        if move_to_free(&path(dir, index), &path(dir, index + 1)).is_err() {
            for index in moved.into_iter().rev() {
                let _ = move_to_free(&path(dir, index + 1), &path(dir, index));
            }
            return false;
        }
        moved.push(index);
    }
    true
}

/// Finishes a rotation that stopped half way (a crash, a file held by
/// somebody): the file moved aside becomes `mika.1.log`. `false` when it is
/// still moved aside afterwards.
pub(crate) fn recover(dir: &Path) -> bool {
    let parked = parked(dir);
    if !parked.exists() {
        return true;
    }
    // The place is taken when the rotation stopped before the others moved.
    if path(dir, 1).exists() && !move_up(dir) {
        return false;
    }
    move_to_free(&parked, &path(dir, 1)).is_ok()
}

/// Removes earlier files beyond `keep_files` and those last written more than
/// `keep_days` days before `now`. With `stale_current` the current file goes
/// too when its last record is that old: nothing in it is worth keeping.
pub(crate) fn prune(
    dir: &Path,
    keep_files: u32,
    keep_days: u32,
    now: SystemTime,
    stale_current: bool,
) {
    let limit = Duration::from_secs(u64::from(keep_days) * 24 * 60 * 60);
    let old = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|written| now.duration_since(written).ok())
            .is_some_and(|age| age > limit)
    };
    for (index, path) in earlier(dir) {
        if index > keep_files || old(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
    if stale_current {
        let current = dir.join(CURRENT);
        if old(&current) {
            let _ = std::fs::remove_file(&current);
        }
    }
}

/// Removes every file with records. The first error, if any; the rest are
/// removed all the same.
pub(crate) fn remove_all(dir: &Path) -> io::Result<()> {
    let mut first = Ok(());
    for path in family(dir) {
        let removed = match std::fs::remove_file(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            // A file somebody holds open can still be emptied.
            Err(_) => OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .map(drop),
            Ok(()) => Ok(()),
        };
        if first.is_ok() {
            first = removed;
        }
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn file_names_follow_the_plan() {
        assert_eq!(file_name(0), "mika.log");
        assert_eq!(file_name(1), "mika.1.log");
        assert_eq!(file_name(12), "mika.12.log");
        assert_eq!(index_of("mika.1.log"), Some(1));
        assert_eq!(index_of("mika.12.log"), Some(12));
        assert_eq!(index_of("mika.0.log"), Some(0));
        for other in [
            "mika.log",
            "mika.old.log",
            "mika.01.log",
            "mika.-1.log",
            "mika.1.log.bak",
            "mika..log",
            "mika.1234567.log",
            "other.1.log",
            "mika.running",
        ] {
            assert_eq!(index_of(other), None, "{other}");
        }
    }

    #[test]
    fn rotation_moves_every_file_up_by_one() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("mika.log"), "current").unwrap();
        std::fs::write(d.path().join("mika.1.log"), "one").unwrap();
        std::fs::write(d.path().join("mika.2.log"), "two").unwrap();
        std::fs::write(d.path().join("notes.txt"), "not ours").unwrap();
        assert!(rotate(d.path()));
        assert_eq!(
            names(d.path()),
            ["mika.1.log", "mika.2.log", "mika.3.log", "notes.txt"]
        );
        let read = |n: &str| std::fs::read_to_string(d.path().join(n)).unwrap();
        assert_eq!(read("mika.1.log"), "current");
        assert_eq!(read("mika.2.log"), "one");
        assert_eq!(read("mika.3.log"), "two");
        assert_eq!(
            earlier(d.path())
                .iter()
                .map(|(i, _)| *i)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn a_rotation_that_stopped_half_way_is_finished() {
        let read = |d: &Path, n: &str| std::fs::read_to_string(d.join(n)).unwrap();
        // Stopped before the earlier files moved.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("mika.0.log"), "moved aside").unwrap();
        std::fs::write(d.path().join("mika.1.log"), "one").unwrap();
        std::fs::write(d.path().join("mika.2.log"), "two").unwrap();
        assert!(recover(d.path()));
        assert_eq!(names(d.path()), ["mika.1.log", "mika.2.log", "mika.3.log"]);
        assert_eq!(read(d.path(), "mika.1.log"), "moved aside");
        assert_eq!(read(d.path(), "mika.3.log"), "two");
        // Stopped after they moved: only the last step is left.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("mika.0.log"), "moved aside").unwrap();
        std::fs::write(d.path().join("mika.2.log"), "one").unwrap();
        recover(d.path());
        assert_eq!(names(d.path()), ["mika.1.log", "mika.2.log"]);
        assert_eq!(read(d.path(), "mika.2.log"), "one");
        // Nothing to finish: nothing changes. The next rotation finishes it too.
        recover(d.path());
        assert_eq!(names(d.path()), ["mika.1.log", "mika.2.log"]);
        std::fs::write(d.path().join("mika.0.log"), "left again").unwrap();
        std::fs::remove_file(d.path().join("mika.1.log")).unwrap();
        std::fs::write(d.path().join("mika.log"), "current").unwrap();
        assert!(rotate(d.path()));
        assert_eq!(names(d.path()), ["mika.1.log", "mika.2.log", "mika.3.log"]);
        assert_eq!(read(d.path(), "mika.1.log"), "current");
        assert_eq!(read(d.path(), "mika.2.log"), "left again");
        assert_eq!(read(d.path(), "mika.3.log"), "one");
    }

    /// Opens a file the way a viewer that locks it does: others may read and
    /// write it, but not rename or remove it.
    #[cfg(windows)]
    fn hold(path: &Path) -> File {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(path)
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn a_rotation_never_replaces_a_file_somebody_holds() {
        let read = |d: &Path, n: &str| std::fs::read_to_string(d.join(n)).unwrap();
        let d = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("mika.log", "current"),
            ("mika.1.log", "one"),
            ("mika.2.log", "two"),
            ("mika.3.log", "three"),
        ] {
            std::fs::write(d.path().join(name), text).unwrap();
        }
        let held = hold(&d.path().join("mika.2.log"));
        assert!(!rotate(d.path()));
        // Nothing moved and nothing was replaced.
        assert_eq!(
            names(d.path()),
            ["mika.1.log", "mika.2.log", "mika.3.log", "mika.log"]
        );
        assert_eq!(read(d.path(), "mika.log"), "current");
        assert_eq!(read(d.path(), "mika.1.log"), "one");
        assert_eq!(read(d.path(), "mika.2.log"), "two");
        assert_eq!(read(d.path(), "mika.3.log"), "three");
        // Once the file is let go the rotation goes through.
        drop(held);
        assert!(rotate(d.path()));
        assert_eq!(
            names(d.path()),
            ["mika.1.log", "mika.2.log", "mika.3.log", "mika.4.log"]
        );
        assert_eq!(read(d.path(), "mika.1.log"), "current");
        assert_eq!(read(d.path(), "mika.4.log"), "three");
    }

    #[cfg(windows)]
    #[test]
    fn a_file_moved_aside_is_not_replaced_by_the_next_rotation() {
        let read = |d: &Path, n: &str| std::fs::read_to_string(d.join(n)).unwrap();
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("mika.0.log"), "moved aside").unwrap();
        std::fs::write(d.path().join("mika.1.log"), "one").unwrap();
        std::fs::write(d.path().join("mika.log"), "current").unwrap();
        let held = hold(&d.path().join("mika.1.log"));
        assert!(!recover(d.path()));
        assert!(!rotate(d.path()));
        assert_eq!(read(d.path(), "mika.0.log"), "moved aside");
        assert_eq!(read(d.path(), "mika.1.log"), "one");
        assert_eq!(read(d.path(), "mika.log"), "current");
        drop(held);
        assert!(rotate(d.path()));
        assert_eq!(read(d.path(), "mika.1.log"), "current");
        assert_eq!(read(d.path(), "mika.2.log"), "moved aside");
        assert_eq!(read(d.path(), "mika.3.log"), "one");
    }

    #[test]
    fn the_current_file_of_the_old_logger_is_told_by_its_first_line() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("mika.log");
        // The old logger's file goes, whatever was added to it later.
        std::fs::write(
            &path,
            "[2026-11-10T12:00:00Z INFO  mika_gui] wake phrase heard: hello\n2026-11-14T10:00:00.000+03:00\tINFO\tapp\tours\n",
        )
        .unwrap();
        remove_old_logger_file(d.path());
        assert!(!path.exists());
        // A journal file stays, also one whose first line was torn, and a
        // file without a whole line.
        for kept in [
            &b"2026-11-14T10:00:00.000+03:00\tINFO\tapp\tours\n[not ours] later\n"[..],
            b"2026-11-14T10:00:0\n",
            b"[2026-11-10T12:00:00Z INFO  mika_gui] no line feed yet",
            b"",
        ] {
            std::fs::write(&path, kept).unwrap();
            remove_old_logger_file(d.path());
            assert!(path.exists(), "{:?}", String::from_utf8_lossy(kept));
        }
        // No file: nothing to do.
        std::fs::remove_file(&path).unwrap();
        remove_old_logger_file(d.path());
    }

    #[test]
    fn a_name_of_the_journal_is_its_own_in_any_letter_case() {
        for name in [
            "mika.log",
            "Mika.log",
            "MIKA.1.LOG",
            "mika.Running",
            "MIKA.OLD.LOG",
        ] {
            assert!(reserved(name), "{name}");
        }
        for name in ["mika.txt", "mika-journal-2026-11-15.txt", "notes.log"] {
            assert!(!reserved(name), "{name}");
        }
    }

    #[test]
    fn rotation_without_a_current_file_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("mika.1.log"), "one").unwrap();
        assert!(!rotate(d.path()));
        assert_eq!(names(d.path()), ["mika.1.log"]);
    }

    #[test]
    fn retention_by_count_and_by_age() {
        let d = tempfile::tempdir().unwrap();
        for n in 1..=6 {
            std::fs::write(path(d.path(), n), "x").unwrap();
        }
        std::fs::write(d.path().join("mika.log"), "current").unwrap();
        let now = SystemTime::now();
        prune(d.path(), 4, 14, now, true);
        assert_eq!(
            names(d.path()),
            [
                "mika.1.log",
                "mika.2.log",
                "mika.3.log",
                "mika.4.log",
                "mika.log"
            ]
        );
        // Fifteen days later every earlier file is too old; the current one
        // stays unless asked.
        let later = now + Duration::from_secs(15 * 24 * 60 * 60);
        prune(d.path(), 4, 14, later, false);
        assert_eq!(names(d.path()), ["mika.log"]);
        // Thirteen days: nothing is old yet.
        std::fs::write(path(d.path(), 1), "x").unwrap();
        let sooner = SystemTime::now() + Duration::from_secs(13 * 24 * 60 * 60);
        prune(d.path(), 4, 14, sooner, true);
        assert_eq!(names(d.path()), ["mika.1.log", "mika.log"]);
        prune(d.path(), 4, 14, later + Duration::from_secs(60), true);
        assert!(names(d.path()).is_empty());
    }

    #[test]
    fn an_unfinished_last_line_gets_its_line_feed() {
        let d = tempfile::tempdir().unwrap();
        let (_, size) = open_current(d.path()).unwrap();
        assert_eq!(size, 0);
        std::fs::write(d.path().join("mika.log"), "whole\nhalf").unwrap();
        let (mut file, size) = open_current(d.path()).unwrap();
        assert_eq!(size, 11);
        file.write_all(b"next\n").unwrap();
        drop(file);
        assert_eq!(
            std::fs::read_to_string(d.path().join("mika.log")).unwrap(),
            "whole\nhalf\nnext\n"
        );
        // A whole file is left as it is.
        let (_, size) = open_current(d.path()).unwrap();
        assert_eq!(size, 16);
    }

    #[test]
    fn removing_all_leaves_other_files() {
        let d = tempfile::tempdir().unwrap();
        for name in [
            "mika.log",
            "mika.1.log",
            "mika.7.log",
            "mika.0.log",
            "mika.old.log",
            "mika.running",
            "notes.txt",
        ] {
            std::fs::write(d.path().join(name), "x").unwrap();
        }
        remove_all(d.path()).unwrap();
        assert_eq!(names(d.path()), ["mika.running", "notes.txt"]);
    }
}
