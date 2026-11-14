//! Reading the journal back: pages from the newest record to the oldest
//! across all the files, and the export of everything into one text file.
//!
//! A page ends with a mark for the next one. The mark names a place by the
//! generation of its file and the offset in it, so it stays right when the
//! files are renamed by a rotation between two pages. A reader holds the
//! `files` lock, which keeps a rotation away while it reads; writers go on.

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use super::line::{escape, format_line, split_line, Level, Row};
use super::scrub::scrub;
use super::sink::Journal;
use super::{files, JournalError};

/// Most records in one page.
pub const MAX_PAGE: usize = 500;
/// Records in a page when the query does not say (`limit` 0).
pub const DEFAULT_PAGE: usize = 100;

/// A file is read from its end in pieces of this size: the first page costs
/// one piece, not the whole file. A record is far shorter than a piece.
const WINDOW: u64 = 512 * 1024;
/// How far into a file the first record is looked for.
const OLDEST_WITHIN: u64 = 256 * 1024;

/// What to read. An empty list means "all"; an empty text means "any".
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct Query {
    pub levels: Vec<Level>,
    pub categories: Vec<String>,
    /// Looked for in the message, letter case ignored.
    pub text: String,
    /// `next` of the previous page; `None` starts from the newest record.
    pub before: Option<String>,
    /// 0 means `DEFAULT_PAGE`; more than `MAX_PAGE` means `MAX_PAGE`.
    pub limit: usize,
    /// Only records written at this time or later.
    #[serde(skip)]
    pub since: Option<DateTime<FixedOffset>>,
}

/// Records from the newest to the oldest and the mark for the next page.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Page {
    pub rows: Vec<Row>,
    /// `None` when there is nothing older that the query wants.
    pub next: Option<String>,
}

/// Where the next page starts: everything before this place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    /// A place in the files. `memory` is there when some records are in
    /// memory only (the file could not be written): of those, the ones with
    /// a number below it are still to come.
    File {
        generation: u64,
        offset: u64,
        memory: Option<u64>,
    },
    Memory {
        seq: u64,
    },
}

impl Mark {
    fn parse(text: &str) -> Option<Mark> {
        let hex = |digits: &str| u64::from_str_radix(digits, 16).ok();
        if let Some(seq) = text.strip_prefix('m') {
            return Some(Mark::Memory { seq: hex(seq)? });
        }
        let mut parts = text.strip_prefix('f')?.split('.');
        let mark = Mark::File {
            generation: hex(parts.next()?)?,
            offset: hex(parts.next()?)?,
            memory: match parts.next() {
                Some(seq) => Some(hex(seq)?),
                None => None,
            },
        };
        parts.next().is_none().then_some(mark)
    }

    fn text(self) -> String {
        match self {
            Mark::File {
                generation,
                offset,
                memory: None,
            } => format!("f{generation:x}.{offset:x}"),
            Mark::File {
                generation,
                offset,
                memory: Some(seq),
            } => format!("f{generation:x}.{offset:x}.{seq:x}"),
            Mark::Memory { seq } => format!("m{seq:x}"),
        }
    }
}

/// The filters of a query, ready to be applied to many lines.
struct Wanted<'a> {
    levels: &'a [Level],
    categories: &'a [String],
    needle: String,
    since: Option<DateTime<FixedOffset>>,
}

impl<'a> Wanted<'a> {
    fn new(query: &'a Query) -> Self {
        Self {
            levels: &query.levels,
            categories: &query.categories,
            needle: query.text.trim().to_lowercase(),
            since: query.since,
        }
    }

    fn level(&self, level: Level) -> bool {
        self.levels.is_empty() || self.levels.contains(&level)
    }

    fn category(&self, category: &str) -> bool {
        self.categories.is_empty() || self.categories.iter().any(|c| c == category)
    }

    fn text(&self, message: &str) -> bool {
        self.needle.is_empty() || message.to_lowercase().contains(&self.needle)
    }

