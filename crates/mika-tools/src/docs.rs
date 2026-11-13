//! Questions about the user's own documents: the chosen folders are read into
//! pieces of text, with vectors when an embedding model is set, and the pieces
//! closest to a question go to the model with their file and page.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::memory::{cosine, decode_vector, encode_vector, Embedder};
use mika_core::storage::rusqlite::{self, params};
use mika_core::storage::Db;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::files::SKIP_DIRS;
use crate::util::{arg_u64, failed, schema, truncate};

/// Kinds of files read as plain text.
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "csv", "log", "json", "xml", "html", "htm", "srt",
];
/// Files looked at in all folders together.
const MAX_FILES: usize = 3000;
const MAX_DEPTH: usize = 8;
const MAX_TEXT_FILE: u64 = 5 * 1024 * 1024;
const MAX_DOCX: u64 = 30 * 1024 * 1024;
const MAX_PDF: u64 = 60 * 1024 * 1024;
/// Pages of one PDF read; each page is drawn and recognised, about a second.
const MAX_PDF_PAGES: u32 = 60;
/// Pieces kept in all: more would make each search slow and each index expensive.
const MAX_CHUNKS: usize = 8000;
/// Size of one piece, characters.
const CHUNK: usize = 1000;
const EMBED_BATCH: usize = 64;
/// Longest text sent for one vector, characters: pieces read by older versions
/// could be far longer than `CHUNK`, and the model refuses them.
const EMBED_CHARS: usize = 2000;
/// Known text that tells a model refusing some piece from one that answers nothing.
const PROBE: &str = "проверка";
/// Longest text unpacked from a DOCX, bytes.
const MAX_XML: u64 = 50 * 1024 * 1024;

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|x| x.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Whether the index reads files of this kind, and their size limit.
fn size_limit(p: &Path) -> Option<u64> {
    let ext = ext_of(p);
    match ext.as_str() {
        "pdf" => Some(MAX_PDF),
        "docx" => Some(MAX_DOCX),
        e if TEXT_EXTS.contains(&e) => Some(MAX_TEXT_FILE),
        _ => None,
    }
}

/// Text inside XML or HTML markup: tags dropped, entities decoded.
fn unescape_xml(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Text of a Word document body: paragraphs on their own lines.
pub fn docx_xml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    while let Some(open) = rest.find('<') {
        let close = match rest[open..].find('>') {
            Some(c) => open + c,
            None => break,
        };
        let tag = &rest[open + 1..close];
        rest = &rest[close + 1..];
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match name {
            "w:t" if !tag.starts_with('/') && !tag.ends_with('/') => {
                if let Some(end) = rest.find("</w:t>") {
                    out.push_str(&unescape_xml(&rest[..end]));
                    rest = &rest[end + "</w:t>".len()..];
                }
            }
            "w:p" if tag.starts_with('/') => out.push('\n'),
            "w:tab" => out.push('\t'),
            "w:br" | "w:cr" => out.push('\n'),
            _ => {}
        }
    }
    out
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// One file from a ZIP archive held in memory: stored or deflated.
pub fn zip_entry(zip: &[u8], wanted: &str) -> Result<Vec<u8>, String> {
    let broken = || "файл повреждён или это не документ Word".to_string();
    // The end record sits in the last 64 KB plus its own 22 bytes.
    let from = zip.len().saturating_sub(65_557);
    let eocd = (from..zip.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_at(zip, i) == Some(0x0605_4b50))
        .ok_or_else(broken)?;
    let count = u16_at(zip, eocd + 10).ok_or_else(broken)? as usize;
    let mut at = u32_at(zip, eocd + 16).ok_or_else(broken)? as usize;
    for _ in 0..count {
        if u32_at(zip, at) != Some(0x0201_4b50) {
            return Err(broken());
        }
        let method = u16_at(zip, at + 10).ok_or_else(broken)?;
        let packed = u32_at(zip, at + 20).ok_or_else(broken)? as usize;
        let name_len = u16_at(zip, at + 28).ok_or_else(broken)? as usize;
        let extra = u16_at(zip, at + 30).ok_or_else(broken)? as usize;
        let comment = u16_at(zip, at + 32).ok_or_else(broken)? as usize;
        let local = u32_at(zip, at + 42).ok_or_else(broken)? as usize;
        let name = zip.get(at + 46..at + 46 + name_len).ok_or_else(broken)?;
        if name == wanted.as_bytes() {
            if u32_at(zip, local) != Some(0x0403_4b50) {
                return Err(broken());
            }
            let n = u16_at(zip, local + 26).ok_or_else(broken)? as usize;
            let e = u16_at(zip, local + 28).ok_or_else(broken)? as usize;
            let start = local + 30 + n + e;
            let data = zip.get(start..start + packed).ok_or_else(broken)?;
            return match method {
                0 => Ok(data.to_vec()),
                8 => {
                    let mut out = Vec::new();
                    flate2::read::DeflateDecoder::new(data)
                        .take(MAX_XML)
                        .read_to_end(&mut out)
                        .map_err(|_| broken())?;
                    Ok(out)
                }
                _ => Err("документ сжат неизвестным способом".into()),
            };
        }
        at += 46 + name_len + extra + comment;
    }
    Err(broken())
}

/// Text of an HTML page: no scripts, styles or tags.
pub fn html_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    // ASCII only: byte positions must stay the same as in `html`.
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < html.len() {
        let Some(open) = html[i..].find('<').map(|o| i + o) else {
            out.push_str(&html[i..]);
            break;
        };
        out.push_str(&html[i..open]);
        let after = &lower[open..];
        let skip_to = ["script", "style"]
            .iter()
            .find(|t| after.starts_with(&format!("<{t}")))
            .and_then(|t| lower[open..].find(&format!("</{t}>")).map(|e| open + e));
        let from = skip_to.unwrap_or(open);
        match html[from..].find('>') {
            Some(c) => {
                let tag = &lower[open..from + c];
                if tag.starts_with("<p")
                    || tag.starts_with("<br")
                    || tag.starts_with("<div")
                    || tag.starts_with("<li")
                    || tag.starts_with("<h")
                    || tag.starts_with("<tr")
                {
                    out.push('\n');
                }
                i = from + c + 1;
            }
            None => break,
        }
    }
    unescape_xml(&out)
}

