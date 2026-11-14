//! The sink: takes records from the `log` macros, scrubs them, keeps the last
//! ones in memory and writes all of them to the files.
//!
//! Three rules hold it together.
//!
//! No call into the sink waits on anything but its own short lock. The text of
//! a record is formatted and scrubbed before the lock is taken; under the lock
//! there is only a buffer, a file and the memory ring. A `log` call made by
//! the thread that is already inside the sink is dropped, so nothing recurses.
//!
//! Disk errors are swallowed. A failed write drops what was buffered, and the
//! file is opened again a few seconds later; the records stay in memory.
//!
//! Readers never stop writers. A reader holds the `files` lock, which only
//! keeps a rotation away; a writer that needs to rotate and finds the lock
//! taken writes on and rotates later.

use std::cell::Cell;
use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};

use chrono::{Local, SecondsFormat};
use parking_lot::{Condvar, Mutex, MutexGuard};

use super::category::{category_of, APP, CATEGORIES, LIB};
use super::line::{format_line, Level, Row};
use super::{files, read, scrub, JournalError};
use crate::config::LogConfig;

/// Size at which the current file becomes `mika.1.log`.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// How many of the last records are held in memory.
pub const RING_RECORDS: usize = 2000;

/// Records below `WARN` wait in the buffer no longer than this. What waits
/// is lost when the process is killed or crashes in native code, so the wait
/// is short: the last thing done before such an end is what the journal is
/// read for.
const FLUSH_DELAY: Duration = Duration::from_millis(100);
/// The buffer is written out when it reaches this size.
const BUFFER_BYTES: usize = 64 * 1024;
/// After a failed write or open the file is tried again this much later.
const REOPEN_DELAY: Duration = Duration::from_secs(5);
/// After a failed rotation the next one is tried this much later.
const ROTATE_RETRY: Duration = Duration::from_secs(30);
/// Files past their age are looked for this often while the app runs.
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);
/// How long the panic hook waits for the sink before it gives up.
const PANIC_WAIT: Duration = Duration::from_secs(1);

/// How a journal is opened.
#[derive(Debug, Clone)]
pub struct Options {
    /// Folder of the files. `None` keeps records in memory only.
    pub dir: Option<PathBuf>,
    /// Also print every record to stderr.
    pub stderr: bool,
    pub config: LogConfig,
    /// Rotation size; `MAX_FILE_BYTES` outside tests.
    pub max_file_bytes: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dir: None,
            stderr: false,
            config: LogConfig::default(),
            max_file_bytes: MAX_FILE_BYTES,
        }
    }
}

impl Options {
    /// Files in `dir`, the settings from `config`, nothing on stderr.
    pub fn in_dir(dir: impl Into<PathBuf>, config: LogConfig) -> Self {
        Self {
            dir: Some(dir.into()),
            config,
            ..Self::default()
        }
    }
}

/// Records held in memory: every one written since `after`, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct Tail {
    pub rows: Vec<Row>,
    /// Number of the last record written by this run; 0 before the first.
    pub last: u64,
}

/// What the files take on disk.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct Summary {
    pub size_bytes: u64,
    pub files: u32,
    /// Time of the oldest record, as written; `None` for an empty journal.
    pub oldest: Option<String>,
}

thread_local! {
    /// Above zero while this thread is inside the sink.
    static INSIDE: Cell<u32> = const { Cell::new(0) };
}

/// Marks the thread as being inside the sink until dropped.
pub(super) struct Inside(bool);

impl Inside {
    pub(super) fn enter() -> Inside {
        // No thread-local storage while a thread is being torn down: then
        // nothing is marked, and nothing is unmarked.
        Inside(INSIDE.try_with(|depth| depth.set(depth.get() + 1)).is_ok())
    }

    fn already() -> bool {
        INSIDE.try_with(|depth| depth.get() > 0).unwrap_or(false)
    }
}

