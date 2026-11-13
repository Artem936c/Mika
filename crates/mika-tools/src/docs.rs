//! Questions about the user's own documents: the chosen folders are read into
//! pieces of text, with vectors when an embedding model is set, and the pieces
//! closest to a question go to the model with their file and page.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::memory::{cosine, decode_vector, encode_vector, Embedder};
use mika_core::storage::rusqlite::params;
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
            let text = crate::search::read_text(&bytes).ok_or("в файле не текст")?;
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
            // A long paragraph is cut at spaces.
            for word in para.split(' ') {
                let w = word.chars().count();
                if cur_len + w + 1 > CHUNK && cur_len > 0 {
                    flush(&mut cur, &mut cur_len, &mut out);
                }
                cur.push_str(word);
                cur.push(' ');
                cur_len += w + 1;
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

/// Supported files under the folders: (path, modified unix seconds, size).
fn walk(folders: &[PathBuf]) -> Vec<(PathBuf, i64, u64)> {
    fn go(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, i64, u64)>) {
        if out.len() >= MAX_FILES {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
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
            go(&d, depth - 1, out);
        }
    }
    let mut out = Vec::new();
    for f in folders {
        go(f, MAX_DEPTH, &mut out);
    }
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

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
}

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
        self.db
            .with(|c| c.execute_batch("DELETE FROM doc_chunks; DELETE FROM doc_files;"))
            .map_err(db_err)
    }

    /// Reads new and changed files, drops gone ones, makes missing vectors.
    /// Returns a short report. A second call while one runs does nothing.
    pub async fn run(
        &self,
        folders: &[PathBuf],
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        if self.running.swap(true, Ordering::Relaxed) {
            return Err("индекс уже обновляется".into());
        }
        let result = self.pass(folders, cancel).await;
        self.running.store(false, Ordering::Relaxed);
        let mut s = self.state.lock();
        s.progress = None;
        s.last_run = Some(chrono::Local::now().timestamp());
        s.error = result.as_ref().err().cloned();
        result
    }

    async fn pass(
        &self,
        folders: &[PathBuf],
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let roots = folders.to_vec();
        let found = tokio::task::spawn_blocking(move || walk(&roots))
            .await
            .map_err(|e| e.to_string())?;
        let known: HashMap<String, (i64, i64)> = self
            .db
            .with(|c| {
                let mut st = c.prepare("SELECT path, modified, size FROM doc_files")?;
                let rows = st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?;
                rows.collect()
            })
            .map_err(db_err)?;
        let here: std::collections::HashSet<String> = found
            .iter()
            .map(|f| f.0.to_string_lossy().into_owned())
            .collect();
        let gone: Vec<&String> = known.keys().filter(|p| !here.contains(*p)).collect();
        self.db
            .with(|c| {
                for p in &gone {
                    c.execute("DELETE FROM doc_chunks WHERE path = ?1", [p])?;
                    c.execute("DELETE FROM doc_files WHERE path = ?1", [p])?;
                }
                Ok(())
            })
            .map_err(db_err)?;
        let todo: Vec<&(PathBuf, i64, u64)> = found
            .iter()
            .filter(|(p, m, s)| known.get(p.to_string_lossy().as_ref()) != Some(&(*m, *s as i64)))
            .collect();
        let mut chunk_count: usize = self
            .db
            .with(|c| {
                c.query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| {
                    r.get::<_, i64>(0)
                })
            })
            .map_err(db_err)? as usize;
        let (mut read, mut failed_files, mut full) = (0, 0, false);
        for (i, (path, modified, size)) in todo.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            self.state.lock().progress = Some((i, todo.len()));
            let p = path.clone();
            let pages = tokio::task::spawn_blocking(move || extract(&p))
                .await
                .map_err(|e| e.to_string())?;
            let key = path.to_string_lossy().into_owned();
            let pieces: Vec<(u32, String)> = match pages {
                Ok(pages) => pages
                    .into_iter()
                    .flat_map(|(page, text)| chunks(&text).into_iter().map(move |t| (page, t)))
                    .collect(),
                Err(e) => {
                    log::info!("document not read: {e}");
                    failed_files += 1;
                    Vec::new()
                }
            };
            let old: usize = self
                .db
                .with(|c| {
                    c.query_row(
                        "SELECT COUNT(*) FROM doc_chunks WHERE path = ?1",
                        [&key],
                        |r| r.get::<_, i64>(0),
                    )
                })
                .map_err(db_err)? as usize;
            if chunk_count - old + pieces.len() > MAX_CHUNKS {
                full = true;
                break;
            }
            chunk_count = chunk_count - old + pieces.len();
            self.db
                .with(|c| {
                    c.execute("DELETE FROM doc_chunks WHERE path = ?1", [&key])?;
                    for (page, text) in &pieces {
                        c.execute(
                            "INSERT INTO doc_chunks (path, page, text) VALUES (?1, ?2, ?3)",
                            params![key, page, text],
                        )?;
                    }
                    c.execute(
                        "INSERT OR REPLACE INTO doc_files (path, modified, size) VALUES (?1, ?2, ?3)",
                        params![key, modified, *size as i64],
                    )?;
                    Ok(())
                })
                .map_err(db_err)?;
            read += 1;
        }
        let embedded = self.embed_missing(cancel).await?;
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
        if full {
            report.push_str(&format!(
                " Индекс полон ({MAX_CHUNKS} кусков): остальные файлы не вошли."
            ));
        }
        Ok(report)
    }

    /// Vectors for pieces that have none of the current model.
    async fn embed_missing(&self, cancel: &CancellationToken) -> Result<usize, String> {
        let Some((e, model)) = self.model() else {
            return Ok(0);
        };
        let mut done = 0;
        loop {
            if cancel.is_cancelled() {
                break;
            }
            let batch: Vec<(i64, String)> = self
                .db
                .with(|c| {
                    let mut st = c.prepare(
                        "SELECT id, text FROM doc_chunks WHERE model IS NULL OR model != ?1 LIMIT ?2",
                    )?;
                    let rows = st.query_map(params![model, EMBED_BATCH as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?;
                    rows.collect()
                })
                .map_err(db_err)?;
            if batch.is_empty() {
                break;
            }
            let texts: Vec<String> = batch.iter().map(|b| b.1.clone()).collect();
            let vectors = e.embed(&texts).await?;
            if vectors.len() != batch.len() {
                return Err("ответ модели эмбеддингов не той длины".into());
            }
            self.db
                .with(|c| {
                    for ((id, _), v) in batch.iter().zip(&vectors) {
                        c.execute(
                            "UPDATE doc_chunks SET model = ?2, embedding = ?3 WHERE id = ?1",
                            params![id, model, encode_vector(v)],
                        )?;
                    }
                    Ok(())
                })
                .map_err(db_err)?;
            done += batch.len();
        }
        Ok(done)
    }

    /// The pieces closest to a question: by meaning with vectors, else by words.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<DocHit>, String> {
        if let Some((e, model)) = self.model() {
            let rows: Vec<(String, u32, String, Vec<u8>)> = self
                .db
                .with(|c| {
                    let mut st = c.prepare(
                        "SELECT path, page, text, embedding FROM doc_chunks WHERE model = ?1",
                    )?;
                    let rows = st.query_map([&model], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?;
                    rows.collect()
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
                        return Ok(hits);
                    }
                    Ok(_) => log::warn!("embedding answer has the wrong size"),
                    Err(err) => log::warn!("document search by meaning failed, using words: {err}"),
                }
            }
        }
        self.search_words(query, limit)
    }

    fn search_words(&self, query: &str, limit: usize) -> Result<Vec<DocHit>, String> {
        let words = stems(query);
        if words.is_empty() {
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
        Ok(hits)
    }
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

    struct Axes;

    #[async_trait]
    impl Embedder for Axes {
        fn model(&self) -> Option<String> {
            Some("axes-1".into())
        }
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
            Ok(texts
                .iter()
                .map(|t| {
                    let t = t.to_lowercase();
                    let has = |w: &[&str]| w.iter().any(|x| t.contains(x)) as u8 as f32;
                    vec![
                        has(&["штраф", "неустойк", "пени"]),
                        has(&["борщ", "суп", "еда"]),
                    ]
                })
                .collect())
        }
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