/// Windows-1251 letters and signs from 0x80 to 0xBF; 0xC0..0xFF are А..я in order.
const CP1251_HIGH: [char; 64] = [
    '\u{0402}', '\u{0403}', '\u{201A}', '\u{0453}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{20AC}', '\u{2030}', '\u{0409}', '\u{2039}', '\u{040A}', '\u{040C}', '\u{040B}', '\u{040F}',
    '\u{0452}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{0098}', '\u{2122}', '\u{0459}', '\u{203A}', '\u{045A}', '\u{045C}', '\u{045B}', '\u{045F}',
    '\u{00A0}', '\u{040E}', '\u{045E}', '\u{0408}', '\u{00A4}', '\u{0490}', '\u{00A6}', '\u{00A7}',
    '\u{0401}', '\u{00A9}', '\u{0404}', '\u{00AB}', '\u{00AC}', '\u{00AD}', '\u{00AE}', '\u{0407}',
    '\u{00B0}', '\u{00B1}', '\u{0406}', '\u{0456}', '\u{0491}', '\u{00B5}', '\u{00B6}', '\u{00B7}',
    '\u{0451}', '\u{2116}', '\u{0454}', '\u{00BB}', '\u{0458}', '\u{0405}', '\u{0455}', '\u{0457}',
];

fn cp1251(b: u8) -> char {
    match b {
        0..=0x7F => b as char,
        0x80..=0xBF => CP1251_HIGH[(b - 0x80) as usize],
        _ => char::from_u32(0x0410 + (b - 0xC0) as u32).unwrap_or('\u{FFFD}'),
    }
}

/// Whether bytes are UTF-8, maybe with stray damage: more letters beyond ASCII
/// decode than bytes fail. Russian in Windows-1251 fails on nearly every letter.
fn mostly_utf8(bytes: &[u8]) -> bool {
    let (mut good, mut bad) = (0usize, 0usize);
    for chunk in bytes.utf8_chunks() {
        good += chunk.valid().chars().filter(|c| !c.is_ascii()).count();
        bad += chunk.invalid().len();
    }
    bad == 0 || good > bad
}

/// Text of a plain file. A file that is not UTF-8 is read as Windows-1251:
/// that is how Russian Excel and 1C save CSV and text.
fn plain_text(bytes: &[u8]) -> Option<String> {
    let text = crate::search::read_text(bytes)?;
    if bytes.starts_with(&[0xFF, 0xFE])
        || bytes.starts_with(&[0xEF, 0xBB, 0xBF])
        || mostly_utf8(bytes)
    {
        return Some(text);
    }
    Some(bytes.iter().map(|&b| cp1251(b)).collect())
}

/// Pages of a document: (page number from 1, or 0 for a file without pages, text).
pub fn extract(path: &Path) -> Result<Vec<(u32, String)>, String> {
    let ext = ext_of(path);
    match ext.as_str() {
        "pdf" => Ok(crate::win::pdf::pages_text(path, MAX_PDF_PAGES)?
            .into_iter()
            .enumerate()
            .map(|(i, t)| (i as u32 + 1, t))
            .collect()),
        "docx" => {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let xml = zip_entry(&bytes, "word/document.xml")?;
            Ok(vec![(0, docx_xml_text(&String::from_utf8_lossy(&xml)))])
        }
        _ => {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let text = plain_text(&bytes).ok_or("в файле не текст")?;
            let text = if ext.starts_with("htm") {
                html_text(&text)
            } else {
                text
            };
            Ok(vec![(0, text)])
        }
    }
}

/// Pieces of about `CHUNK` characters along paragraph borders.
pub fn chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0;
    let flush = |cur: &mut String, cur_len: &mut usize, out: &mut Vec<String>| {
        let t = cur.trim();
        if t.chars().filter(|c| c.is_alphanumeric()).count() >= 20 {
            out.push(t.to_string());
        }
        cur.clear();
        *cur_len = 0;
    };
    for para in text.split('\n') {
        let para = para.split_whitespace().collect::<Vec<_>>().join(" ");
        if para.is_empty() {
            continue;
        }
        let len = para.chars().count();
        if cur_len + len > CHUNK && cur_len > 0 {
            flush(&mut cur, &mut cur_len, &mut out);
        }
        if len > CHUNK {
            // A long paragraph is cut at spaces, and a word longer than a piece
            // (base64, a long link) by characters.
            for mut word in para.split(' ') {
                loop {
                    let (part, rest) = match word.char_indices().nth(CHUNK) {
                        Some((at, _)) => word.split_at(at),
                        None => (word, ""),
                    };
                    let w = part.chars().count();
                    if cur_len + w + 1 > CHUNK && cur_len > 0 {
                        flush(&mut cur, &mut cur_len, &mut out);
                    }
                    cur.push_str(part);
                    cur.push(' ');
                    cur_len += w + 1;
                    if rest.is_empty() {
                        break;
                    }
                    word = rest;
                }
            }
        } else {
            cur.push_str(&para);
            cur.push('\n');
            cur_len += len + 1;
        }
    }
    flush(&mut cur, &mut cur_len, &mut out);
    out
}

/// What a walk over the folders found.
struct Walk {
    /// Supported files: (path, modified unix seconds, size).
    files: Vec<(PathBuf, i64, u64)>,
    /// Folders that did not open: a network drive away, a stick out.
    unread: Vec<PathBuf>,
}

/// Whether the path lies in one of the folders.
fn under(path: &Path, folders: &[PathBuf]) -> bool {
    folders.iter().any(|f| path.starts_with(f))
}

fn walk(folders: &[PathBuf]) -> Walk {
    fn go(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, i64, u64)>, unread: &mut Vec<PathBuf>) {
        if out.len() >= MAX_FILES {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            unread.push(dir.to_path_buf());
            return;
        };
        let mut dirs = Vec::new();
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                let name = e.file_name().to_string_lossy().to_lowercase();
                if depth > 0 && !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) {
                    dirs.push(p);
                }
                continue;
            }
            let Some(limit) = size_limit(&p) else {
                continue;
            };
            // Office keeps `~$name.docx` while a document is open.
            if meta.len() == 0
                || meta.len() > limit
                || e.file_name().to_string_lossy().starts_with("~$")
            {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64);
            out.push((p, modified, meta.len()));
            if out.len() >= MAX_FILES {
                return;
            }
        }
        for d in dirs {
            go(&d, depth - 1, out, unread);
        }
    }
    let (mut files, mut unread) = (Vec::new(), Vec::new());
    for f in folders {
        go(f, MAX_DEPTH, &mut files, &mut unread);
    }
    files.sort();
    files.dedup_by(|a, b| a.0 == b.0);
    Walk { files, unread }
}

/// Folders chosen right now.
type Folders<'a> = dyn Fn() -> Vec<PathBuf> + Send + Sync + 'a;

/// Where the index stands, for the settings page.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IndexStatus {
    pub files: usize,
    pub chunks: usize,
    /// Pieces that have a vector of the current model.
    pub embedded: usize,
    pub running: bool,
    /// Unix seconds of the last finished run.
    pub last_run: Option<i64>,
    pub error: Option<String>,
    /// Files read in the running pass and how many it found.
    pub progress: Option<(usize, usize)>,
}