impl Drop for Inside {
    fn drop(&mut self) {
        if self.0 {
            let _ = INSIDE.try_with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }
}

/// A record in memory and where its line starts: file generation and offset.
struct Entry {
    seq: u64,
    row: Row,
    at: Option<(u64, u64)>,
}

/// Where things are at one moment, for a reader.
pub(super) struct Settled {
    /// Generation of the current file.
    pub(super) generation: u64,
    /// Length of the current file as written by now; `None` while it is not
    /// open.
    pub(super) size: Option<u64>,
    /// Number of the last record.
    pub(super) seq: u64,
    /// Places of the records in memory: generation, offset, number.
    pub(super) places: Vec<(u64, u64, u64)>,
    /// Records in memory that are in no file, the newest first: the file
    /// could not be written, or somebody removed it.
    pub(super) unplaced: Vec<Row>,
}

pub(super) struct State {
    pub(super) config: LogConfig,
    /// Folder of the files, as in the journal.
    dir: Option<PathBuf>,
    file: Option<File>,
    /// Length of the current file, the buffer included.
    size: u64,
    buf: Vec<u8>,
    /// Counts rotations: the current file is `generation`, `mika.K.log` is
    /// `generation - K`. A place in a file is named by generation and offset,
    /// and stays right when the files are renamed.
    pub(super) generation: u64,
    seq: u64,
    /// Number of the last record that reached the file.
    flushed: u64,
    ring: VecDeque<Entry>,
    /// A thread writes the buffer out after `FLUSH_DELAY`. Without it every
    /// record is written at once.
    buffered: bool,
    reopen_at: Option<Instant>,
    rotate_at: Option<Instant>,
    pruned: Instant,
}

impl State {
    /// Places of the records in memory: generation, offset, number.
    pub(super) fn places(&self) -> Vec<(u64, u64, u64)> {
        let mut places: Vec<(u64, u64, u64)> = self
            .ring
            .iter()
            .filter_map(|e| e.at.map(|(generation, offset)| (generation, offset, e.seq)))
            .collect();
        places.sort_unstable();
        places
    }

    /// Closes the file as a failed write does, with no new attempt in sight.
    #[cfg(test)]
    pub(super) fn close_for_test(&mut self) {
        flush(self);
        self.file = None;
        self.reopen_at = Some(Instant::now() + Duration::from_secs(3600));
    }

    /// Lets the next record open the file again.
    #[cfg(test)]
    pub(super) fn retry_for_test(&mut self) {
        self.reopen_at = None;
    }

    /// Records that are in memory only, the newest first.
    fn unplaced(&self) -> Vec<Row> {
        if self.dir.is_none() {
            return Vec::new();
        }
        self.ring
            .iter()
            .rev()
            .filter(|e| e.at.is_none())
            .map(|e| e.row.clone())
            .collect()
    }

    /// Rows in memory, the newest first.
    pub(super) fn rows_newest_first(&self) -> impl Iterator<Item = &Row> {
        self.ring.iter().rev().map(|e| &e.row)
    }
}

/// The state lock with the mark that this thread is inside the sink.
pub(super) struct Locked<'a> {
    state: MutexGuard<'a, State>,
    _inside: Inside,
}

impl Deref for Locked<'_> {
    type Target = State;

    fn deref(&self) -> &State {
        &self.state
    }
}

impl DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.state
    }
}

#[derive(Default)]
struct Flag {
    dirty: bool,
    stop: bool,
}

/// Wakes the thread that writes the buffer out.
#[derive(Default)]
struct Signal {
    flag: Mutex<Flag>,
    wake: Condvar,
}

impl Signal {
    fn dirty(&self) {
        let mut flag = self.flag.lock();
        if !flag.dirty {
            flag.dirty = true;
            self.wake.notify_one();
        }
    }

    fn stop(&self) {
        self.flag.lock().stop = true;
        self.wake.notify_all();
    }
}

/// The journal: a sink for the `log` macros and a reader of what it wrote.
pub struct Journal {
    pub(super) dir: Option<PathBuf>,
    stderr: bool,
    max_file_bytes: u64,
    level: AtomicU8,
    content: AtomicBool,
    pub(super) crashed: AtomicBool,
    /// `begin_run` has been called: the marker in the folder is our own.
    pub(super) began: AtomicBool,
    state: Mutex<State>,
    /// Held by whoever reads, rotates or removes the files.
    pub(super) files: Mutex<()>,
    signal: Arc<Signal>,
}

