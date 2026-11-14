//! "Wait and tell": conditions she watches until they hold, and what she says
//! then. Watches live in memory and end with the app: after a restart a pid
//! may belong to another program and "the file grows" has lost its start.
//!
//! Checking is a function of what the watcher saw (`World`), so it is tested
//! without Windows. No text built here holds a window title or window text.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use mika_core::triggers::one_line;
use parking_lot::Mutex;

use super::sensors::Proc;
use crate::batch::size_text;

pub const MAX_WAITS: usize = 5;
pub const DEFAULT_TIMEOUT_MIN: u64 = 60;
pub const MAX_TIMEOUT_MIN: u64 = 480;
/// A path unchanged this long has stopped growing.
pub const QUIET_S: i64 = 30;
pub const MAX_SAY: usize = 200;
pub const MAX_PHRASE: usize = 200;
/// Endings of files a browser is still downloading.
pub(crate) const PARTIAL: [&str; 3] = [".crdownload", ".part", ".partial"];
/// A watch past its term by this long ends at any look, also one that did
/// not see the watched thing (a path or a window that stopped answering).
const OVERDUE_S: i64 = 60;
/// Entries of a folder that are looked at.
const MAX_ENTRIES: usize = 5000;

pub(crate) const FULL: &str = "Я уже слежу за пятью условиями. Останови лишнее через wait_stop.";

#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    PathAppears {
        path: PathBuf,
    },
    PathSettles {
        path: PathBuf,
    },
    ProcessExits {
        name: String,
        /// One process: its pid and its start time, so a reused pid is not it.
        pid: Option<(u32, u64)>,
    },
    WindowText {
        window: u64,
        /// The program of the window. Its title is never kept.
        process: String,
        phrase: String,
        /// `false`: wait until the phrase is gone.
        present: bool,
    },
}

/// The last part of a path, as the user calls the file.
pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl Condition {
    pub fn kind(&self) -> &'static str {
        match self {
            Condition::PathAppears { .. } => "file_appears",
            Condition::PathSettles { .. } => "file_ready",
            Condition::ProcessExits { .. } => "process_exits",
            Condition::WindowText { present: true, .. } => "window_text",
            Condition::WindowText { present: false, .. } => "window_text_gone",
        }
    }

    /// What is watched, for the settings page: a file name, a process name,
    /// or the program with the phrase.
    pub fn what(&self) -> String {
        match self {
            Condition::PathAppears { path } | Condition::PathSettles { path } => file_name(path),
            Condition::ProcessExits { name, .. } => name.clone(),
            Condition::WindowText {
                process, phrase, ..
            } => format!("{process}: {phrase}"),
        }
    }

    /// The condition in words: «скажу, когда …».
    pub fn words(&self) -> String {
        match self {
            Condition::PathAppears { path } => format!("появится «{}»", file_name(path)),
            Condition::PathSettles { path } => format!("«{}» перестанет расти", file_name(path)),
            Condition::ProcessExits { name, pid: None } => format!("завершится процесс {name}"),
            Condition::ProcessExits {
                name,
                pid: Some((pid, _)),
            } => format!("завершится процесс {name} (pid {pid})"),
            Condition::WindowText {
                process,
                phrase,
                present: true,
                ..
            } => format!("в окне программы {process} появится «{phrase}»"),
            Condition::WindowText {
                process,
                phrase,
                present: false,
                ..
            } => format!("из окна программы {process} пропадёт «{phrase}»"),
        }
    }

    /// Why the watch ended without its event.
    fn not_yet(&self) -> String {
        match self {
            Condition::PathAppears { path } => format!("«{}» не появился", file_name(path)),
            Condition::PathSettles { path } => {
                format!("«{}» всё ещё меняется или его нет", file_name(path))
            }
            Condition::ProcessExits { name, .. } => format!("процесс {name} ещё работает"),
            Condition::WindowText {
                process,
                phrase,
                present: true,
                ..
            } => format!("фраза «{phrase}» не появилась в окне программы {process}"),
            Condition::WindowText {
                process,
                phrase,
                present: false,
                ..
            } => format!("фраза «{phrase}» всё ещё в окне программы {process}"),
        }
    }
}

/// One watch as the settings page shows it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WaitView {
    pub id: u32,
    /// `file_appears`, `file_ready`, `process_exits`, `window_text`, `window_text_gone`.
    pub kind: &'static str,
    pub what: String,
    /// Unix seconds: when the watch began and when it gives up.
    pub since: i64,
    pub until: i64,
}