/// One found piece.
#[derive(Debug, Clone, PartialEq)]
pub struct DocHit {
    pub path: String,
    pub page: u32,
    pub text: String,
    pub score: f32,
}

#[derive(Clone)]
pub struct DocIndex {
    db: Arc<Db>,
    embedder: Option<Arc<dyn Embedder>>,
    state: Arc<Mutex<IndexStatus>>,
    running: Arc<AtomicBool>,
    /// Files that did not fit: such a file is not read again while there is
    /// still no room for it.
    too_big: Arc<Mutex<HashMap<String, TooBig>>>,
    max_chunks: usize,
}

/// A file that did not fit: its (modified, size) and how many pieces it needs.
type TooBig = ((i64, i64), usize);

/// Lower case word stems: the first five letters, so «договора» finds «договор».
fn stems(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.chars().take(5).collect())
        .collect()
}

fn db_err(e: impl std::fmt::Display) -> String {
    format!("индекс документов недоступен: {e}")
}

impl DocIndex {
    pub fn new(db: Arc<Db>, embedder: Option<Arc<dyn Embedder>>) -> Self {
        Self {
            db,
            embedder,
            state: Default::default(),
            running: Default::default(),
            too_big: Default::default(),
            max_chunks: MAX_CHUNKS,
        }
    }

    fn model(&self) -> Option<(Arc<dyn Embedder>, String)> {
        let e = self.embedder.clone()?;
        let m = e.model().filter(|m| !m.trim().is_empty())?;
        Some((e, m))
    }