impl Journal {
    /// Opens a journal. Never fails: when the folder or the file cannot be
    /// used, records stay in memory and the file is tried again later.
    ///
    /// At the first start the file of the old logger, `mika.old.log`, is
    /// removed, and so are files past their count and age.
    pub fn open(options: Options) -> Arc<Journal> {
        let config = options.config.normalized();
        let mut state = State {
            config: config.clone(),
            dir: options.dir.clone(),
            file: None,
            size: 0,
            buf: Vec::new(),
            generation: 0,
            seq: 0,
            flushed: 0,
            ring: VecDeque::new(),
            buffered: false,
            reopen_at: None,
            rotate_at: None,
            pruned: Instant::now(),
        };
        if let Some(dir) = &options.dir {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::remove_file(dir.join(files::LEGACY));
            // And its current file: the old logger wrote recognised speech
            // there, and none of its lines can be shown or exported.
            files::remove_old_logger_file(dir);
            files::recover(dir);
            files::prune(
                dir,
                config.keep_files,
                config.keep_days,
                SystemTime::now(),
                true,
            );
            state.generation = files::earlier(dir)
                .last()
                .map_or(0, |(index, _)| u64::from(*index));
            reopen(dir, &mut state);
        }
        let journal = Arc::new(Journal {
            dir: options.dir,
            stderr: options.stderr,
            max_file_bytes: options.max_file_bytes.max(1),
            level: AtomicU8::new(config.level as u8),
            content: AtomicBool::new(config.content),
            crashed: AtomicBool::new(false),
            began: AtomicBool::new(false),
            state: Mutex::new(state),
            files: Mutex::new(()),
            signal: Arc::new(Signal::default()),
        });
        if journal.dir.is_some() {
            let weak = Arc::downgrade(&journal);
            let signal = journal.signal.clone();
            let started = std::thread::Builder::new()
                .name("mika-journal".into())
                .spawn(move || write_out_later(weak, signal))
                .is_ok();
            journal.lock().buffered = started;
        }
        journal
    }

    /// Folder of the files; `None` for a journal kept in memory.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The settings in force.
    pub fn config(&self) -> LogConfig {
        self.lock().config.clone()
    }

    /// The least severe level that is written.
    pub fn level(&self) -> Level {
        Level::from_u8(self.level.load(Ordering::Relaxed))
    }

    /// Whether texts and arguments may be written (`log.content`).
    pub fn content_allowed(&self) -> bool {
        self.content.load(Ordering::Relaxed)
    }

    /// Applies changed settings at once: the level, the content switch, and
    /// the retention, which removes files that no longer fit.
    pub fn apply(&self, config: &LogConfig) {
        let config = config.normalized();
        self.level.store(config.level as u8, Ordering::Relaxed);
        self.content.store(config.content, Ordering::Relaxed);
        let _files = self.files.lock();
        let mut st = self.lock();
        if let Some(dir) = &self.dir {
            files::prune(
                dir,
                config.keep_files,
                config.keep_days,
                SystemTime::now(),
                false,
            );
            st.pruned = Instant::now();
        }
        st.config = config;
    }

    /// Writes a record the way a `log` macro would, without the macro. An
    /// unknown category becomes `app`.
    pub fn write(&self, level: Level, category: &str, message: &str) {
        let category = CATEGORIES
            .iter()
            .copied()
            .find(|known| *known == category)
            .unwrap_or(APP);
        if self.passes(level, category) && !Inside::already() {
            self.record(level, category, message);
        }
    }

    /// Writes the buffer to the file.
    pub fn flush(&self) {
        flush(&mut self.lock());
    }

    /// Records in memory with a number above `after`, oldest first. `None`
    /// gives all of them.
    pub fn tail(&self, after: Option<u64>) -> Tail {
        let st = self.lock();
        let after = after.unwrap_or(0);
        let from = st.ring.partition_point(|e| e.seq <= after);
        Tail {
            rows: st.ring.iter().skip(from).map(|e| e.row.clone()).collect(),
            last: st.seq,
        }
    }

    /// How much the files take, how many there are and when the oldest
    /// record was written.
    pub fn summary(&self) -> Summary {
        let Some(dir) = &self.dir else {
            return Summary::default();
        };
        let _files = self.files.lock();
        self.flush();
        let paths = files::family(dir);
        let size_bytes = paths
            .iter()
            .filter_map(|path| std::fs::metadata(path).ok())
            .map(|meta| meta.len())
            .sum();
        Summary {
            size_bytes,
            files: paths.len() as u32,
            oldest: read::oldest(dir),
        }
    }