/// What a path looks like from outside. Only metadata: no file is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PathSig {
    pub dir: bool,
    /// A file: its size. A folder: the sizes of its files, top level only.
    pub size: u64,
    /// Unix seconds; for a folder the newest of its own and its entries'.
    pub modified: i64,
    /// How many entries a folder has; 0 for a file.
    pub entries: u32,
    /// A download is still going: a file with a browser's partial ending is
    /// in the folder, or lies next to the file under its name.
    pub partial: bool,
}

fn modified(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// `None`: the path is not there.
pub(crate) fn path_sig(path: &Path) -> Option<PathSig> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_dir() {
        let name = file_name(path);
        let partial = PARTIAL
            .iter()
            .any(|end| path.with_file_name(format!("{name}{end}")).exists());
        return Some(PathSig {
            dir: false,
            size: meta.len(),
            modified: modified(&meta),
            entries: 0,
            partial,
        });
    }
    let mut sig = PathSig {
        dir: true,
        size: 0,
        modified: modified(&meta),
        entries: 0,
        partial: false,
    };
    // A folder that cannot be listed is still there.
    let entries = std::fs::read_dir(path).into_iter().flatten().flatten();
    for entry in entries.take(MAX_ENTRIES) {
        sig.entries += 1;
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if PARTIAL.iter().any(|end| name.ends_with(end)) {
            sig.partial = true;
        }
        if let Ok(m) = entry.metadata() {
            if m.is_file() {
                sig.size += m.len();
            }
            sig.modified = sig.modified.max(modified(&m));
        }
    }
    Some(sig)
}

/// What the watcher made of a watched window at this look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WindowLook {
    /// The window is closed.
    Gone,
    /// Its title is in the "blind zone" now: the text was not read.
    Blind,
    /// The text could not be read this time.
    Unread,
    Text(String),
}

/// What the watcher saw at one look. A watch whose part is missing here
/// (not looked at this time) keeps waiting.
pub(crate) struct World {
    /// Processes of the user's session; `None`: not read.
    pub procs: Option<Vec<Proc>>,
    /// Watched paths; the value `None`: the path is not there.
    pub paths: HashMap<PathBuf, Option<PathSig>>,
    pub windows: HashMap<u64, WindowLook>,
    /// The newest watch there was when the look began (`Waits::mark`). A
    /// watch added while the system was being read is not judged by what was
    /// read before it existed: it waits for the next look.
    pub upto: u32,
}