    pub fn status(&self) -> IndexStatus {
        let mut s = self.state.lock().clone();
        let model = self.model().map(|m| m.1).unwrap_or_default();
        let counts = self.db.with(|c| {
            let files: i64 = c.query_row("SELECT COUNT(*) FROM doc_files", [], |r| r.get(0))?;
            let chunks: i64 = c.query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| r.get(0))?;
            let embedded: i64 = c.query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE model = ?1",
                [&model],
                |r| r.get(0),
            )?;
            Ok((files as usize, chunks as usize, embedded as usize))
        });
        if let Ok((files, chunks, embedded)) = counts {
            s.files = files;
            s.chunks = chunks;
            s.embedded = embedded;
        }
        s.running = self.running.load(Ordering::Relaxed);
        s
    }

    pub fn clear(&self) -> Result<(), String> {
        self.too_big.lock().clear();
        self.db
            .with(|c| c.execute_batch("DELETE FROM doc_chunks; DELETE FROM doc_files;"))
            .map_err(db_err)
    }

    fn count_chunks(&self, path: Option<&str>) -> Result<usize, String> {
        self.db
            .with(|c| match path {
                Some(p) => c.query_row(
                    "SELECT COUNT(*) FROM doc_chunks WHERE path = ?1",
                    [p],
                    |r| r.get::<_, i64>(0),
                ),
                None => c.query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| r.get(0)),
            })
            .map(|n| n as usize)
            .map_err(db_err)
    }

    /// Drops files and their pieces, in one transaction.
    fn forget<S: AsRef<str>>(&self, paths: &[S]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        self.db
            .with(|c| {
                let tx = c.unchecked_transaction()?;
                for p in paths {
                    tx.execute("DELETE FROM doc_chunks WHERE path = ?1", [p.as_ref()])?;
                    tx.execute("DELETE FROM doc_files WHERE path = ?1", [p.as_ref()])?;
                }
                tx.commit()
            })
            .map_err(db_err)
    }

    /// Follows the folders chosen right now: when they changed since `seen`,
    /// what lies outside them leaves the index at once. Returns whether anything left.
    fn follow(&self, current: &Folders<'_>, seen: &mut Vec<PathBuf>) -> Result<bool, String> {
        let now = current();
        if now == *seen {
            return Ok(false);
        }
        *seen = now;
        let paths: Vec<String> = self
            .db
            .with(|c| {
                let mut st =
                    c.prepare("SELECT path FROM doc_files UNION SELECT path FROM doc_chunks")?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()
            })
            .map_err(db_err)?;
        let outside: Vec<&String> = paths
            .iter()
            .filter(|p| !under(Path::new(p), seen))
            .collect();
        self.forget(&outside)?;
        Ok(!outside.is_empty())
    }

    /// Reads new and changed files, drops gone ones, makes missing vectors.
    /// Returns a short report. A second call while one runs does nothing.
    pub async fn run(
        &self,
        folders: &[PathBuf],
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        self.run_with(|| folders.to_vec(), cancel).await
    }

    /// Like `run`, but `current` gives the folders chosen right now. It is asked
    /// again before each file and each batch of vectors: a folder removed from
    /// the settings mid-pass is not read further, its pieces leave the index,
    /// and none of them goes to the embedding model.
    pub async fn run_with(
        &self,
        current: impl Fn() -> Vec<PathBuf> + Send + Sync,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        if self.running.swap(true, Ordering::Relaxed) {
            return Err("индекс уже обновляется".into());
        }
        let result = self.pass(&current, cancel).await;
        self.running.store(false, Ordering::Relaxed);
        let mut s = self.state.lock();
        s.progress = None;
        s.last_run = Some(chrono::Local::now().timestamp());
        s.error = result.as_ref().err().cloned();
        result
    }

    async fn pass(
        &self,
        current: &Folders<'_>,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let mut seen = current();
        let roots = seen.clone();
        let walked = tokio::task::spawn_blocking(move || walk(&roots))
            .await
            .map_err(|e| e.to_string())?;
        let found = &walked.files;
        let known: HashMap<String, (i64, i64)> = self
            .db
            .with(|c| {
                let mut st = c.prepare("SELECT path, modified, size FROM doc_files")?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?;
                rows.collect()
            })
            .map_err(db_err)?;
        let here: HashSet<String> = found
            .iter()
            .map(|f| f.0.to_string_lossy().into_owned())
            .collect();
        // A folder that did not open is not a folder emptied: what the index
        // has from it stays until it opens again or leaves the settings.
        let gone: Vec<&String> = known
            .keys()
            .filter(|p| !here.contains(*p) && !under(Path::new(p), &walked.unread))
            .collect();
        self.forget(&gone)?;
        self.too_big.lock().retain(|p, _| here.contains(p));
        let mut todo: Vec<&(PathBuf, i64, u64)> = found
            .iter()
            .filter(|(p, m, s)| known.get(p.to_string_lossy().as_ref()) != Some(&(*m, *s as i64)))
            .collect();
        // Changed files first: they already hold room, new ones take what is left.
        todo.sort_by_cached_key(|(p, _, _)| !known.contains_key(p.to_string_lossy().as_ref()));
        let mut chunk_count = self.count_chunks(None)?;
        let (mut read, mut failed_files, mut full) = (0, 0, false);
        for (i, (path, modified, size)) in todo.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            self.state.lock().progress = Some((i, todo.len()));
            if self.follow(current, &mut seen)? {
                chunk_count = self.count_chunks(None)?;
            }
            if !under(path, &seen) {
                continue;
            }
            let key = path.to_string_lossy().into_owned();
            let stamp = (*modified, *size as i64);
            let old = self.count_chunks(Some(key.as_str()))?;
            let need = self
                .too_big
                .lock()
                .get(&key)
                .filter(|t| t.0 == stamp)
                .map(|t| t.1);
            if need.is_some_and(|n| chunk_count.saturating_sub(old) + n > self.max_chunks) {
                full = true;
                continue;
            }
            let p = path.clone();
            let pages = tokio::task::spawn_blocking(move || extract(&p))
                .await
                .map_err(|e| e.to_string())?;
            let pieces: Vec<(u32, String)> = match pages {
                Ok(pages) => pages
                    .into_iter()
                    .flat_map(|(page, text)| chunks(&text).into_iter().map(move |t| (page, t)))
                    .collect(),
                Err(e) => {
                    // The file stays as it was and is read again next pass:
                    // the reason may go away (a recognition language, a lock).
                    log::info!("document not read: {e}");
                    failed_files += 1;
                    continue;
                }
            };
            // The folder may have left the settings while the file was read.
            if self.follow(current, &mut seen)? {
                chunk_count = self.count_chunks(None)?;
            }
            if !under(path, &seen) {
                continue;
            }
            if chunk_count.saturating_sub(old) + pieces.len() > self.max_chunks {
                full = true;
                self.too_big
                    .lock()
                    .insert(key.clone(), (stamp, pieces.len()));
                if old > 0 {
                    // The old text of a changed file must not answer as its text now.
                    self.forget(&[&key])?;
                    chunk_count = chunk_count.saturating_sub(old);
                }
                continue;
            }
            self.too_big.lock().remove(&key);
            // One transaction: one write to disk instead of one per piece, and
            // a file is either all in the index or as it was.
            self.db
                .with(|c| {
                    let tx = c.unchecked_transaction()?;
                    tx.execute("DELETE FROM doc_chunks WHERE path = ?1", [&key])?;
                    {
                        let mut st = tx.prepare(
                            "INSERT INTO doc_chunks (path, page, text) VALUES (?1, ?2, ?3)",
                        )?;
                        for (page, text) in &pieces {
                            st.execute(params![key, page, text])?;
                        }
                    }
                    tx.execute(
                        "INSERT OR REPLACE INTO doc_files (path, modified, size) VALUES (?1, ?2, ?3)",
                        params![key, stamp.0, stamp.1],
                    )?;
                    tx.commit()
                })
                .map_err(db_err)?;
            chunk_count = chunk_count.saturating_sub(old) + pieces.len();
            read += 1;
        }
        let (embedded, refused) = self.embed_missing(current, &mut seen, cancel).await?;
        let mut report = format!(
            "Файлов в папках: {}, прочитано заново: {read}, убрано: {}.",
            found.len(),
            gone.len()
        );
        if failed_files > 0 {
            report.push_str(&format!(" Не прочитались: {failed_files}."));
        }
        if embedded > 0 {
            report.push_str(&format!(" Векторов построено: {embedded}."));
        }
        if refused > 0 {
            report.push_str(&format!(" Модель не приняла кусков: {refused}."));
        }
        if full {
            report.push_str(&format!(
                " Индекс полон ({} кусков): остальные файлы не вошли.",
                self.max_chunks
            ));
        }
        Ok(report)
    }

    /// Vectors for pieces that have none of the current model: how many were
    /// made, and how many pieces the model refused (they are tried next pass).
    async fn embed_missing(
        &self,
        current: &Folders<'_>,
        seen: &mut Vec<PathBuf>,
        cancel: &CancellationToken,
    ) -> Result<(usize, usize), String> {
        let Some((e, model)) = self.model() else {
            return Ok((0, 0));
        };
        let mut done = 0;
        // Refused pieces do not hold back the rest of this pass.
        let mut refused: HashSet<i64> = HashSet::new();
        loop {
            if cancel.is_cancelled() {
                break;
            }
            // A folder removed mid-pass leaves before anything more is sent.
            self.follow(current, seen)?;
            let rows: Vec<(i64, String)> = self
                .db
                .with(|c| {
                    let mut st = c.prepare(
                        "SELECT id, text FROM doc_chunks WHERE model IS NULL OR model != ?1 ORDER BY id LIMIT ?2",
                    )?;
                    let rows = st.query_map(
                        params![model, (EMBED_BATCH + refused.len()) as i64],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    rows.collect()
                })
                .map_err(db_err)?;
            let batch: Vec<(i64, String)> = rows
                .into_iter()
                .filter(|(id, _)| !refused.contains(id))
                .take(EMBED_BATCH)
                .collect();
            if batch.is_empty() {
                break;
            }
            let texts: Vec<String> = batch
                .iter()
                .map(|b| b.1.chars().take(EMBED_CHARS).collect())
                .collect();
            let vectors: Vec<Option<Vec<f32>>> = match e.embed(&texts).await {
                Ok(v) if v.len() == texts.len() => v.into_iter().map(Some).collect(),
                Ok(_) => return Err("ответ модели эмбеддингов не той длины".into()),
                Err(err) => {
                    // A model that answers nothing stops the pass; one that
                    // refuses some piece gets the pieces one by one.
                    if e.embed(&[PROBE.to_string()]).await.is_err() {
                        return Err(err);
                    }
                    let mut one_by_one = Vec::with_capacity(texts.len());
                    for t in &texts {
                        if cancel.is_cancelled() {
                            break;
                        }
                        one_by_one.push(match e.embed(std::slice::from_ref(t)).await {
                            Ok(mut v) if v.len() == 1 => v.pop(),
                            _ => None,
                        });
                    }
                    one_by_one
                }
            };
            self.db
                .with(|c| {
                    let tx = c.unchecked_transaction()?;
                    {
                        let mut st = tx.prepare(
                            "UPDATE doc_chunks SET model = ?2, embedding = ?3 WHERE id = ?1",
                        )?;
                        for ((id, _), v) in batch.iter().zip(&vectors) {
                            if let Some(v) = v {
                                st.execute(params![id, model, encode_vector(v)])?;
                            }
                        }
                    }
                    tx.commit()
                })
                .map_err(db_err)?;
            for ((id, _), v) in batch.iter().zip(&vectors) {
                match v {
                    Some(_) => done += 1,
                    None => {
                        refused.insert(*id);
                    }
                }
            }
        }
        Ok((done, refused.len()))
    }

    /// The pieces closest to a question: by meaning with vectors, else by words.
    /// Pieces still without a vector (the first pass goes on, the model refused
    /// them, the model changed) are found by their words alongside.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<DocHit>, String> {
        if let Some((e, model)) = self.model() {
            type Rows = (
                Vec<(String, u32, String, Vec<u8>)>,
                Vec<(String, u32, String)>,
            );
            let (rows, rest): Rows = self
                .db
                .with(|c| {
                    let mut st = c.prepare(
                        "SELECT path, page, text, embedding FROM doc_chunks WHERE model = ?1",
                    )?;
                    let rows = st
                        .query_map([&model], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                        })?
                        .collect::<rusqlite::Result<_>>()?;
                    let mut st = c.prepare(
                        "SELECT path, page, text FROM doc_chunks WHERE model IS NULL OR model != ?1",
                    )?;
                    let rest = st
                        .query_map([&model], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                        .collect::<rusqlite::Result<_>>()?;
                    Ok((rows, rest))
                })
                .map_err(db_err)?;
            if !rows.is_empty() {
                match e.embed(&[query.to_string()]).await {
                    Ok(q) if q.len() == 1 => {
                        let q = &q[0];
                        let words = stems(query);
                        let mut hits: Vec<DocHit> = rows
                            .into_iter()
                            .map(|(path, page, text, v)| {
                                // Exact words lift a piece a little: names and numbers.
                                let lower = text.to_lowercase();
                                let bonus =
                                    words.iter().filter(|w| lower.contains(w.as_str())).count()
                                        as f32
                                        * 0.02;
                                let score = cosine(q, &decode_vector(&v)) + bonus;
                                DocHit {
                                    path,
                                    page,
                                    text,
                                    score,
                                }
                            })
                            .collect();
                        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
                        hits.truncate(limit);
                        // Scores of the two kinds do not compare: they take turns.
                        let by_words = word_hits(query, rest, limit);
                        return Ok(interleave(hits, by_words, limit));
                    }
                    Ok(_) => log::warn!("embedding answer has the wrong size"),
                    Err(err) => log::warn!("document search by meaning failed, using words: {err}"),
                }
            }
        }
        self.search_words(query, limit)
    }

    fn search_words(&self, query: &str, limit: usize) -> Result<Vec<DocHit>, String> {
        if stems(query).is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(String, u32, String)> = self
            .db
            .with(|c| {
                let mut st = c.prepare("SELECT path, page, text FROM doc_chunks")?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect()
            })
            .map_err(db_err)?;
        Ok(word_hits(query, rows, limit))
    }
}