    /// The record of a line of a file, if the line is a record and the query
    /// wants it. What leaves a file is scrubbed again: a secret that became
    /// known after the record was written does not get out.
    fn row(&self, line: &[u8]) -> Option<Row> {
        let fields = split_line(std::str::from_utf8(line).ok()?)?;
        if !self.level(fields.level) || !self.category(fields.category) {
            return None;
        }
        let time = fields.time()?;
        if self.since.is_some_and(|since| time < since) {
            return None;
        }
        let mut row = fields.row();
        if !self.text(&row.message) {
            return None;
        }
        row.message = scrub(&row.message);
        Some(row)
    }

    /// The same for a record in memory.
    fn wants(&self, row: &Row) -> bool {
        self.level(row.level)
            && self.category(&row.category)
            && self.text(&row.message)
            && self.since.is_none_or(|since| {
                DateTime::parse_from_rfc3339(&row.t).is_ok_and(|time| time >= since)
            })
    }
}

/// Walks the whole lines of the first `end` bytes of a file from the last one
/// to the first. `each` gets the offset of a line and the line without its
/// line feed, and says whether to go on. A last line without a line feed is
/// not whole (it is being written, or a crash cut it) and is skipped.
fn lines_back(
    path: &Path,
    end: Option<u64>,
    each: impl FnMut(u64, &[u8]) -> bool,
) -> io::Result<()> {
    lines_back_by(path, end, WINDOW, each)
}

/// `lines_back` with the size of a piece given.
fn lines_back_by(
    path: &Path,
    end: Option<u64>,
    window: u64,
    mut each: impl FnMut(u64, &[u8]) -> bool,
) -> io::Result<()> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut end = end.map_or(len, |end| end.min(len));
    while end > 0 {
        let start = end.saturating_sub(window);
        file.seek(SeekFrom::Start(start))?;
        let mut data = vec![0u8; (end - start) as usize];
        file.read_exact(&mut data)?;
        // A piece that does not start the file starts inside a line: that
        // line belongs to the piece before.
        let low = if start == 0 {
            0
        } else {
            match data.iter().position(|b| *b == b'\n') {
                Some(feed) => feed + 1,
                None => data.len(),
            }
        };
        let mut high = match data.iter().rposition(|b| *b == b'\n') {
            Some(feed) if feed + 1 > low => feed + 1,
            _ => low,
        };
        while high > low {
            let line_end = high - 1;
            let line_start = data[low..line_end]
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(low, |feed| low + feed + 1);
            if !each(start + line_start as u64, &data[line_start..line_end]) {
                return Ok(());
            }
            high = line_start;
        }
        // A line longer than a piece is not a record: step over it.
        let next = start + low as u64;
        end = if next < end { next } else { start };
    }
    Ok(())
}

/// The number of the record in memory whose line starts at this place.
fn seq_at(places: &[(u64, u64, u64)], generation: u64, offset: u64) -> Option<u64> {
    places
        .binary_search_by_key(&(generation, offset), |(g, o, _)| (*g, *o))
        .ok()
        .map(|found| places[found].2)
}

/// Time of the first record of the oldest file that has one.
pub(super) fn oldest(dir: &Path) -> Option<String> {
    oldest_first(dir).into_iter().find_map(|index| {
        let file = File::open(files::path(dir, index)).ok()?;
        let mut reader = BufReader::new(file.take(OLDEST_WITHIN));
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            let whole = line.strip_suffix(b"\n")?;
            let record = std::str::from_utf8(whole)
                .ok()
                .and_then(split_line)
                .filter(|fields| fields.time().is_some());
            if let Some(fields) = record {
                return Some(fields.t.to_string());
            }
        }
    })
}

/// Whether a folder given by the user is the folder of the journal, however
/// its path is spelled: another letter case, a short name, a link.
fn same_folder(given: Option<&Path>, dir: &Path) -> bool {
    let Some(given) = given else {
        return false;
    };
    // A relative name with no folder part means the working folder.
    let given = if given.as_os_str().is_empty() {
        Path::new(".")
    } else {
        given
    };
    if given == dir {
        return true;
    }
    match (std::fs::canonicalize(given), std::fs::canonicalize(dir)) {
        (Ok(given), Ok(dir)) => given == dir,
        // A folder that cannot be looked at is compared by its letters.
        _ => given
            .to_string_lossy()
            .eq_ignore_ascii_case(&dir.to_string_lossy()),
    }
}

