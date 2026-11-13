//! Looking inside folders: text in files, and what takes the space.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::paths::PathAccess;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::batch::size_text;
use crate::files::{path_arg, path_assessment, SKIP_DIRS};
use crate::util::{failed, schema, truncate};

/// Files of these kinds are read as text.
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "csv", "log", "json", "xml", "html", "htm", "ini", "cfg", "conf", "toml", "yaml",
    "yml", "rs", "py", "js", "ts", "java", "cs", "cpp", "c", "h", "go", "php", "sql", "bat", "ps1",
    "sh", "css", "svelte", "vue", "bsl", "srt", "tex", "rtf",
];
const MAX_FILE: u64 = 2 * 1024 * 1024;
const MAX_FILES: usize = 3000;
const MAX_HITS: usize = 40;
const MAX_DEPTH: usize = 8;
/// A long walk stops here and says so.
const TIME_LIMIT: Duration = Duration::from_secs(20);

fn skipped_dir(p: &Path) -> bool {
    p.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .is_some_and(|n| SKIP_DIRS.contains(&n.as_str()))
}

fn is_text_file(p: &Path) -> bool {
    p.extension()
        .map(|x| x.to_string_lossy().to_lowercase())
        .is_some_and(|x| TEXT_EXTS.contains(&x.as_str()))
}

/// The file as text: UTF-8, or UTF-16 with its mark. `None` for binary data.
fn read_text(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return Some(String::from_utf16_lossy(&units));
    }
    let head = &bytes[..bytes.len().min(1024)];
    if head.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes));
    Some(text.into_owned())
}

#[derive(Debug, Default, PartialEq)]
struct Found {
    hits: Vec<String>,
    files_read: usize,
    /// The walk stopped early: too many files or too long.
    cut: bool,
}

fn search(root: &Path, needle: &str, cancel: &CancellationToken) -> Found {
    let needle = needle.to_lowercase();
    let started = Instant::now();
    let mut found = Found::default();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if cancel.is_cancelled()
                || started.elapsed() > TIME_LIMIT
                || found.files_read >= MAX_FILES
            {
                found.cut = true;
                return found;
            }
            let p = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if depth < MAX_DEPTH && !skipped_dir(&p) {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            if !is_text_file(&p) || meta.len() > MAX_FILE {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else {
                continue;
            };
            found.files_read += 1;
            let Some(text) = read_text(&bytes) else {
                continue;
            };
            let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    found
                        .hits
                        .push(format!("{rel}:{}: {}", i + 1, truncate(line.trim(), 160)));
                    if found.hits.len() >= MAX_HITS {
                        found.cut = true;
                        return found;
                    }
                }
            }
        }
    }
    found
}

pub struct SearchTextTool {
    pub user_profile: String,
}