/// Pieces that hold at least half the words of the question, best first.
fn word_hits(query: &str, rows: Vec<(String, u32, String)>, limit: usize) -> Vec<DocHit> {
    let words = stems(query);
    if words.is_empty() {
        return Vec::new();
    }
    let need = words.len().div_ceil(2);
    let mut hits: Vec<DocHit> = rows
        .into_iter()
        .filter_map(|(path, page, text)| {
            let lower = text.to_lowercase();
            let present = words.iter().filter(|w| lower.contains(w.as_str())).count();
            if present < need {
                return None;
            }
            let total: usize = words
                .iter()
                .map(|w| lower.matches(w.as_str()).count().min(5))
                .sum();
            Some(DocHit {
                path,
                page,
                text,
                score: present as f32 + total as f32 * 0.1,
            })
        })
        .collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(limit);
    hits
}

/// Two lists in turns, the first one leading, up to `limit`.
fn interleave(first: Vec<DocHit>, second: Vec<DocHit>, limit: usize) -> Vec<DocHit> {
    let mut out = Vec::with_capacity(limit);
    let (mut a, mut b) = (first.into_iter(), second.into_iter());
    while out.len() < limit {
        let (x, y) = (a.next(), b.next());
        if x.is_none() && y.is_none() {
            break;
        }
        out.extend(x);
        if out.len() < limit {
            out.extend(y);
        }
    }
    out
}

pub struct DocsSearchTool(pub DocIndex);

