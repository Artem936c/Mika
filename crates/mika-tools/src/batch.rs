//! Many files at once: list a folder, then move, copy, rename, create folders and
//! delete to the Recycle Bin under one confirmation that shows the whole list.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::paths::PathAccess;
use mika_core::tools::{arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::files::{path_arg, path_assessment};
use crate::sys::Trash;
use crate::util::{arg_bool, failed, resolve_path, schema};

const MAX_LIST: usize = 300;
const MAX_OPS: usize = 200;
/// Lines of the batch shown on the confirm card.
const SHOWN_OPS: usize = 15;

pub struct ListFolderTool {
    pub user_profile: String,
}

fn size_text(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b < K {
        format!("{bytes} Б")
    } else if b < K * K {
        format!("{:.0} КБ", b / K)
    } else if b < K * K * K {
        format!("{:.1} МБ", b / K / K).replace('.', ",")
    } else {
        format!("{:.1} ГБ", b / K / K / K).replace('.', ",")
    }
}

/// `*.pdf`, `pdf`, `.pdf` or `pdf,jpg` — lower-case extensions without the dot.
fn extensions(pattern: &str) -> Vec<String> {
    pattern
        .split([',', ';', ' '])
        .map(|p| {
            p.trim()
                .trim_start_matches('*')
                .trim_start_matches('.')
                .to_lowercase()
        })
        .filter(|p| !p.is_empty())
        .collect()
}

fn list(
    root: &Path,
    dir: &Path,
    depth: usize,
    exts: &[String],
    out: &mut Vec<String>,
    total: &mut usize,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
    for e in entries {
        let p = e.path();
        let Ok(meta) = e.metadata() else { continue };
        let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
        if meta.is_dir() {
            if exts.is_empty() {
                *total += 1;
                if out.len() < MAX_LIST {
                    out.push(format!("{rel}\\ · папка"));
                }
            }
            if depth > 0 {
                list(root, &p, depth - 1, exts, out, total);
            }
            continue;
        }
        let ext = p
            .extension()
            .map(|x| x.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !exts.is_empty() && !exts.contains(&ext) {
            continue;
        }
        *total += 1;
        if out.len() < MAX_LIST {
            let date = meta
                .modified()
                .ok()
                .map(|t| {
                    chrono::DateTime::<chrono::Local>::from(t)
                        .format("%Y-%m-%d")
                        .to_string()
                })
                .unwrap_or_default();
            out.push(format!("{rel} · {} · {date}", size_text(meta.len())));
        }
    }
}

#[async_trait]
impl Tool for ListFolderTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "list_folder",
            "Показать, что лежит в папке: имена, размеры, даты изменения. Можно отобрать по расширению и заглянуть во вложенные папки.",
            schema(
                &[
                    ("path", "string", "Папка, например ~/Downloads"),
                    ("pattern", "string", "Расширения через запятую: pdf,jpg"),
                    ("recursive", "boolean", "Заглянуть во вложенные папки"),
                ],
                &["path"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let p = path_arg(args, "path", &self.user_profile)?;
        Ok(path_assessment(
            &p,
            &self.user_profile,
            PathAccess::Read,
            "Посмотреть папку".into(),
            RiskLevel::Low,
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let p = path_arg(&args, "path", &self.user_profile)?;
        if !p.is_dir() {
            return Err(failed(format!("нет такой папки: {}", p.display())));
        }
        let exts = extensions(arg_str_opt(&args, "pattern").unwrap_or(""));
        let depth = if arg_bool(&args, "recursive") { 4 } else { 0 };
        let (lines, total) = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            let mut total = 0;
            list(&p, &p, depth, &exts, &mut out, &mut total);
            (out, total)
        })
        .await
        .map_err(|e| failed(e.to_string()))?;
        if lines.is_empty() {
            return Ok(ToolOutput::new("Пусто: подходящих файлов нет."));
        }
        let mut text = lines.join("\n");
        if total > lines.len() {
            text.push_str(&format!("\n…и ещё {} не показаны", total - lines.len()));
        }
        Ok(ToolOutput::new(format!("Всего {total}:\n{text}")))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Move(PathBuf, PathBuf),
    Copy(PathBuf, PathBuf),
    Mkdir(PathBuf),
    Trash(PathBuf),
}

impl Op {
    fn line(&self) -> String {
        match self {
            Op::Move(a, b) => format!("перенести {} → {}", a.display(), b.display()),
            Op::Copy(a, b) => format!("скопировать {} → {}", a.display(), b.display()),
            Op::Mkdir(p) => format!("создать папку {}", p.display()),
            Op::Trash(p) => format!("в корзину {}", p.display()),
        }
    }

    fn paths(&self) -> Vec<(&Path, PathAccess)> {
        match self {
            Op::Move(a, b) => vec![(a, PathAccess::Write), (b, PathAccess::Write)],
            Op::Copy(a, b) => vec![(a, PathAccess::Read), (b, PathAccess::Write)],
            Op::Mkdir(p) => vec![(p, PathAccess::Write)],
            Op::Trash(p) => vec![(p, PathAccess::Delete)],
        }
    }
}

pub fn parse_ops(args: &Value, profile: &str) -> Result<Vec<Op>, ToolError> {
    let items = args
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| ToolError::BadArguments("нужен список действий `operations`".into()))?;
    if items.is_empty() {
        return Err(ToolError::BadArguments("список действий пуст".into()));
    }
    if items.len() > MAX_OPS {
        return Err(ToolError::BadArguments(format!(
            "слишком много действий: не больше {MAX_OPS} за раз"
        )));
    }
    let path = |item: &Value, keys: &[&str]| -> Result<PathBuf, ToolError> {
        let raw = keys
            .iter()
            .find_map(|k| item.get(*k).and_then(Value::as_str))
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| ToolError::BadArguments(format!("в действии нет пути `{}`", keys[0])))?;
        checked_path(raw, profile)
    };
    items
        .iter()
        .map(|item| {
            let op = item.get("op").and_then(Value::as_str).unwrap_or("").trim();
            Ok(match op {
                "move" | "rename" => Op::Move(path(item, &["from", "path"])?, path(item, &["to"])?),
                "copy" => Op::Copy(path(item, &["from", "path"])?, path(item, &["to"])?),
                "mkdir" => Op::Mkdir(path(item, &["path", "to"])?),
                "trash" | "delete" => Op::Trash(path(item, &["path", "from"])?),
                other => {
                    return Err(ToolError::BadArguments(format!(
                        "неизвестное действие `{other}`: move, copy, rename, mkdir, trash"
                    )))
                }
            })
        })
        .collect()
}

/// A batch path must be full: `C:\…`, `\\server\…`, `~\…` or `%VAR%\…`. A relative path
/// would land in the user's home folder, and names of dots or masks are never files.
pub fn checked_path(raw: &str, profile: &str) -> Result<PathBuf, ToolError> {
    let s = raw.trim().trim_matches('"');
    let drive = s.len() >= 3
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.as_bytes()[1] == b':'
        && matches!(s.as_bytes()[2], b'\\' | b'/');
    if !(drive || s.starts_with("\\\\") || s.starts_with('~') || s.starts_with('%')) {
        return Err(ToolError::BadArguments(format!(
            "нужен полный путь вроде C:\\Папка\\файл или ~\\Downloads\\файл, а не «{s}»"
        )));
    }
    if s.contains(['*', '?', '<', '>', '|']) {
        return Err(ToolError::BadArguments(format!(
            "в пути «{s}» маска: перечисли файлы по одному"
        )));
    }
    let p = resolve_path(s, profile);
    let dots = p
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => Some(n.to_string_lossy()),
            std::path::Component::ParentDir => Some("..".into()),
            _ => None,
        })
        .any(|n| n.chars().all(|c| c == '.'));
    if dots {
        return Err(ToolError::BadArguments(format!(
            "в пути «{s}» многоточие вместо части пути: пиши путь полностью, без сокращений"
        )));
    }
    Ok(p)
}