impl Default for World {
    fn default() -> Self {
        Self {
            procs: None,
            paths: HashMap::new(),
            windows: HashMap::new(),
            upto: u32::MAX,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Keep,
    /// The condition holds: what to tell.
    Done(String),
    /// The watch cannot go on: what to tell.
    Dropped(String),
}

struct Item {
    id: u32,
    condition: Condition,
    /// What to add to the built text when the condition holds; may be empty.
    say: String,
    since: i64,
    until: i64,
    /// `PathSettles`: the last look at the path and since when it is the same.
    seen: Option<(PathSig, i64)>,
}

/// Lower case, runs of whitespace as one space.
fn norm(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Item {
    fn view(&self) -> WaitView {
        WaitView {
            id: self.id,
            kind: self.condition.kind(),
            what: self.condition.what(),
            since: self.since,
            until: self.until,
        }
    }

    /// The term ran out. It is said by a look that saw the watched thing
    /// itself, so that an event at the last moment still beats the term; a
    /// watch whose thing cannot be looked at any more is ended a little later
    /// by any look.
    fn overdue(&self, world: &World, at: i64) -> bool {
        let looked = match &self.condition {
            Condition::PathAppears { path } | Condition::PathSettles { path } => {
                world.paths.contains_key(path)
            }
            Condition::ProcessExits { .. } => world.procs.is_some(),
            Condition::WindowText { window, .. } => world.windows.contains_key(window),
        };
        at >= self.until && (looked || at >= self.until + OVERDUE_S)
    }

    fn check(&mut self, world: &World, at: i64) -> Verdict {
        match &self.condition {
            Condition::PathAppears { path } => match world.paths.get(path) {
                Some(Some(sig)) => {
                    let name = file_name(path);
                    Verdict::Done(if sig.dir {
                        format!("Появилась папка «{name}».")
                    } else {
                        format!("Появился файл «{name}».")
                    })
                }
                _ => Verdict::Keep,
            },
            Condition::PathSettles { path } => {
                let Some(now) = world.paths.get(path) else {
                    return Verdict::Keep;
                };
                let Some(sig) = *now else {
                    self.seen = None;
                    return Verdict::Keep;
                };
                let quiet_since = match self.seen {
                    Some((old, since)) if old == sig => since,
                    _ => {
                        self.seen = Some((sig, at));
                        return Verdict::Keep;
                    }
                };
                // An empty file is a place kept for a download, not its end.
                let whole = !sig.partial && (sig.dir || sig.size > 0);
                if !whole || at - quiet_since < QUIET_S {
                    return Verdict::Keep;
                }
                let (name, size) = (file_name(path), size_text(sig.size));
                Verdict::Done(if sig.dir {
                    format!(
                        "Папка «{name}» больше не меняется: файлов {}, {size}.",
                        sig.entries
                    )
                } else {
                    format!("Файл «{name}» готов: больше не растёт, {size}.")
                })
            }
            Condition::ProcessExits { name, pid } => {
                let Some(procs) = &world.procs else {
                    return Verdict::Keep;
                };
                let alive = match pid {
                    // Another start time under the same pid is another process.
                    Some((pid, started)) => {
                        procs.iter().any(|p| p.pid == *pid && p.started == *started)
                    }
                    None => procs.iter().any(|p| p.name == *name),
                };
                if alive {
                    Verdict::Keep
                } else {
                    Verdict::Done(format!("Процесс {name} завершился."))
                }
            }
            Condition::WindowText {
                window,
                process,
                phrase,
                present,
            } => match world.windows.get(window) {
                None | Some(WindowLook::Unread) => Verdict::Keep,
                Some(WindowLook::Gone) => Verdict::Dropped(format!(
                    "Окно программы {process} закрылось, больше за ним не слежу."
                )),
                Some(WindowLook::Blind) => Verdict::Dropped(format!(
                    "Окно программы {process} теперь в «слепой зоне», больше за ним не слежу."
                )),
                Some(WindowLook::Text(text)) => {
                    if norm(text).contains(&norm(phrase)) != *present {
                        Verdict::Keep
                    } else if *present {
                        Verdict::Done(format!(
                            "В окне программы {process} появилась фраза «{phrase}»."
                        ))
                    } else {
                        Verdict::Done(format!(
                            "Из окна программы {process} пропала фраза «{phrase}»."
                        ))
                    }
                }
            },
        }
    }
}

/// Whether `text` holds `phrase`, as a window watch compares them.
pub(crate) fn holds(text: &str, phrase: &str) -> bool {
    norm(text).contains(&norm(phrase))
}

#[derive(Default)]
struct Inner {
    /// The last id given out; ids count up within one run of the app.
    last: u32,
    items: Vec<Item>,
}

/// The running watches, shared by the tools, the watcher and the settings page.
#[derive(Clone, Default)]
pub struct Waits(Arc<Mutex<Inner>>);

impl Waits {
    /// Starts a watch. The same condition again makes no second watch: the
    /// running one takes the new text and term.
    pub fn add(
        &self,
        condition: Condition,
        say: &str,
        timeout_min: u64,
        at: i64,
    ) -> Result<WaitView, String> {
        // One line: it is shown on the confirm card and said after the fact.
        let say: String = one_line(say).chars().take(MAX_SAY).collect();
        let until = at + timeout_min.clamp(1, MAX_TIMEOUT_MIN) as i64 * 60;
        let mut inner = self.0.lock();
        if let Some(same) = inner.items.iter_mut().find(|i| i.condition == condition) {
            same.say = say;
            same.until = until;
            return Ok(same.view());
        }
        if inner.items.len() >= MAX_WAITS {
            return Err(FULL.into());
        }
        inner.last += 1;
        let item = Item {
            id: inner.last,
            condition,
            say,
            since: at,
            until,
            seen: None,
        };
        let view = item.view();
        inner.items.push(item);
        Ok(view)
    }

    /// In the order they were added.
    pub fn list(&self) -> Vec<WaitView> {
        self.0.lock().items.iter().map(Item::view).collect()
    }

    pub fn stop(&self, id: u32) -> bool {
        let mut inner = self.0.lock();
        let before = inner.items.len();
        inner.items.retain(|i| i.id != id);
        inner.items.len() != before
    }

    /// How many watches were stopped.
    pub fn stop_all(&self) -> usize {
        std::mem::take(&mut self.0.lock().items).len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.lock().items.is_empty()
    }

    pub(crate) fn is_full(&self) -> bool {
        self.0.lock().items.len() >= MAX_WAITS
    }

    /// The newest watch there is now, for `World::upto`.
    pub(crate) fn mark(&self) -> u32 {
        self.0.lock().last
    }

    /// Ends every watch of a window: reading windows was switched off. The
    /// result is what to tell about each: (id, text).
    pub(crate) fn stop_windows(&self) -> Vec<(u32, String)> {
        let mut told = Vec::new();
        self.0.lock().items.retain(|item| match &item.condition {
            Condition::WindowText { process, .. } => {
                told.push((
                    item.id,
                    format!("Чтение окон выключено, больше не слежу за окном программы {process}."),
                ));
                false
            }
            _ => true,
        });
        told
    }

    pub(crate) fn conditions(&self) -> Vec<Condition> {
        self.0
            .lock()
            .items
            .iter()
            .map(|i| i.condition.clone())
            .collect()
    }

    /// Every watch with its condition in words.
    pub(crate) fn described(&self) -> Vec<(WaitView, String)> {
        self.0
            .lock()
            .items
            .iter()
            .map(|i| (i.view(), i.condition.words()))
            .collect()
    }

    /// Checks every watch against what was seen. A watch that is done, cannot
    /// go on or ran out of time is removed; the result is what to tell about
    /// each: (id, text). The text always names what happened; the words the
    /// model asked to say come after it, never in its place.
    pub(crate) fn check_all(&self, world: &World, at: i64) -> Vec<(u32, String)> {
        let mut told = Vec::new();
        self.0.lock().items.retain_mut(|item| {
            if item.id > world.upto {
                return true;
            }
            let text = match item.check(world, at) {
                Verdict::Done(text) if !item.say.is_empty() => format!("{text} {}", item.say),
                Verdict::Done(text) | Verdict::Dropped(text) => text,
                Verdict::Keep if item.overdue(world, at) => format!(
                    "Не дождалась за {} мин: {}.",
                    (item.until - item.since) / 60,
                    item.condition.not_yet()
                ),
                Verdict::Keep => return true,
            };
            told.push((item.id, text));
            false
        });
        told
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(size: u64, modified: i64) -> PathSig {
        PathSig {
            dir: false,
            size,
            modified,
            entries: 0,
            partial: false,
        }
    }

    fn world_path(path: &str, sig: Option<PathSig>) -> World {
        World {
            paths: [(PathBuf::from(path), sig)].into(),
            ..Default::default()
        }
    }

    fn world_procs(procs: &[(u32, &str, u64)]) -> World {
        World {
            procs: Some(
                procs
                    .iter()
                    .map(|(pid, name, started)| Proc {
                        pid: *pid,
                        name: name.to_string(),
                        started: *started,
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    fn world_window(id: u64, look: WindowLook) -> World {
        World {
            windows: [(id, look)].into(),
            ..Default::default()
        }
    }

    fn window(phrase: &str, present: bool) -> Condition {
        Condition::WindowText {
            window: 7,
            process: "setup.exe".into(),
            phrase: phrase.into(),
            present,
        }
    }

    const ISO: &str = "C:\\Users\\me\\Downloads\\ubuntu.iso";

    #[test]
    fn a_watch_has_a_number_a_term_and_a_view() {
        let w = Waits::default();
        assert!(w.is_empty());
        let a = w
            .add(
                Condition::PathSettles { path: ISO.into() },
                "",
                DEFAULT_TIMEOUT_MIN,
                1000,
            )
            .unwrap();
        assert_eq!(
            a,
            WaitView {
                id: 1,
                kind: "file_ready",
                what: "ubuntu.iso".into(),
                since: 1000,
                until: 1000 + 3600,
            }
        );
        let b = w
            .add(
                Condition::ProcessExits {
                    name: "cargo.exe".into(),
                    pid: Some((42, 900)),
                },
                "",
                // The term is kept within 1..=480 minutes.
                100_000,
                2000,
            )
            .unwrap();
        assert_eq!(
            (b.id, b.kind, b.what.as_str()),
            (2, "process_exits", "cargo.exe")
        );
        assert_eq!(b.until, 2000 + 480 * 60);
        let c = w.add(window("Готово", true), "", 0, 3000).unwrap();
        assert_eq!(
            (c.kind, c.what.as_str()),
            ("window_text", "setup.exe: Готово")
        );
        assert_eq!(c.until, 3000 + 60);
        let d = w.add(window("Подождите", false), "", 5, 3000).unwrap();
        assert_eq!(d.kind, "window_text_gone");
        let e = w
            .add(Condition::PathAppears { path: ISO.into() }, "", 5, 3000)
            .unwrap();
        assert_eq!(
            (e.id, e.kind, e.what.as_str()),
            (5, "file_appears", "ubuntu.iso")
        );
        assert_eq!(w.list(), vec![a, b, c, d, e]);
        assert_eq!(
            serde_json::to_value(&w.list()[1]).unwrap(),
            serde_json::json!({"id": 2, "kind": "process_exits", "what": "cargo.exe", "since": 2000, "until": 30_800})
        );
    }

    #[test]
    fn the_sixth_watch_is_refused_and_stopping_makes_room() {
        let w = Waits::default();
        for i in 0..MAX_WAITS {
            let path = PathBuf::from(format!("C:\\f{i}"));
            w.add(Condition::PathAppears { path }, "", 5, 0).unwrap();
        }
        assert!(w.is_full());
        let extra = Condition::PathAppears {
            path: "C:\\extra".into(),
        };
        assert_eq!(w.add(extra.clone(), "", 5, 0), Err(FULL.to_string()));
        assert!(w.stop(2));
        assert!(!w.stop(2));
        // Ids are not reused.
        assert_eq!(w.add(extra, "", 5, 0).unwrap().id, 6);
        assert_eq!(w.stop_all(), 5);
        assert!(w.is_empty());
        assert_eq!(w.stop_all(), 0);
    }

    #[test]
    fn the_same_condition_again_renews_the_running_watch() {
        let w = Waits::default();
        let c = Condition::ProcessExits {
            name: "cargo.exe".into(),
            pid: None,
        };
        let first = w.add(c.clone(), "", 10, 0).unwrap();
        let again = w.add(c.clone(), " Сборка закончилась ", 20, 300).unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!((again.since, again.until), (0, 300 + 1200));
        assert_eq!(w.list().len(), 1);
        assert_eq!(
            w.check_all(&world_procs(&[]), 400),
            vec![(
                1,
                "Процесс cargo.exe завершился. Сборка закончилась".to_string()
            )]
        );
    }

    #[test]
    fn conditions_are_put_in_words() {
        let words = |c: Condition| c.words();
        assert_eq!(
            words(Condition::PathAppears { path: ISO.into() }),
            "появится «ubuntu.iso»"
        );
        assert_eq!(
            words(Condition::PathSettles { path: ISO.into() }),
            "«ubuntu.iso» перестанет расти"
        );
        assert_eq!(
            words(Condition::ProcessExits {
                name: "cargo.exe".into(),
                pid: None
            }),
            "завершится процесс cargo.exe"
        );
        assert_eq!(
            words(Condition::ProcessExits {
                name: "cargo.exe".into(),
                pid: Some((42, 1))
            }),
            "завершится процесс cargo.exe (pid 42)"
        );
        assert_eq!(
            words(window("Готово", true)),
            "в окне программы setup.exe появится «Готово»"
        );
        assert_eq!(
            words(window("Подождите", false)),
            "из окна программы setup.exe пропадёт «Подождите»"
        );
        // A drive has no last part: the whole path is shown.
        assert_eq!(file_name(Path::new("D:\\")), "D:\\");
    }

    #[test]
    fn a_file_or_a_folder_appears() {
        let w = Waits::default();
        w.add(Condition::PathAppears { path: ISO.into() }, "", 5, 0)
            .unwrap();
        // Not looked at, then not there.
        assert!(w.check_all(&World::default(), 5).is_empty());
        assert!(w.check_all(&world_path(ISO, None), 10).is_empty());
        assert_eq!(
            w.check_all(&world_path(ISO, Some(file(0, 15))), 15),
            vec![(1, "Появился файл «ubuntu.iso».".to_string())]
        );
        assert!(w.is_empty());

        w.add(
            Condition::PathAppears {
                path: "C:\\out".into(),
            },
            "",
            5,
            0,
        )
        .unwrap();
        let folder = PathSig {
            dir: true,
            size: 0,
            modified: 1,
            entries: 0,
            partial: false,
        };
        assert_eq!(
            w.check_all(&world_path("C:\\out", Some(folder)), 5),
            vec![(2, "Появилась папка «out».".to_string())]
        );
    }

    #[test]
    fn a_file_is_ready_after_a_quiet_time_without_a_partial_twin() {
        let w = Waits::default();
        w.add(Condition::PathSettles { path: ISO.into() }, "", 60, 0)
            .unwrap();
        let look = |sig, at| w.check_all(&world_path(ISO, sig), at);
        // Not there yet, then growing.
        assert!(look(None, 0).is_empty());
        assert!(look(Some(file(100, 5)), 5).is_empty());
        assert!(look(Some(file(200, 10)), 10).is_empty());
        // The same for almost QUIET_S, then it grows again: the count restarts.
        assert!(look(Some(file(200, 10)), 10 + QUIET_S - 1).is_empty());
        assert!(look(Some(file(300, 40)), 40).is_empty());
        assert!(look(Some(file(300, 40)), 40 + QUIET_S - 1).is_empty());
        // Quiet, but the browser's partial file is still next to it.
        let partial = PathSig {
            partial: true,
            ..file(300, 40)
        };
        assert!(look(Some(partial), 100).is_empty());
        assert!(look(Some(partial), 200).is_empty());
        // The partial file is gone: a change, so QUIET_S more.
        assert!(look(Some(file(300, 40)), 205).is_empty());
        assert!(look(Some(file(300, 40)), 205 + QUIET_S - 1).is_empty());
        assert_eq!(
            look(Some(file(300, 40)), 205 + QUIET_S),
            vec![(
                1,
                "Файл «ubuntu.iso» готов: больше не растёт, 300 Б.".to_string()
            )]
        );
        assert!(w.is_empty());
    }

    #[test]
    fn a_file_that_vanishes_starts_the_count_again_and_an_empty_one_is_not_ready() {
        let w = Waits::default();
        w.add(Condition::PathSettles { path: ISO.into() }, "", 60, 0)
            .unwrap();
        let look = |sig, at| w.check_all(&world_path(ISO, sig), at);
        assert!(look(Some(file(500, 1)), 0).is_empty());
        assert!(look(None, 20).is_empty());
        // Back and the same as before: still counted from its return.
        assert!(look(Some(file(500, 1)), 25).is_empty());
        assert!(look(Some(file(500, 1)), 25 + QUIET_S - 1).is_empty());
        assert_eq!(look(Some(file(500, 1)), 25 + QUIET_S).len(), 1);

        // A file of zero bytes is a place kept for a download.
        w.add(Condition::PathSettles { path: ISO.into() }, "", 60, 0)
            .unwrap();
        assert!(look(Some(file(0, 1)), 100).is_empty());
        assert!(look(Some(file(0, 1)), 100 + 10 * QUIET_S).is_empty());
        assert!(!w.is_empty());
    }

    #[test]
    fn a_folder_settles_with_its_count_and_size() {
        let w = Waits::default();
        let path = "C:\\Users\\me\\Downloads\\album";
        w.add(Condition::PathSettles { path: path.into() }, "", 60, 0)
            .unwrap();
        let folder = |entries, size| PathSig {
            dir: true,
            size,
            modified: 1,
            entries,
            partial: false,
        };
        let look = |sig, at| w.check_all(&world_path(path, Some(sig)), at);
        assert!(look(folder(1, 1024), 0).is_empty());
        assert!(look(folder(2, 4096), 10).is_empty());
        assert!(look(folder(2, 4096), 39).is_empty());
        assert_eq!(
            look(folder(2, 4096), 40),
            vec![(
                1,
                "Папка «album» больше не меняется: файлов 2, 4 КБ.".to_string()
            )]
        );
    }

    #[test]
    fn a_process_exits_by_name_or_by_pid() {
        let w = Waits::default();
        w.add(
            Condition::ProcessExits {
                name: "cargo.exe".into(),
                pid: None,
            },
            "",
            60,
            0,
        )
        .unwrap();
        // The process list was not read: nothing is known.
        assert!(w.check_all(&World::default(), 5).is_empty());
        // One of two is gone: the program still runs.
        assert!(w
            .check_all(
                &world_procs(&[(2, "cargo.exe", 10), (3, "rustc.exe", 11)]),
                10
            )
            .is_empty());
        assert_eq!(
            w.check_all(&world_procs(&[(3, "rustc.exe", 11)]), 15),
            vec![(1, "Процесс cargo.exe завершился.".to_string())]
        );

        w.add(
            Condition::ProcessExits {
                name: "cargo.exe".into(),
                pid: Some((2, 10)),
            },
            "",
            60,
            0,
        )
        .unwrap();
        assert!(w
            .check_all(&world_procs(&[(2, "cargo.exe", 10)]), 20)
            .is_empty());
        // The pid lives on in a process started later, another cargo even:
        // the watched one has exited.
        assert_eq!(
            w.check_all(
                &world_procs(&[(2, "cargo.exe", 99), (5, "cargo.exe", 50)]),
                25
            ),
            vec![(2, "Процесс cargo.exe завершился.".to_string())]
        );
    }

    #[test]
    fn a_phrase_appears_in_a_window_or_goes() {
        let w = Waits::default();
        w.add(window("Установка  завершена", true), "", 60, 0)
            .unwrap();
        let look = |look, at| w.check_all(&world_window(7, look), at);
        assert!(look(WindowLook::Text("Копирование файлов…".into()), 10).is_empty());
        // Not read this time, or not looked at: keep waiting.
        assert!(look(WindowLook::Unread, 20).is_empty());
        assert!(w.check_all(&World::default(), 25).is_empty());
        // Case and line breaks do not matter.
        assert_eq!(
            look(
                WindowLook::Text("Мастер\nУСТАНОВКА\nзавершена\nГотово".into()),
                30
            ),
            vec![(
                1,
                "В окне программы setup.exe появилась фраза «Установка  завершена».".to_string()
            )]
        );

        w.add(window("Подождите", false), "", 60, 0).unwrap();
        assert!(look(WindowLook::Text("Подождите, идёт установка".into()), 40).is_empty());
        assert_eq!(
            look(WindowLook::Text("Готово".into()), 50),
            vec![(
                2,
                "Из окна программы setup.exe пропала фраза «Подождите».".to_string()
            )]
        );
        assert!(holds("Мастер\n Готово ", "мастер готово"));
        assert!(!holds("Мастер", "Готово"));
    }

    #[test]
    fn a_closed_or_blind_window_ends_the_watch_without_its_title() {
        let w = Waits::default();
        w.add(window("Готово", true), "Установилось", 60, 0)
            .unwrap();
        // The model's own text is for the event, not for a watch that broke.
        assert_eq!(
            w.check_all(&world_window(7, WindowLook::Gone), 10),
            vec![(
                1,
                "Окно программы setup.exe закрылось, больше за ним не слежу.".to_string()
            )]
        );
        w.add(window("Готово", false), "", 60, 0).unwrap();
        assert_eq!(
            w.check_all(&world_window(7, WindowLook::Blind), 20),
            vec![(
                2,
                "Окно программы setup.exe теперь в «слепой зоне», больше за ним не слежу."
                    .to_string()
            )]
        );
        assert!(w.is_empty());
    }

    #[test]
    fn a_watch_that_runs_out_of_time_says_what_it_did_not_see() {
        let cases = [
            (
                Condition::PathAppears { path: ISO.into() },
                "Не дождалась за 5 мин: «ubuntu.iso» не появился.",
            ),
            (
                Condition::PathSettles { path: ISO.into() },
                "Не дождалась за 5 мин: «ubuntu.iso» всё ещё меняется или его нет.",
            ),
            (
                Condition::ProcessExits {
                    name: "cargo.exe".into(),
                    pid: Some((2, 10)),
                },
                "Не дождалась за 5 мин: процесс cargo.exe ещё работает.",
            ),
            (
                window("Готово", true),
                "Не дождалась за 5 мин: фраза «Готово» не появилась в окне программы setup.exe.",
            ),
            (
                window("Подождите", false),
                "Не дождалась за 5 мин: фраза «Подождите» всё ещё в окне программы setup.exe.",
            ),
        ];
        for (i, (condition, text)) in cases.into_iter().enumerate() {
            let w = Waits::default();
            // The model's own text is not said for a failure.
            w.add(condition, "Готово!", 5, 1000).unwrap();
            let world = World {
                procs: Some(vec![Proc {
                    pid: 2,
                    name: "cargo.exe".into(),
                    started: 10,
                }]),
                paths: [(PathBuf::from(ISO), None)].into(),
                windows: [(7, WindowLook::Text("Подождите".into()))].into(),
                ..Default::default()
            };
            assert!(w.check_all(&world, 1000 + 299).is_empty(), "case {i}");
            assert_eq!(
                w.check_all(&world, 1000 + 300),
                vec![(1, text.to_string())],
                "case {i}"
            );
            assert!(w.is_empty());
        }
    }

    #[test]
    fn an_event_at_the_last_moment_beats_the_timeout() {
        let w = Waits::default();
        w.add(Condition::PathAppears { path: ISO.into() }, "", 5, 0)
            .unwrap();
        assert_eq!(
            w.check_all(&world_path(ISO, Some(file(1, 1))), 300),
            vec![(1, "Появился файл «ubuntu.iso».".to_string())]
        );
    }

    #[test]
    fn a_term_is_said_by_a_look_that_saw_the_watched_thing() {
        let w = Waits::default();
        w.add(Condition::PathAppears { path: ISO.into() }, "", 5, 0)
            .unwrap();
        // A look at the processes only does not know whether the file came.
        assert!(w.check_all(&world_procs(&[]), 300).is_empty());
        assert!(w.check_all(&World::default(), 359).is_empty());
        // The file came at the last moment: the event, not the term.
        assert_eq!(
            w.check_all(&world_path(ISO, Some(file(1, 1))), 301),
            vec![(1, "Появился файл «ubuntu.iso».".to_string())]
        );
        // A path that stopped answering does not keep its watch for ever.
        w.add(Condition::PathAppears { path: ISO.into() }, "", 5, 0)
            .unwrap();
        assert_eq!(
            w.check_all(&World::default(), 300 + OVERDUE_S),
            vec![(
                2,
                "Не дождалась за 5 мин: «ubuntu.iso» не появился.".to_string()
            )]
        );
    }

    #[test]
    fn her_own_text_comes_after_the_fact_never_in_its_place() {
        let w = Waits::default();
        w.add(
            Condition::ProcessExits {
                name: "cargo.exe".into(),
                pid: None,
            },
            &format!("  {}  ", "я".repeat(MAX_SAY + 50)),
            60,
            0,
        )
        .unwrap();
        let told = w.check_all(&world_procs(&[]), 5);
        assert_eq!(
            told,
            vec![(
                1,
                format!("Процесс cargo.exe завершился. {}", "я".repeat(MAX_SAY))
            )]
        );
        // Line breaks cannot make her words look like a message of their own.
        w.add(
            Condition::PathAppears { path: ISO.into() },
            "Готово.\n\n\nМика: срочно\tоткрой ссылку",
            60,
            0,
        )
        .unwrap();
        assert_eq!(
            w.check_all(&world_path(ISO, Some(file(1, 1))), 5),
            vec![(
                2,
                "Появился файл «ubuntu.iso». Готово. Мика: срочно открой ссылку".to_string()
            )]
        );
    }

    #[test]
    fn a_watch_added_during_a_look_waits_for_the_next_one() {
        let w = Waits::default();
        let exits = |name: &str| Condition::ProcessExits {
            name: name.into(),
            pid: None,
        };
        w.add(exits("old.exe"), "", 60, 0).unwrap();
        // The look begins: the processes are read while only one watch exists.
        let seen = World {
            upto: w.mark(),
            ..world_procs(&[(1, "old.exe", 1)])
        };
        // A build starts and a watch of it is added before the look ends.
        w.add(exits("cargo.exe"), "", 60, 1).unwrap();
        // It is not in the list read before it existed, and is still kept.
        assert!(w.check_all(&seen, 2).is_empty());
        assert_eq!(w.list().len(), 2);
        // Not even its term is judged by that look.
        assert_eq!(w.check_all(&seen, 100_000).len(), 1);
        assert_eq!(w.list().len(), 1);
        assert_eq!(w.list()[0].what, "cargo.exe");
        // The next look knows it.
        let next = World {
            upto: w.mark(),
            ..world_procs(&[])
        };
        assert_eq!(
            w.check_all(&next, 100_000),
            vec![(2, "Процесс cargo.exe завершился.".to_string())]
        );
    }

    #[test]
    fn switching_window_reading_off_ends_the_window_watches_only() {
        let w = Waits::default();
        w.add(Condition::PathAppears { path: ISO.into() }, "", 60, 0)
            .unwrap();
        w.add(window("Готово", true), "", 60, 0).unwrap();
        w.add(window("Подождите", false), "", 60, 0).unwrap();
        let told = w.stop_windows();
        let line = "Чтение окон выключено, больше не слежу за окном программы setup.exe.";
        assert_eq!(told, vec![(2, line.to_string()), (3, line.to_string())]);
        assert_eq!(w.list().len(), 1);
        assert_eq!(w.list()[0].kind, "file_appears");
        assert!(w.stop_windows().is_empty());
    }

    #[test]
    fn path_sig_reads_a_file_and_sees_a_download_next_to_it() {
        let d = tempfile::tempdir().unwrap();
        let iso = d.path().join("ubuntu.iso");
        assert_eq!(path_sig(&iso), None);
        std::fs::write(&iso, b"12345").unwrap();
        let sig = path_sig(&iso).unwrap();
        assert!(!sig.dir && !sig.partial);
        assert_eq!((sig.size, sig.entries), (5, 0));
        assert!(sig.modified > 1_600_000_000);
        for end in PARTIAL {
            let twin = d.path().join(format!("ubuntu.iso{end}"));
            std::fs::write(&twin, b"").unwrap();
            assert!(path_sig(&iso).unwrap().partial, "{end}");
            std::fs::remove_file(&twin).unwrap();
        }
        assert!(!path_sig(&iso).unwrap().partial);
    }

    #[test]
    fn path_sig_of_a_folder_counts_its_top_level_only() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("album");
        std::fs::create_dir(&dir).unwrap();
        let empty = path_sig(&dir).unwrap();
        assert!(empty.dir && !empty.partial);
        assert_eq!((empty.size, empty.entries), (0, 0));
        std::fs::write(dir.join("a.mp3"), b"123").unwrap();
        std::fs::write(dir.join("b.mp3"), b"4567").unwrap();
        std::fs::create_dir(dir.join("covers")).unwrap();
        std::fs::write(dir.join("covers").join("front.jpg"), b"89").unwrap();
        let sig = path_sig(&dir).unwrap();
        assert_eq!((sig.size, sig.entries), (7, 3));
        assert!(!sig.partial);
        std::fs::write(dir.join("c.mp3.CRDOWNLOAD"), b"1").unwrap();
        let sig = path_sig(&dir).unwrap();
        assert!(sig.partial);
        assert_eq!((sig.size, sig.entries), (8, 4));
    }
}