    /// Removes every record: the files and the memory. Numbers go on, so a
    /// reader that follows the tail keeps its place. One record says that the
    /// journal was cleared.
    pub fn clear(&self) -> Result<(), JournalError> {
        let removed = {
            let _files = self.files.lock();
            let mut st = self.lock();
            st.buf.clear();
            st.file = None;
            st.ring.clear();
            st.flushed = st.seq;
            match &self.dir {
                Some(dir) => {
                    let removed = files::remove_all(dir);
                    // A new generation: no place handed out before the
                    // clearing points into the new file.
                    st.generation += 1;
                    st.rotate_at = None;
                    reopen(dir, &mut st);
                    removed
                }
                None => Ok(()),
            }
        };
        self.write(Level::Info, APP, "journal cleared");
        Ok(removed?)
    }

    /// Writes the buffer out and tells where things are.
    pub(super) fn settled(&self) -> Settled {
        let mut st = self.lock();
        flush(&mut st);
        Settled {
            generation: st.generation,
            size: st.file.as_ref().map(|_| st.size),
            seq: st.seq,
            places: st.places(),
            unplaced: st.unplaced(),
        }
    }

    pub(super) fn lock(&self) -> Locked<'_> {
        let inside = Inside::enter();
        Locked {
            state: self.state.lock(),
            _inside: inside,
        }
    }

    fn passes(&self, level: Level, category: &str) -> bool {
        level <= self.level() && (category != LIB || level <= Level::Warn)
    }

    /// Scrubs a message and writes the record.
    pub(super) fn record(&self, level: Level, category: &'static str, message: &str) {
        let message = scrub::clean(message);
        let line = self.push(&mut self.lock(), level, category, message);
        self.echo(line);
    }

    /// The record of a panic: written and forced to the disk at once. Waits
    /// for the sink only so long: a panic must not hang on it. `message` is
    /// what `run::describe` made of the panic by the switch of this journal.
    pub(super) fn record_panic(&self, message: &str) {
        let message = scrub::clean(message);
        let _inside = Inside::enter();
        let Some(mut st) = self.state.try_lock_for(PANIC_WAIT) else {
            return;
        };
        self.push(&mut st, Level::Error, APP, message);
        flush(&mut st);
        if let Some(file) = &st.file {
            let _ = file.sync_data();
        }
    }

    fn echo(&self, line: Option<String>) {
        if let (true, Some(line)) = (self.stderr, line) {
            let _ = std::io::stderr().lock().write_all(line.as_bytes());
        }
    }

    /// Adds a record to the memory ring and to the buffer of the file. Gives
    /// the line back when somebody needs it.
    fn push(
        &self,
        st: &mut State,
        level: Level,
        category: &'static str,
        message: String,
    ) -> Option<String> {
        let t = Local::now().to_rfc3339_opts(SecondsFormat::Millis, false);
        let line =
            (self.dir.is_some() || self.stderr).then(|| format_line(&t, level, category, &message));
        let mut at = None;
        let mut first_in_buffer = false;
        if let (Some(dir), Some(line)) = (&self.dir, &line) {
            let len = line.len() as u64;
            if st.size > 0 && st.size + len > self.max_file_bytes {
                self.rotate(dir, st);
            }
            if st.file.is_none() && st.reopen_at.is_none_or(|at| Instant::now() >= at) {
                reopen(dir, st);
            }
            if st.file.is_some() {
                at = Some((st.generation, st.size));
                first_in_buffer = st.buf.is_empty();
                st.buf.extend_from_slice(line.as_bytes());
                st.size += len;
            }
        }
        st.seq += 1;
        st.ring.push_back(Entry {
            seq: st.seq,
            row: Row {
                seq: Some(st.seq),
                t,
                level,
                category: category.to_string(),
                message,
            },
            at,
        });
        if st.ring.len() > RING_RECORDS {
            st.ring.pop_front();
        }
        if !st.buf.is_empty() {
            if level <= Level::Warn || !st.buffered || st.buf.len() >= BUFFER_BYTES {
                flush(st);
            } else if first_in_buffer {
                self.signal.dirty();
            }
        }
        line
    }

    /// Starts a new current file. Put off when a reader holds the files or
    /// when the last attempt failed a moment ago.
    fn rotate(&self, dir: &Path, st: &mut State) {
        if st.file.is_none() || st.rotate_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        let Some(_files) = self.files.try_lock() else {
            return;
        };
        flush(st);
        if st.file.is_none() {
            return;
        }
        st.file = None;
        if files::rotate(dir) {
            st.generation += 1;
            st.rotate_at = None;
            files::prune(
                dir,
                st.config.keep_files,
                st.config.keep_days,
                SystemTime::now(),
                false,
            );
            st.pruned = Instant::now();
        } else {
            st.rotate_at = Some(Instant::now() + ROTATE_RETRY);
        }
        reopen(dir, st);
    }

    /// What the background thread does: writes the buffer out and, once in a
    /// while, removes files past their age.
    fn write_out(&self) {
        let mut st = self.lock();
        flush(&mut st);
        let Some(dir) = &self.dir else {
            return;
        };
        if st.pruned.elapsed() >= PRUNE_EVERY {
            if let Some(_files) = self.files.try_lock() {
                files::prune(
                    dir,
                    st.config.keep_files,
                    st.config.keep_days,
                    SystemTime::now(),
                    false,
                );
            }
            st.pruned = Instant::now();
        }
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.signal.stop();
        flush(self.state.get_mut());
    }
}