/// Indexes of the files from the oldest to the current one.
fn oldest_first(dir: &Path) -> Vec<u32> {
    let mut indexes: Vec<u32> = files::earlier(dir)
        .into_iter()
        .map(|(index, _)| index)
        .collect();
    indexes.reverse();
    indexes.push(0);
    indexes
}

impl Journal {
    /// One page of records, the newest first. Pass `next` of a page as
    /// `before` of the query to get the page after it; every record comes
    /// once, also when files were rotated or records were written in between.
    /// A mark that cannot be read gives an empty page.
    pub fn read(&self, query: &Query) -> Page {
        let limit = match query.limit {
            0 => DEFAULT_PAGE,
            limit => limit.min(MAX_PAGE),
        };
        let wanted = Wanted::new(query);
        let before = match query.before.as_deref() {
            None => None,
            Some(text) => match Mark::parse(text) {
                Some(mark) => Some(mark),
                None => return Page::default(),
            },
        };
        match &self.dir {
            Some(dir) => self.read_files(dir, &wanted, before, limit),
            None => self.read_memory(&wanted, before, limit),
        }
    }

    fn read_files(
        &self,
        dir: &Path,
        wanted: &Wanted<'_>,
        before: Option<Mark>,
        limit: usize,
    ) -> Page {
        let _files = self.files.lock();
        let now = self.settled();
        let generation = now.generation;
        // The first page ends where the current file ended when the places
        // were taken. A record written after that moment is not among the
        // places: read here, it would come without its number, and the
        // reader that follows the tail would get it a second time.
        let (first, mut end, bound) = match before {
            None => (0, now.size, now.seq.saturating_add(1)),
            Some(Mark::File {
                generation: of_mark,
                offset,
                memory,
            }) if of_mark <= generation => {
                (generation - of_mark, Some(offset), memory.unwrap_or(0))
            }
            Some(_) => return Page::default(),
        };
        let top = files::earlier(dir)
            .last()
            .map_or(0, |(index, _)| u64::from(*index));
        // Where this page starts in the files: the mark of a page that
        // takes nothing from them.
        let mut place = (generation - first, end.unwrap_or(u64::MAX));
        let mut found: Vec<(Row, (u64, u64))> = Vec::new();
        let mut more = false;
        for index in first..=top.min(generation) {
            let of_file = generation - index;
            let until = end.take();
            let Ok(index) = u32::try_from(index) else {
                break;
            };
            // A file that is not there or cannot be read is stepped over.
            let _ = lines_back(&files::path(dir, index), until, |offset, line| {
                let Some(mut row) = wanted.row(line) else {
                    return true;
                };
                if found.len() == limit {
                    more = true;
                    return false;
                }
                row.seq = seq_at(&now.places, of_file, offset);
                found.push((row, (of_file, offset)));
                true
            });
            if more {
                break;
            }
        }
        // Records that are in memory only take their places among those of
        // the files by their numbers. A record of the files without a number
        // is older than everything in memory.
        let waiting = !now.unplaced.is_empty();
        let mut memory = now
            .unplaced
            .into_iter()
            .filter(|row| row.seq.is_some_and(|seq| seq < bound) && wanted.wants(row))
            .peekable();
        let mut found = found.into_iter().peekable();
        let mut bound = bound;
        let mut rows = Vec::new();
        loop {
            let from_memory = match (found.peek(), memory.peek()) {
                (None, None) => break,
                (Some(_), None) => false,
                (None, Some(_)) => true,
                (Some((in_file, _)), Some(in_memory)) => in_file.seq < in_memory.seq,
            };
            if rows.len() == limit {
                more = true;
                break;
            }
            if from_memory {
                if let Some(row) = memory.next() {
                    bound = row.seq.unwrap_or(bound);
                    rows.push(row);
                }
            } else if let Some((row, at)) = found.next() {
                place = at;
                rows.push(row);
            }
        }
        let next = more.then(|| {
            Mark::File {
                generation: place.0,
                offset: place.1,
                memory: waiting.then_some(bound),
            }
            .text()
        });
        Page { rows, next }
    }