/// `name (2).ext` next to `path` when `path` is taken.
pub fn free_name(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new(""));
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// A destination that is an existing folder means "into that folder". So does a missing
/// one without an extension when a file with an extension goes there: `report.pdf → Документы`.
fn destination(from: &Path, to: &Path) -> PathBuf {
    let folder_meant =
        !to.exists() && to.extension().is_none() && from.is_file() && from.extension().is_some();
    let to = if to.is_dir() || folder_meant {
        match from.file_name() {
            Some(name) => to.join(name),
            None => to.to_path_buf(),
        }
    } else {
        to.to_path_buf()
    };
    free_name(&to)
}

fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_all(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

fn run_op(op: &Op, trash: &dyn Trash) -> Result<String, String> {
    match op {
        Op::Move(from, to) => {
            if !from.exists() {
                return Err(format!("нет {}", from.display()));
            }
            let dest = destination(from, to);
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            if std::fs::rename(from, &dest).is_err() {
                // Another disk: copy, then remove the source.
                copy_all(from, &dest).map_err(|e| e.to_string())?;
                if from.is_dir() {
                    std::fs::remove_dir_all(from).map_err(|e| e.to_string())?;
                } else {
                    std::fs::remove_file(from).map_err(|e| e.to_string())?;
                }
            }
            Ok(dest.display().to_string())
        }
        Op::Copy(from, to) => {
            if !from.exists() {
                return Err(format!("нет {}", from.display()));
            }
            let dest = destination(from, to);
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            copy_all(from, &dest).map_err(|e| e.to_string())?;
            Ok(dest.display().to_string())
        }
        Op::Mkdir(p) => {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            Ok(p.display().to_string())
        }
        Op::Trash(p) => {
            if !p.exists() {
                return Err(format!("нет {}", p.display()));
            }
            trash.trash(p)?;
            Ok(p.display().to_string())
        }
    }
}

pub struct FileBatchTool {
    pub trash: Arc<dyn Trash>,
    pub user_profile: String,
}

#[async_trait]
impl Tool for FileBatchTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "file_batch",
            "Выполнить много действий с файлами под одним подтверждением: move (перенести или переименовать), copy, mkdir, trash (в корзину). Если имя занято, добавляется « (2)». Папка в `to` значит «внутрь этой папки», недостающие папки создаются сами. Пути только полные (C:\\… или ~\\…), по одному файлу, без масок. Сначала посмотри файлы через list_folder.",
            json!({
                "type": "object",
                "properties": {
                    "operations": {
                        "type": "array",
                        "description": "Действия по порядку, до 200",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": { "type": "string", "enum": ["move", "rename", "copy", "mkdir", "trash"] },
                                "from": { "type": "string", "description": "Откуда (move, copy)" },
                                "to": { "type": "string", "description": "Куда (move, copy)" },
                                "path": { "type": "string", "description": "Путь (mkdir, trash)" }
                            },
                            "required": ["op"]
                        }
                    }
                },
                "required": ["operations"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let ops = parse_ops(args, &self.user_profile)?;
        let mut level = RiskLevel::Medium;
        let mut reason = String::new();
        for op in &ops {
            for (p, access) in op.paths() {
                let a = path_assessment(
                    p,
                    &self.user_profile,
                    access,
                    String::new(),
                    RiskLevel::Medium,
                );
                if a.level > level || reason.is_empty() {
                    level = level.max(a.level);
                    reason = a.reason;
                }
            }
        }
        let count = |f: fn(&Op) -> bool| ops.iter().filter(|o| f(o)).count();
        let parts: Vec<String> = [
            ("перенести", count(|o| matches!(o, Op::Move(..)))),
            ("скопировать", count(|o| matches!(o, Op::Copy(..)))),
            ("создать папок", count(|o| matches!(o, Op::Mkdir(..)))),
            ("в корзину", count(|o| matches!(o, Op::Trash(..)))),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(w, n)| format!("{w} {n}"))
        .collect();
        let mut lines: Vec<String> = ops.iter().take(SHOWN_OPS).map(Op::line).collect();
        if ops.len() > SHOWN_OPS {
            lines.push(format!("…и ещё {}", ops.len() - SHOWN_OPS));
        }
        let target = lines.join("\n");
        let mut a = Assessment::new(
            level,
            format!("Файлы пакетом: {}", parts.join(", ")),
            target.clone(),
            format!("{} действий одним подтверждением; {reason}", ops.len()),
        )
        // "Always" only ever covers this exact list.
        .with_scope(target.to_lowercase());
        if ops.iter().any(|o| matches!(o, Op::Trash(_))) {
            a = a.at_least(RiskLevel::High, "удаление в корзину");
        }
        Ok(a)
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let ops = parse_ops(&args, &self.user_profile)?;
        let total = ops.len();
        let trash = self.trash.clone();
        let stop = cancel.clone();
        let results = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for op in &ops {
                if stop.is_cancelled() {
                    break;
                }
                out.push((op.line(), run_op(op, trash.as_ref())));
            }
            out
        })
        .await
        .map_err(|e| failed(e.to_string()))?;
        let done = results.iter().filter(|(_, r)| r.is_ok()).count();
        let mut text = format!("Сделано {done} из {total}.");
        let failures: Vec<String> = results
            .iter()
            .filter_map(|(line, r)| r.as_ref().err().map(|e| format!("- {line}: {e}")))
            .collect();
        if !failures.is_empty() {
            text.push_str("\nНе вышло:\n");
            text.push_str(&failures.join("\n"));
        }
        if results.len() < total {
            text.push_str(&format!(
                "\nОстановлено, не выполнено {}.",
                total - results.len()
            ));
        }
        Ok(ToolOutput::new(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env, run};

    fn temp() -> (tempfile::TempDir, String) {
        let d = tempfile::tempdir().unwrap();
        let profile = d.path().display().to_string();
        (d, profile)
    }

    #[tokio::test]
    async fn list_filters_by_extension_and_goes_deep() {
        let (d, profile) = temp();
        std::fs::create_dir_all(d.path().join("Downloads/sub")).unwrap();
        std::fs::write(d.path().join("Downloads/a.pdf"), b"12345").unwrap();
        std::fs::write(d.path().join("Downloads/b.jpg"), b"1").unwrap();
        std::fs::write(d.path().join("Downloads/sub/c.PDF"), b"1").unwrap();
        let tool = ListFolderTool {
            user_profile: profile,
        };
        let flat = run(&tool, json!({"path": "~/Downloads"}))
            .await
            .unwrap()
            .text;
        assert!(flat.starts_with("Всего 3:"), "{flat}");
        assert!(flat.contains("a.pdf · 5 Б") && flat.contains("sub\\ · папка"));
        let deep = run(
            &tool,
            json!({"path": "~/Downloads", "pattern": "*.pdf", "recursive": true}),
        )
        .await
        .unwrap()
        .text;
        assert!(deep.starts_with("Всего 2:"), "{deep}");
        assert!(deep.contains("sub\\c.PDF") && !deep.contains("b.jpg"));
    }

    #[tokio::test]
    async fn batch_moves_copies_renames_and_resolves_name_clashes() {
        let (d, profile) = temp();
        let dl = d.path().join("Downloads");
        std::fs::create_dir_all(&dl).unwrap();
        std::fs::write(dl.join("a.pdf"), b"a").unwrap();
        std::fs::write(dl.join("b.pdf"), b"b").unwrap();
        std::fs::write(dl.join("c.txt"), b"c").unwrap();
        std::fs::create_dir_all(dl.join("PDF")).unwrap();
        std::fs::write(dl.join("PDF/a.pdf"), b"old").unwrap();
        let (env, f) = env();
        let tool = FileBatchTool {
            trash: env.trash.clone(),
            user_profile: profile,
        };
        let args = json!({"operations": [
            {"op": "mkdir", "path": "~/Downloads/PDF"},
            {"op": "move", "from": "~/Downloads/a.pdf", "to": "~/Downloads/PDF"},
            {"op": "move", "from": "~/Downloads/b.pdf", "to": "~/Downloads/PDF"},
            {"op": "copy", "from": "~/Downloads/c.txt", "to": "~/Downloads/c-copy.txt"},
            {"op": "rename", "from": "~/Downloads/missing.doc", "to": "~/Downloads/x.doc"},
            {"op": "trash", "path": "~/Downloads/c.txt"}
        ]});
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::High, "trash makes it high");
        assert!(a.summary.contains("перенести 3") && a.summary.contains("в корзину 1"));
        assert_eq!(a.target.lines().count(), 6);
        let out = run(&tool, args).await.unwrap().text;
        assert!(out.starts_with("Сделано 5 из 6."), "{out}");
        assert!(out.contains("missing.doc"));
        assert_eq!(std::fs::read(dl.join("PDF/a.pdf")).unwrap(), b"old");
        assert_eq!(std::fs::read(dl.join("PDF/a (2).pdf")).unwrap(), b"a");
        assert!(dl.join("PDF/b.pdf").exists() && !dl.join("b.pdf").exists());
        assert_eq!(std::fs::read(dl.join("c-copy.txt")).unwrap(), b"c");
        assert_eq!(f.trashed.lock().len(), 1);
    }

    #[test]
    fn batch_without_trash_is_medium_and_checks_arguments() {
        let (_d, profile) = temp();
        let (env, _) = env();
        let tool = FileBatchTool {
            trash: env.trash.clone(),
            user_profile: profile,
        };
        let a = tool
            .assess(&json!({"operations": [{"op": "move", "from": "~/a", "to": "~/b"}]}))
            .unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert!(tool.assess(&json!({"operations": []})).is_err());
        assert!(tool
            .assess(&json!({"operations": [{"op": "format", "path": "C:\\"}]}))
            .is_err());
        assert!(tool
            .assess(&json!({"operations": [{"op": "move", "from": "~/a"}]}))
            .is_err());
        let many: Vec<Value> = (0..201)
            .map(|i| json!({"op": "mkdir", "path": format!("~/d{i}")}))
            .collect();
        assert!(tool.assess(&json!({ "operations": many })).is_err());
        let sys = tool
            .assess(&json!({"operations": [{"op": "move", "from": "C:\\Windows\\a.dll", "to": "~/a.dll"}]}))
            .unwrap();
        assert_eq!(sys.level, RiskLevel::High);
    }

    #[test]
    fn batch_paths_must_be_full_without_dots_or_masks() {
        let profile = r"C:\Users\me";
        assert!(checked_path(r"C:\Users\me\Downloads\a.pdf", profile).is_ok());
        assert!(checked_path(r"~\Downloads\a.pdf", profile).is_ok());
        assert!(checked_path(r"%USERPROFILE%\a.pdf", profile).is_ok());
        // `..` folds into a plain path, and the risk is judged on that path.
        assert_eq!(
            checked_path(r"C:\Users\me\..\other\a.pdf", profile).unwrap(),
            PathBuf::from(r"C:\Users\other\a.pdf")
        );
        for bad in [
            r"Downloads\a.pdf",
            r"...\song.mp3",
            r"C:\Users\me\...\song.mp3",
            r"C:\Users\me\Downloads\*.pdf",
            "song.mp3",
        ] {
            assert!(checked_path(bad, profile).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_missing_folder_without_extension_takes_the_file_inside() {
        let (d, profile) = temp();
        std::fs::write(d.path().join("song.mp3"), b"m").unwrap();
        let (env, _) = env();
        let tool = FileBatchTool {
            trash: env.trash.clone(),
            user_profile: profile,
        };
        let out = run(
            &tool,
            json!({"operations": [{"op": "move", "from": "~/song.mp3", "to": "~/Музыка"}]}),
        )
        .await
        .unwrap();
        assert!(out.text.starts_with("Сделано 1 из 1."), "{}", out.text);
        assert!(d.path().join("Музыка").join("song.mp3").is_file());
    }

    #[test]
    fn free_name_counts_up() {
        let (d, _) = temp();
        let p = d.path().join("x.txt");
        assert_eq!(free_name(&p), p);
        std::fs::write(&p, b"").unwrap();
        std::fs::write(d.path().join("x (2).txt"), b"").unwrap();
        assert_eq!(free_name(&p), d.path().join("x (3).txt"));
    }
}