#[async_trait]
impl Tool for SearchTextTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "search_text",
            "Найти слово или фразу внутри файлов папки: текстовые файлы, заметки, таблицы csv, код. Отвечает файлом, номером строки и самой строкой. find_files ищет только по имени файла.",
            schema(
                &[
                    ("text", "string", "Что искать, без учёта регистра"),
                    ("path", "string", "Папка, например ~/Documents"),
                ],
                &["text", "path"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let text = arg_str(args, "text")?.trim();
        if text.chars().count() < 2 {
            return Err(ToolError::BadArguments(
                "искать нужно хотя бы две буквы".into(),
            ));
        }
        let p = path_arg(args, "path", &self.user_profile)?;
        Ok(path_assessment(
            &p,
            &self.user_profile,
            PathAccess::Read,
            format!("Найти «{}» в файлах", truncate(text, 40)),
            RiskLevel::Low,
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let text = arg_str(&args, "text")?.trim().to_string();
        let root = path_arg(&args, "path", &self.user_profile)?;
        if !root.is_dir() {
            return Err(failed(format!("нет такой папки: {}", root.display())));
        }
        let shown = root.display().to_string();
        let found = tokio::task::spawn_blocking(move || search(&root, &text, &cancel))
            .await
            .map_err(|e| failed(e.to_string()))?;
        let cut = if found.cut {
            " Поиск остановлен раньше: слишком много файлов или совпадений, сузь папку."
        } else {
            ""
        };
        if found.hits.is_empty() {
            return Ok(ToolOutput::new(format!(
                "В {shown} не нашла: просмотрела {} текстовых файлов.{cut}",
                found.files_read
            )));
        }
        Ok(ToolOutput::new(format!(
            "Нашла в {shown} ({} файлов просмотрено):\n{}{}",
            found.files_read,
            found.hits.join("\n"),
            if cut.is_empty() {
                String::new()
            } else {
                format!("\n{}", cut.trim())
            }
        )))
    }
}

#[derive(Debug, Default)]
struct Usage {
    total: u64,
    /// (name, bytes) of each folder right inside the root.
    folders: Vec<(String, u64)>,
    /// (path from the root, bytes) of the biggest files anywhere.
    files: Vec<(String, u64)>,
    cut: bool,
}

const TOP: usize = 10;

fn keep_biggest(list: &mut Vec<(String, u64)>, item: (String, u64)) {
    list.push(item);
    list.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
    list.truncate(TOP);
}

fn usage(root: &Path, cancel: &CancellationToken) -> Usage {
    let started = Instant::now();
    let mut u = Usage::default();
    let mut sizes: Vec<(String, u64)> = Vec::new();
    // (folder, index in `sizes` of the top-level folder it belongs to)
    let mut stack: Vec<(PathBuf, Option<usize>)> = vec![(root.to_path_buf(), None)];
    while let Some((dir, top)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.filter_map(Result::ok) {
            if cancel.is_cancelled() || started.elapsed() > TIME_LIMIT {
                u.cut = true;
                break;
            }
            let p = e.path();
            // Links and junctions would count a folder twice.
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                let owner = top.or_else(|| {
                    sizes.push((e.file_name().to_string_lossy().into_owned(), 0));
                    Some(sizes.len() - 1)
                });
                stack.push((p, owner));
                continue;
            }
            let len = meta.len();
            u.total += len;
            if let Some(i) = top {
                sizes[i].1 += len;
            }
            let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
            keep_biggest(&mut u.files, (rel, len));
        }
        if u.cut {
            break;
        }
    }
    sizes.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
    sizes.truncate(TOP);
    u.folders = sizes;
    u
}

pub struct DiskUsageTool {
    pub user_profile: String,
}

#[async_trait]
impl Tool for DiskUsageTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "disk_usage",
            "Что занимает место в папке: сколько весят вложенные папки и какие файлы самые большие. Для «куда делось место на диске».",
            schema(&[("path", "string", "Папка, например ~ или ~/Downloads")], &["path"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let p = path_arg(args, "path", &self.user_profile)?;
        Ok(path_assessment(
            &p,
            &self.user_profile,
            PathAccess::Read,
            "Посчитать, что занимает место".into(),
            RiskLevel::Low,
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let root = path_arg(&args, "path", &self.user_profile)?;
        if !root.is_dir() {
            return Err(failed(format!("нет такой папки: {}", root.display())));
        }
        let shown = root.display().to_string();
        let u = tokio::task::spawn_blocking(move || usage(&root, &cancel))
            .await
            .map_err(|e| failed(e.to_string()))?;
        let line = |(name, bytes): &(String, u64)| format!("{name} — {}", size_text(*bytes));
        let mut text = format!("В {shown} всего {}.", size_text(u.total));
        if !u.folders.is_empty() {
            let list: Vec<String> = u.folders.iter().map(line).collect();
            text.push_str(&format!("\nПапки: {}.", list.join("; ")));
        }
        if !u.files.is_empty() {
            let list: Vec<String> = u.files.iter().map(line).collect();
            text.push_str(&format!("\nСамые большие файлы: {}.", list.join("; ")));
        }
        if u.cut {
            text.push_str("\nПодсчёт остановлен через 20 секунд: числа меньше настоящих.");
        }
        Ok(ToolOutput::new(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use serde_json::json;

    #[tokio::test]
    async fn text_is_found_inside_files_with_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("notes/deep")).unwrap();
        std::fs::write(
            root.join("notes/a.txt"),
            "первая\nКлюч от гаража у соседа\n",
        )
        .unwrap();
        std::fs::write(root.join("notes/deep/b.md"), "ключ — в ящике").unwrap();
        std::fs::write(
            root.join("pic.png"),
            [b"\x89PNG ", "ключ".as_bytes()].concat(),
        )
        .unwrap();
        std::fs::write(
            root.join("bin.txt"),
            [b"\x00\x01", "ключ".as_bytes()].concat(),
        )
        .unwrap();
        let mut utf16 = vec![0xFF, 0xFE];
        for u in "где ключ?".encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        std::fs::write(root.join("w.txt"), utf16).unwrap();
        let profile = root.display().to_string();
        let tool = SearchTextTool {
            user_profile: profile.clone(),
        };
        let out = run(&tool, json!({"text": "КЛЮЧ", "path": profile}))
            .await
            .unwrap()
            .text;
        assert!(
            out.contains("notes\\a.txt:2: Ключ от гаража у соседа"),
            "{out}"
        );
        assert!(out.contains("notes\\deep\\b.md:1:"), "{out}");
        assert!(out.contains("w.txt:1: где ключ?"), "{out}");
        assert!(
            !out.contains("pic.png") && !out.contains("bin.txt"),
            "{out}"
        );
        assert!(tool.assess(&json!({"text": "к", "path": "~"})).is_err());
    }

    #[tokio::test]
    async fn usage_sums_folders_and_names_the_biggest_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("video/old")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("video/old/film.mkv"), vec![0u8; 5000]).unwrap();
        std::fs::write(root.join("video/clip.mp4"), vec![0u8; 3000]).unwrap();
        std::fs::write(root.join("docs/a.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(root.join("top.bin"), vec![0u8; 50]).unwrap();
        let profile = root.display().to_string();
        let tool = DiskUsageTool {
            user_profile: profile.clone(),
        };
        let out = run(&tool, json!({"path": profile})).await.unwrap().text;
        assert!(out.contains("всего 8 КБ"), "{out}");
        assert!(out.contains("Папки: video — 8 КБ; docs — 100 Б."), "{out}");
        assert!(
            out.contains(
                "Самые большие файлы: video\\old\\film.mkv — 5 КБ; video\\clip.mp4 — 3 КБ"
            ),
            "{out}"
        );
    }
}