    fn read_memory(&self, wanted: &Wanted<'_>, before: Option<Mark>, limit: usize) -> Page {
        let before = match before {
            None => u64::MAX,
            Some(Mark::Memory { seq }) => seq,
            Some(_) => return Page::default(),
        };
        let st = self.lock();
        let mut rows: Vec<Row> = Vec::new();
        let mut more = false;
        for row in st.rows_newest_first() {
            if row.seq.is_none_or(|seq| seq >= before) || !wanted.wants(row) {
                continue;
            }
            if rows.len() == limit {
                more = true;
                break;
            }
            rows.push(row.clone());
        }
        let next = rows
            .last()
            .and_then(|row| row.seq)
            .filter(|_| more)
            .map(|seq| Mark::Memory { seq }.text());
        Page { rows, next }
    }

    /// Writes the records the query wants into one text file, the oldest
    /// first, and tells how many. `None` takes every record; `before` and
    /// `limit` of a query are not used. Each line of `header` goes first,
    /// after `# `: the caller says there what it is a journal of.
    pub fn export(
        &self,
        query: Option<&Query>,
        header: &[String],
        dest: &Path,
    ) -> Result<usize, JournalError> {
        if let (Some(dir), Some(name)) = (&self.dir, dest.file_name().and_then(|n| n.to_str())) {
            if files::reserved(name) && same_folder(dest.parent(), dir) {
                return Err(JournalError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the journal cannot be exported over its own files",
                )));
            }
        }
        let everything = Query::default();
        let wanted = Wanted::new(query.unwrap_or(&everything));
        let mut out = BufWriter::new(File::create(dest)?);
        for part in header.iter().flat_map(|text| text.lines()) {
            writeln!(out, "# {}", escape(&scrub(part)))?;
        }
        let mut count = 0;
        match &self.dir {
            Some(dir) => {
                let _files = self.files.lock();
                self.flush();
                for index in oldest_first(dir) {
                    let Ok(file) = File::open(files::path(dir, index)) else {
                        continue;
                    };
                    let mut reader = BufReader::new(file);
                    let mut line = Vec::new();
                    loop {
                        line.clear();
                        match reader.read_until(b'\n', &mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                        let Some(whole) = line.strip_suffix(b"\n") else {
                            break;
                        };
                        if let Some(row) = wanted.row(whole) {
                            let text = format_line(&row.t, row.level, &row.category, &row.message);
                            out.write_all(text.as_bytes())?;
                            count += 1;
                        }
                    }
                }
                // What never reached a file: the latest records, as a rule.
                let mut unplaced = self.settled().unplaced;
                unplaced.reverse();
                for row in unplaced.iter().filter(|row| wanted.wants(row)) {
                    let text = format_line(&row.t, row.level, &row.category, &row.message);
                    out.write_all(text.as_bytes())?;
                    count += 1;
                }
            }
            None => {
                let rows: Vec<Row> = {
                    let st = self.lock();
                    let mut rows: Vec<Row> = st
                        .rows_newest_first()
                        .filter(|row| wanted.wants(row))
                        .cloned()
                        .collect();
                    rows.reverse();
                    rows
                };
                for row in rows {
                    let text = format_line(&row.t, row.level, &row.category, &row.message);
                    out.write_all(text.as_bytes())?;
                    count += 1;
                }
            }
        }
        out.flush()?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mark_round_trips_and_garbage_is_not_a_mark() {
        for mark in [
            Mark::File {
                generation: 0,
                offset: 0,
                memory: None,
            },
            Mark::File {
                generation: 17,
                offset: 5_242_880,
                memory: None,
            },
            Mark::File {
                generation: 3,
                offset: u64::MAX,
                memory: Some(2001),
            },
            Mark::Memory { seq: 2001 },
        ] {
            assert_eq!(Mark::parse(&mark.text()), Some(mark));
        }
        for text in [
            "", "f", "f1", "f1.", "f.1", "fz.1", "m", "mz", "12", "x1.2", "f1.2.", "f1.2.z",
            "f1.2.3.4",
        ] {
            assert_eq!(Mark::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_query_comes_from_the_interface_as_json() {
        let query: Query = serde_json::from_str(
            r#"{"levels":["error","warn"],"categories":["tool"],"text":"Disk","before":"f1.20","limit":50}"#,
        )
        .unwrap();
        assert_eq!(query.levels, [Level::Error, Level::Warn]);
        assert_eq!(query.categories, ["tool"]);
        assert_eq!(query.before.as_deref(), Some("f1.20"));
        assert_eq!((query.text.as_str(), query.limit), ("Disk", 50));
        let query: Query = serde_json::from_str(r#"{"before":null}"#).unwrap();
        assert_eq!(query, Query::default());
    }

    fn collect(path: &Path, end: Option<u64>) -> Vec<(u64, String)> {
        let mut lines = Vec::new();
        lines_back(path, end, |offset, line| {
            lines.push((offset, String::from_utf8_lossy(line).into_owned()));
            true
        })
        .unwrap();
        lines
    }

    #[test]
    fn lines_come_from_the_last_to_the_first_with_their_offsets() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f");
        std::fs::write(&path, "one\ntwo\n\nfour\nhalf").unwrap();
        assert_eq!(
            collect(&path, None),
            [
                (9, "four".to_string()),
                (8, String::new()),
                (4, "two".to_string()),
                (0, "one".to_string())
            ]
        );
        // Up to a place: only what stands before it.
        assert_eq!(
            collect(&path, Some(8)),
            [(4, "two".to_string()), (0, "one".to_string())]
        );
        assert!(collect(&path, Some(0)).is_empty());
        // A place past the end is the end.
        assert_eq!(collect(&path, Some(1000)).len(), 4);
        std::fs::write(&path, "no line feed at all").unwrap();
        assert!(collect(&path, None).is_empty());
        std::fs::write(&path, "").unwrap();
        assert!(collect(&path, None).is_empty());
        assert!(lines_back(&d.path().join("missing"), None, |_, _| true).is_err());
    }

    #[test]
    fn a_file_read_in_small_pieces_gives_the_same_lines() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f");
        let mut text = String::new();
        for n in 0..200 {
            text.push_str(&format!("line {n}{}\n", "x".repeat(n % 9)));
        }
        text.push_str("unfinished");
        std::fs::write(&path, &text).unwrap();
        let whole = collect(&path, None);
        assert_eq!(whole.len(), 200);
        assert_eq!(whole[0].1, "line 199x");
        assert_eq!(whole[199], (0, "line 0".to_string()));
        for window in [24, 25, 31, 64, 1000, 1 << 20] {
            let mut pieces = Vec::new();
            lines_back_by(&path, None, window, |offset, line| {
                pieces.push((offset, String::from_utf8_lossy(line).into_owned()));
                true
            })
            .unwrap();
            assert_eq!(pieces, whole, "window {window}");
        }
        // Every offset is where its line starts.
        for (offset, line) in &whole {
            let at = *offset as usize;
            assert_eq!(&text[at..at + line.len()], line);
            assert!(at == 0 || text.as_bytes()[at - 1] == b'\n');
        }
    }

    #[test]
    fn a_line_longer_than_a_piece_is_stepped_over() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f");
        let text = format!("first\n{}\nlast\n", "y".repeat(100));
        std::fs::write(&path, text).unwrap();
        let mut seen = Vec::new();
        lines_back_by(&path, None, 16, |_, line| {
            seen.push(String::from_utf8_lossy(line).into_owned());
            true
        })
        .unwrap();
        assert_eq!(seen, ["last", "first"]);
    }

    #[test]
    fn the_walk_stops_when_told() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("f");
        std::fs::write(&path, "1\n2\n3\n4\n").unwrap();
        let mut seen = Vec::new();
        lines_back(&path, None, |offset, _| {
            seen.push(offset);
            seen.len() < 2
        })
        .unwrap();
        assert_eq!(seen, [6, 4]);
    }
}