#[async_trait]
impl Tool for DocsSearchTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "docs_search",
            "Найти ответ в документах пользователя (папки выбраны в настройках: договоры, инструкции, заметки, PDF, Word). Возвращает отрывки с файлом и страницей; отвечай по ним и называй файл.",
            schema(
                &[
                    ("query", "string", "Вопрос или слова для поиска"),
                    ("limit", "integer", "Сколько отрывков, по умолчанию 5"),
                ],
                &["query"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let q = arg_str(args, "query")?;
        if q.trim().is_empty() {
            return Err(ToolError::BadArguments("пустой вопрос".into()));
        }
        Ok(Assessment::new(
            RiskLevel::Low,
            "Поискать в документах",
            truncate(q, 200),
            "только чтение выбранных папок",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let query = arg_str(&args, "query")?;
        let limit = arg_u64(&args, "limit").unwrap_or(5).clamp(1, 10) as usize;
        let status = self.0.status();
        if status.files == 0 {
            return Ok(ToolOutput::new(if status.running {
                "Документы ещё читаются, попробуй через минуту."
            } else {
                "Папки с документами не выбраны: их можно добавить в настройках, раздел «Инструменты» → «Документы»."
            }));
        }
        let hits = self.0.search(query, limit).await.map_err(failed)?;
        if hits.is_empty() {
            return Ok(ToolOutput::new(format!(
                "В документах ничего не нашлось по запросу «{}».",
                query.trim()
            )));
        }
        let parts: Vec<String> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let name = Path::new(&h.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| h.path.clone());
                let page = if h.page > 0 {
                    format!(", стр. {}", h.page)
                } else {
                    String::new()
                };
                format!(
                    "[{}] {name}{page} — {}\n{}",
                    i + 1,
                    h.path,
                    truncate(&h.text, 1000)
                )
            })
            .collect();
        Ok(ToolOutput::new(parts.join("\n\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use serde_json::json;
    use std::io::Write;

    /// A ZIP with one deflated file, written by hand like Word does.
    fn zip_with(name: &str, body: &[u8]) -> Vec<u8> {
        let mut packed = Vec::new();
        {
            let mut enc =
                flate2::write::DeflateEncoder::new(&mut packed, flate2::Compression::default());
            enc.write_all(body).unwrap();
            enc.finish().unwrap();
        }
        let mut z = Vec::new();
        z.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        z.extend_from_slice(&[20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(name.as_bytes());
        z.extend_from_slice(&packed);
        let central = z.len();
        z.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        z.extend_from_slice(&[20, 0, 20, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&(packed.len() as u32).to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&[0; 12]);
        z.extend_from_slice(&0u32.to_le_bytes());
        z.extend_from_slice(name.as_bytes());
        let size = z.len() - central;
        z.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        z.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend_from_slice(&(size as u32).to_le_bytes());
        z.extend_from_slice(&(central as u32).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z
    }

    const BODY: &str = r#"<?xml version="1.0"?><w:document><w:body><w:p><w:r><w:t>Договор №5</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Штраф за просрочку &lt;1%&gt;</w:t><w:tab/><w:t>в день</w:t></w:r></w:p></w:body></w:document>"#;

    #[test]
    fn word_text_comes_out_of_the_archive() {
        let zip = zip_with("word/document.xml", BODY.as_bytes());
        let xml = zip_entry(&zip, "word/document.xml").unwrap();
        let text = docx_xml_text(&String::from_utf8(xml).unwrap());
        assert_eq!(text, "Договор №5\nШтраф за просрочку <1%>\tв день\n");
        assert!(zip_entry(&zip, "word/other.xml").is_err());
        assert!(zip_entry(b"not a zip at all, just text", "x").is_err());
    }

    #[test]
    fn html_and_pieces() {
        let t = html_text("<html><style>p{}</style><script>alert(1)</script><p>Привет&nbsp;&amp; пока</p><br>Ещё</html>");
        assert!(
            t.contains("Привет  & пока") || t.contains("Привет & пока"),
            "{t:?}"
        );
        assert!(
            !t.contains("alert") && !t.contains("p{}") && t.contains("Ещё"),
            "{t:?}"
        );
        let long = "Первое предложение про договор. ".repeat(80);
        let parts = chunks(&format!("Заголовок раздела номер один\n\n{long}\nкоротко"));
        assert!(parts.len() >= 3, "{}", parts.len());
        assert!(parts.iter().all(|p| p.chars().count() <= CHUNK + 40));
        assert!(chunks("а\nб\n").is_empty());
    }

    #[tokio::test]
    async fn folders_are_read_changed_files_reread_and_gone_ones_dropped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("договор.txt"),
            "Штраф за просрочку оплаты составляет 1% в день от суммы договора.",
        )
        .unwrap();
        std::fs::write(
            d.path().join("рецепт.docx"),
            zip_with(
                "word/document.xml",
                "<w:p><w:t>Борщ: свёкла, капуста, картошка и много любви к готовке.</w:t></w:p>"
                    .as_bytes(),
            ),
        )
        .unwrap();
        std::fs::write(d.path().join("картинка.png"), b"png").unwrap();
        std::fs::create_dir(d.path().join("node_modules")).unwrap();
        std::fs::write(
            d.path().join("node_modules").join("x.txt"),
            "служебный файл, который не нужен никому вообще",
        )
        .unwrap();
        let index = DocIndex::new(Arc::new(Db::open_in_memory().unwrap()), None);
        let cancel = CancellationToken::new();
        let folders = vec![d.path().to_path_buf()];
        let report = index.run(&folders, &cancel).await.unwrap();
        assert!(
            report.starts_with("Файлов в папках: 2, прочитано заново: 2"),
            "{report}"
        );
        let s = index.status();
        assert_eq!((s.files, s.chunks, s.running), (2, 2, false));
        let again = index.run(&folders, &cancel).await.unwrap();
        assert!(again.contains("прочитано заново: 0"), "{again}");
        let hits = index.search("какой штраф по договору", 3).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("договор.txt"));
        std::fs::remove_file(d.path().join("договор.txt")).unwrap();
        let after = index.run(&folders, &cancel).await.unwrap();
        assert!(after.contains("убрано: 1"), "{after}");
        assert!(index.search("штраф", 3).await.unwrap().is_empty());

        let tool = DocsSearchTool(index.clone());
        let out = run(&tool, json!({"query": "борщ капуста"}))
            .await
            .unwrap()
            .text;
        assert!(out.starts_with("[1] рецепт.docx — "), "{out}");
        index.clear().unwrap();
        let none = run(&tool, json!({"query": "борщ"})).await.unwrap().text;
        assert!(none.contains("не выбраны"), "{none}");
    }

    /// Two axes of meaning: fines and soup.
    fn axes(texts: &[String]) -> Vec<Vec<f32>> {
        texts
            .iter()
            .map(|t| {
                let t = t.to_lowercase();
                let has = |w: &[&str]| w.iter().any(|x| t.contains(x)) as u8 as f32;
                vec![
                    has(&["штраф", "неустойк", "пени"]),
                    has(&["борщ", "суп", "еда"]),
                ]
            })
            .collect()
    }

    struct Axes;

    #[async_trait]
    impl Embedder for Axes {
        fn model(&self) -> Option<String> {
            Some("axes-1".into())
        }
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            Ok(axes(texts))
        }
    }

    /// Like a real provider: refuses a batch with an overlong piece or with
    /// «ОТКАЗ» in it, answers nothing while `dead`, and keeps what it was sent.
    #[derive(Default)]
    struct Picky {
        sent: Mutex<Vec<String>>,
        dead: AtomicBool,
    }

    #[async_trait]
    impl Embedder for Picky {
        fn model(&self) -> Option<String> {
            Some("axes-1".into())
        }
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            if self.dead.load(Ordering::Relaxed) {
                return Err("сеть недоступна".into());
            }
            self.sent.lock().extend(texts.iter().cloned());
            if texts
                .iter()
                .any(|t| t.chars().count() > 3000 || t.contains("ОТКАЗ"))
            {
                return Err("400: input refused".into());
            }
            Ok(axes(texts))
        }
    }

    fn modified_secs(p: &Path) -> i64 {
        std::fs::metadata(p)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// One paragraph per word, each a piece of its own.
    fn paras(words: &[&str]) -> String {
        words
            .iter()
            .map(|w| format!("{w} ").repeat(120))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_word_longer_than_a_piece_is_cut() {
        let blob = "QUJD".repeat(10_000);
        let parts = chunks(&format!("Вложение в base64: {blob} конец"));
        assert!(parts.len() >= 40, "{}", parts.len());
        assert!(
            parts.iter().all(|p| p.chars().count() <= CHUNK),
            "{:?}",
            parts.iter().map(|p| p.chars().count()).max()
        );
        let kept: usize = parts.iter().map(|p| p.chars().count()).sum();
        assert!(kept >= blob.len(), "{kept}");
    }

    #[test]
    fn text_in_windows_1251_is_read() {
        let ansi = [
            0xC4, 0xEE, 0xE3, 0xEE, 0xE2, 0xEE, 0xF0, b' ', 0xB9, b'5', b' ', 0xAB, 0xB8, 0xA8,
            0xBB, b';', 0xF8, 0xF2, 0xF0, 0xE0, 0xF4, b' ', b'1', b'%',
        ];
        assert_eq!(plain_text(&ansi).unwrap(), "Договор №5 «ёЁ»;штраф 1%");
        let mut damaged = "Штраф по договору".as_bytes().to_vec();
        damaged.push(0xD0);
        assert!(plain_text(&damaged)
            .unwrap()
            .starts_with("Штраф по договору"));
        assert_eq!(plain_text(b"plain ascii").unwrap(), "plain ascii");
        assert!(plain_text(b"bin\0ary").is_none());

        let d = tempfile::tempdir().unwrap();
        let csv = d.path().join("выгрузка.csv");
        std::fs::write(&csv, ansi).unwrap();
        let pages = extract(&csv).unwrap();
        assert!(pages[0].1.contains("Договор"), "{pages:?}");
    }

    #[tokio::test]
    async fn a_folder_that_does_not_open_keeps_its_documents() {
        let d = tempfile::tempdir().unwrap();
        let docs = d.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        std::fs::write(
            docs.join("договор.txt"),
            "Штраф за просрочку оплаты составляет 1% в день от суммы договора.",
        )
        .unwrap();
        let index = DocIndex::new(Arc::new(Db::open_in_memory().unwrap()), None);
        let cancel = CancellationToken::new();
        let folders = std::slice::from_ref(&docs);
        index.run(folders, &cancel).await.unwrap();
        assert_eq!(index.status().files, 1);
        // A network drive away or a stick out: the folder does not open.
        std::fs::rename(&docs, d.path().join("away")).unwrap();
        let report = index.run(folders, &cancel).await.unwrap();
        assert!(report.contains("убрано: 0"), "{report}");
        assert_eq!(index.search("штраф договор", 3).await.unwrap().len(), 1);
        // A folder removed from the settings does leave.
        let report = index.run(&[], &cancel).await.unwrap();
        assert!(report.contains("убрано: 1"), "{report}");
        assert_eq!(index.status().files, 0);
    }

    #[tokio::test]
    async fn a_file_that_fails_to_read_keeps_its_text_and_is_read_again() {
        let d = tempfile::tempdir().unwrap();
        let doc = d.path().join("рецепт.docx");
        std::fs::write(
            &doc,
            zip_with(
                "word/document.xml",
                "<w:p><w:t>Борщ: свёкла, капуста, картошка и много любви к готовке.</w:t></w:p>"
                    .as_bytes(),
            ),
        )
        .unwrap();
        let index = DocIndex::new(Arc::new(Db::open_in_memory().unwrap()), None);
        let cancel = CancellationToken::new();
        let folders = vec![d.path().to_path_buf()];
        index.run(&folders, &cancel).await.unwrap();
        // Caught half-saved: not a ZIP for now.
        std::fs::write(&doc, b"PK half-saved").unwrap();
        let report = index.run(&folders, &cancel).await.unwrap();
        assert!(report.contains("Не прочитались: 1"), "{report}");
        assert_eq!(index.search("борщ капуста", 3).await.unwrap().len(), 1);
        let again = index.run(&folders, &cancel).await.unwrap();
        assert!(again.contains("Не прочитались: 1"), "{again}");
        // A new file that does not open is not taken as read.
        std::fs::write(d.path().join("новый.docx"), b"not a zip").unwrap();
        index.run(&folders, &cancel).await.unwrap();
        assert_eq!(index.status().files, 1);
    }

    #[tokio::test]
    async fn a_full_index_still_rereads_changed_files() {
        let d = tempfile::tempdir().unwrap();
        let file = |name: &str| d.path().join(name);
        std::fs::write(file("a.txt"), paras(&["альфа"])).unwrap();
        std::fs::write(file("b.txt"), paras(&["браво", "бетон", "башня"])).unwrap();
        std::fs::write(file("c.txt"), paras(&["чарли"])).unwrap();
        let mut index = DocIndex::new(Arc::new(Db::open_in_memory().unwrap()), None);
        index.max_chunks = 3;
        let cancel = CancellationToken::new();
        let folders = vec![d.path().to_path_buf()];
        let found = |q: &'static str| {
            let index = index.clone();
            async move { !index.search(q, 3).await.unwrap().is_empty() }
        };

        // b.txt does not fit; c.txt after it still goes in.
        let report = index.run(&folders, &cancel).await.unwrap();
        assert!(report.contains("Индекс полон (3 кусков)"), "{report}");
        assert!(found("чарли").await && found("альфа").await && !found("браво").await);

        // While there is no room, b.txt is not read again: the same date and
        // size hide a shorter text that would fit.
        let stamp = std::fs::metadata(file("b.txt")).unwrap();
        let mut small = paras(&["дельта"]).into_bytes();
        small.resize(stamp.len() as usize, b'\n');
        std::fs::write(file("b.txt"), &small).unwrap();
        std::fs::File::options()
            .write(true)
            .open(file("b.txt"))
            .unwrap()
            .set_modified(stamp.modified().unwrap())
            .unwrap();
        index.run(&folders, &cancel).await.unwrap();
        assert!(!found("дельта").await);

        // A changed file goes before a new one: its new text takes the room.
        std::fs::write(file("b2.txt"), paras(&["эхо"])).unwrap();
        std::fs::write(file("c.txt"), paras(&["чайник", "чашка"])).unwrap();
        index.run(&folders, &cancel).await.unwrap();
        assert!(found("чашка").await && !found("эхо").await);

        // A changed file that no longer fits does not answer with its old text.
        std::fs::write(file("a.txt"), paras(&["омега", "оазис", "осина"])).unwrap();
        index.run(&folders, &cancel).await.unwrap();
        assert!(!found("альфа").await && !found("омега").await);
    }

    #[tokio::test]
    async fn a_file_goes_into_the_index_whole_or_not_at_all() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("журнал.txt");
        std::fs::write(
            &log,
            "Первая запись журнала о поставке товара на склад номер один.",
        )
        .unwrap();
        let db = Arc::new(Db::open_in_memory().unwrap());
        let index = DocIndex::new(db.clone(), None);
        let cancel = CancellationToken::new();
        let folders = vec![d.path().to_path_buf()];
        index.run(&folders, &cancel).await.unwrap();
        // The second piece of the changed file fails to be written.
        db.with(|c| {
            c.execute_batch(
                "CREATE TRIGGER fail BEFORE INSERT ON doc_chunks WHEN NEW.text LIKE '%СБОЙ%'
                 BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
            )
        })
        .unwrap();
        std::fs::write(
            &log,
            format!(
                "{}\n{}",
                "Вторая запись журнала. ".repeat(30),
                "СБОЙ записи. ".repeat(60)
            ),
        )
        .unwrap();
        assert!(index.run(&folders, &cancel).await.is_err());
        let texts: Vec<String> = db
            .with(|c| {
                let mut st = c.prepare("SELECT text FROM doc_chunks")?;
                let rows = st.query_map([], |r| r.get(0))?;
                rows.collect()
            })
            .unwrap();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(texts[0].starts_with("Первая"), "{texts:?}");
    }

    #[tokio::test]
    async fn a_refused_or_overlong_piece_does_not_hold_back_the_vectors() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.txt"),
            "Неустойка за каждый день задержки равна одному проценту.",
        )
        .unwrap();
        std::fs::write(
            d.path().join("b.txt"),
            "ОТКАЗ: этот кусок модель не принимает ни в каком виде вообще.",
        )
        .unwrap();
        // A piece read by an older version: one word of 40 000 characters.
        let old = d.path().join("выгрузка.json");
        std::fs::write(&old, "{}").unwrap();
        let key = old.to_string_lossy().into_owned();
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.with(|c| {
            c.execute(
                "INSERT INTO doc_files (path, modified, size) VALUES (?1, ?2, 2)",
                params![key, modified_secs(&old)],
            )?;
            c.execute(
                "INSERT INTO doc_chunks (path, page, text) VALUES (?1, 0, ?2)",
                params![key, "QUJD".repeat(10_000)],
            )
        })
        .unwrap();
        let model = Arc::new(Picky::default());
        let index = DocIndex::new(db, Some(model.clone()));
        let cancel = CancellationToken::new();
        let folders = vec![d.path().to_path_buf()];
        let report = index.run(&folders, &cancel).await.unwrap();
        assert!(report.contains("Векторов построено: 2"), "{report}");
        assert!(report.contains("Модель не приняла кусков: 1"), "{report}");
        assert_eq!(index.status().embedded, 2);
        let longest = model.sent.lock().iter().map(|t| t.chars().count()).max();
        assert_eq!(longest, Some(EMBED_CHARS));

        // No answer at all is not a refusal: the pass fails and marks nothing.
        model.dead.store(true, Ordering::Relaxed);
        let err = index.run(&folders, &cancel).await.unwrap_err();
        assert!(err.contains("сеть"), "{err}");
        assert_eq!(index.status().error.as_deref(), Some(err.as_str()));
        // The refused piece is tried again on the next pass.
        model.dead.store(false, Ordering::Relaxed);
        let report = index.run(&folders, &cancel).await.unwrap();
        assert!(report.contains("Модель не приняла кусков: 1"), "{report}");
    }

    #[tokio::test]
    async fn pieces_without_vectors_are_found_by_words() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.txt"),
            "Неустойка за каждый день задержки равна одному проценту.",
        )
        .unwrap();
        std::fs::write(
            d.path().join("b.txt"),
            "Борщ варят из свёклы и капусты, подают со сметаной.",
        )
        .unwrap();
        let index = DocIndex::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Some(Arc::new(Picky::default())),
        );
        let cancel = CancellationToken::new();
        let folders = vec![d.path().to_path_buf()];
        index.run(&folders, &cancel).await.unwrap();
        // The new contract gets no vector: the model refuses it.
        std::fs::write(
            d.path().join("c.txt"),
            "ОТКАЗ модели. Договор поставки: штрафы за просрочку по договору 5%.",
        )
        .unwrap();
        let _ = index.run(&folders, &cancel).await;
        let hits = index.search("штрафы по договору", 3).await.unwrap();
        assert!(hits[0].path.ends_with("a.txt"), "{hits:?}");
        assert!(hits.iter().any(|h| h.path.ends_with("c.txt")), "{hits:?}");
    }

    #[tokio::test]
    async fn a_folder_removed_mid_pass_is_not_read_and_not_sent() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("keep");
        let private = d.path().join("private");
        std::fs::create_dir(&keep).unwrap();
        std::fs::create_dir(&private).unwrap();
        std::fs::write(
            keep.join("a.txt"),
            "Неустойка за каждый день задержки равна одному проценту.",
        )
        .unwrap();
        std::fs::write(
            private.join("b.txt"),
            "Личное: номер паспорта и адрес, которые не должны уйти наружу.",
        )
        .unwrap();
        let db = Arc::new(Db::open_in_memory().unwrap());
        let cancel = CancellationToken::new();
        // Read once without a model: these pieces wait for their vectors.
        DocIndex::new(db.clone(), None)
            .run(&[keep.clone(), private.clone()], &cancel)
            .await
            .unwrap();
        std::fs::write(
            private.join("c.txt"),
            "Ещё одна личная запись, которую никто не должен читать без спроса.",
        )
        .unwrap();
        std::fs::write(
            keep.join("d.txt"),
            "Борщ варят из свёклы и капусты, подают со сметаной.",
        )
        .unwrap();
        let model = Arc::new(Picky::default());
        let index = DocIndex::new(db, Some(model.clone()));
        // The private folder leaves the settings right after the pass begins.
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let current = || {
            if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                vec![keep.clone(), private.clone()]
            } else {
                vec![keep.clone()]
            }
        };
        let report = index.run_with(current, &cancel).await.unwrap();
        assert!(report.contains("прочитано заново: 1"), "{report}");
        let sent = model.sent.lock().join("\n");
        assert!(
            sent.contains("Неустойка") && sent.contains("Борщ"),
            "{sent}"
        );
        assert!(!sent.to_lowercase().contains("личн"), "{sent}");
        assert_eq!(index.status().files, 2);
        let hits = index.search("личная запись паспорт", 5).await.unwrap();
        assert!(hits.iter().all(|h| h.path.contains("keep")), "{hits:?}");
    }

    #[tokio::test]
    async fn search_by_meaning_finds_a_piece_without_shared_words() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.txt"),
            "Неустойка за каждый день задержки равна одному проценту.",
        )
        .unwrap();
        std::fs::write(
            d.path().join("b.txt"),
            "Борщ варят из свёклы и капусты, подают со сметаной.",
        )
        .unwrap();
        let index = DocIndex::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Some(Arc::new(Axes)),
        );
        let report = index
            .run(&[d.path().to_path_buf()], &CancellationToken::new())
            .await
            .unwrap();
        assert!(report.contains("Векторов построено: 2"), "{report}");
        assert_eq!(index.status().embedded, 2);
        let hits = index.search("какие пени за опоздание", 1).await.unwrap();
        assert!(hits[0].path.ends_with("a.txt"), "{hits:?}");
    }
}