impl log::Log for Journal {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        Level::from_log(metadata.level())
            .is_some_and(|level| self.passes(level, category_of(metadata.target())))
    }

    fn log(&self, record: &log::Record<'_>) {
        let Some(level) = Level::from_log(record.level()) else {
            return;
        };
        let category = category_of(record.target());
        if !self.passes(level, category) || Inside::already() {
            return;
        }
        // What is being printed may log by itself: that record is dropped.
        let _inside = Inside::enter();
        let mut message = String::new();
        let _ = std::fmt::write(&mut message, *record.args());
        self.record(level, category, &message);
    }

    fn flush(&self) {
        Journal::flush(self);
    }
}

/// Opens the current file again. The buffer must be empty.
fn reopen(dir: &Path, st: &mut State) {
    match files::open_current(dir) {
        Ok((file, size)) => {
            st.file = Some(file);
            st.size = size;
            st.reopen_at = None;
        }
        Err(_) => {
            st.file = None;
            st.reopen_at = Some(Instant::now() + REOPEN_DELAY);
        }
    }
}

/// Writes the buffer to the file. A failed write drops the buffer: the
/// records stay in memory, without a place in the file.
fn flush(st: &mut State) {
    if st.buf.is_empty() {
        return;
    }
    if st.file.is_some()
        && st
            .dir
            .as_deref()
            .is_some_and(|dir| !files::has_current(dir))
    {
        start_anew(st);
    }
    let written = st
        .file
        .as_mut()
        .is_some_and(|file| file.write_all(&st.buf).is_ok());
    st.buf.clear();
    if !written {
        st.file = None;
        st.reopen_at = Some(Instant::now() + REOPEN_DELAY);
        let flushed = st.flushed;
        for entry in st.ring.iter_mut().rev() {
            if entry.seq <= flushed {
                break;
            }
            entry.at = None;
        }
    }
    st.flushed = st.seq;
}

/// Somebody removed or renamed the current file while the app runs: the open
/// handle would go on writing into a file nobody can find. A new file is
/// started. What the lost file held of this run stays in memory, without a
/// place; what waits in the buffer goes to the start of the new file.
fn start_anew(st: &mut State) {
    let Some(dir) = st.dir.clone() else {
        return;
    };
    st.file = None;
    reopen(&dir, st);
    let opened = st.file.is_some();
    let (generation, flushed) = (st.generation, st.flushed);
    let mut offset = st.size;
    for entry in st.ring.iter_mut() {
        if !entry.at.is_some_and(|(of_entry, _)| of_entry == generation) {
            continue;
        }
        if entry.seq <= flushed || !opened {
            entry.at = None;
            continue;
        }
        let row = &entry.row;
        let len = format_line(&row.t, row.level, &row.category, &row.message).len() as u64;
        entry.at = Some((generation, offset));
        offset += len;
    }
    if opened {
        st.size = offset;
    }
}

/// The background thread: after the first record lands in an empty buffer it
/// waits `FLUSH_DELAY` and writes the buffer out. Ends when the journal is
/// dropped.
fn write_out_later(journal: Weak<Journal>, signal: Arc<Signal>) {
    loop {
        {
            let mut flag = signal.flag.lock();
            while !flag.dirty && !flag.stop {
                signal.wake.wait(&mut flag);
            }
            if !flag.stop {
                signal.wake.wait_for(&mut flag, FLUSH_DELAY);
            }
            if flag.stop {
                return;
            }
            flag.dirty = false;
        }
        match journal.upgrade() {
            Some(journal) => journal.write_out(),
            None => return,
        }
    }
}
