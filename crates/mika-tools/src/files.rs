//! Files: find, read, write, move, create folders, delete to the Recycle Bin, open.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::paths::{class_label, classify_path, path_risk, PathAccess};
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::{Launcher, Trash};
use crate::util::{arg_bool, arg_u64, failed, resolve_path, schema, truncate};

const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;
const MAX_READ_CHARS: usize = 20_000;
const EXECUTABLE: &[&str] = &[
    "exe", "msi", "bat", "cmd", "ps1", "vbs", "vbe", "js", "jse", "wsf", "scr", "com", "lnk",
    "reg", "hta", "cpl", "jar",
];

pub(crate) fn path_assessment(
    path: &Path,
    user_profile: &str,
    access: PathAccess,
    summary: String,
    base: RiskLevel,
) -> Assessment {
    let shown = path.display().to_string();
    let class = classify_path(&shown, user_profile, false);
    let level = path_risk(class, access).max(base);
    Assessment::new(level, summary, shown.clone(), class_label(class))
        .with_scope(shown.to_lowercase())
}

pub(crate) fn path_arg(args: &Value, key: &str, profile: &str) -> Result<PathBuf, ToolError> {
    let raw = arg_str(args, key)?;
    if raw.trim().is_empty() {
        return Err(ToolError::BadArguments(format!("пустой путь `{key}`")));
    }
    Ok(resolve_path(raw, profile))
}

pub struct FindFilesTool {
    pub user_profile: String,
}

const SKIP_DIRS: &[&str] = &["appdata", "node_modules", ".git", "$recycle.bin", "target"];

fn walk(dir: &Path, query: &str, depth: usize, out: &mut Vec<PathBuf>, limit: usize) {
    if out.len() >= limit {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut subdirs = Vec::new();
    for e in rd.filter_map(Result::ok) {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_lowercase();
        if name.contains(query) {
            out.push(p.clone());
            if out.len() >= limit {
                return;
            }
        }
        if depth > 0
            && e.file_type().is_ok_and(|t| t.is_dir())
            && !SKIP_DIRS.contains(&name.as_str())
        {
            subdirs.push(p);
        }
    }
    for d in subdirs {
        walk(&d, query, depth - 1, out, limit);
    }
}

#[async_trait]
impl Tool for FindFilesTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "find_files",
            "Найти файлы и папки по части имени. По умолчанию ищет в Рабочем столе, Документах, Загрузках, Изображениях, Музыке и Видео.",
            schema(
                &[
                    ("query", "string", "Часть имени файла"),
                    ("folder", "string", "Где искать, необязательно"),
                    ("limit", "integer", "Сколько результатов, до 50"),
                ],
                &["query"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let q = arg_str(args, "query")?;
        match arg_str_opt(args, "folder") {
            Some(f) if !f.trim().is_empty() => {
                let p = resolve_path(f, &self.user_profile);
                Ok(path_assessment(
                    &p,
                    &self.user_profile,
                    PathAccess::Read,
                    format!("Найти «{q}»"),
                    RiskLevel::Low,
                )
                .with_scope("*"))
            }
            _ => Ok(Assessment::new(
                RiskLevel::Low,
                format!("Найти «{q}»"),
                "папки пользователя",
                "только чтение",
            )),
        }
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let query = arg_str(&args, "query")?.trim().to_lowercase();
        if query.is_empty() {
            return Err(ToolError::BadArguments("пустой запрос".into()));
        }
        let limit = arg_u64(&args, "limit").unwrap_or(20).clamp(1, 50) as usize;
        let roots: Vec<PathBuf> = match arg_str_opt(&args, "folder") {
            Some(f) if !f.trim().is_empty() => vec![resolve_path(f, &self.user_profile)],
            _ => [
                "Desktop",
                "Documents",
                "Downloads",
                "Pictures",
                "Music",
                "Videos",
            ]
            .iter()
            .map(|d| PathBuf::from(&self.user_profile).join(d))
            .collect(),
        };
        let found = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for r in roots {
                walk(&r, &query, 6, &mut out, limit);
            }
            out
        })
        .await
        .map_err(|e| failed(e.to_string()))?;
        if found.is_empty() {
            return Ok(ToolOutput::new("Ничего не нашла."));
        }
        Ok(ToolOutput::new(
            found
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }
}

pub struct ReadFileTool {
    pub user_profile: String,
}

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "read_file",
            "Прочитать текстовый файл.",
            schema(&[("path", "string", "Путь к файлу")], &["path"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let p = path_arg(args, "path", &self.user_profile)?;
        Ok(path_assessment(
            &p,
            &self.user_profile,
            PathAccess::Read,
            "Прочитать файл".into(),
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
        let meta = tokio::fs::metadata(&p)
            .await
            .map_err(|e| failed(format!("{}: {e}", p.display())))?;
        if meta.is_dir() {
            let mut rd = tokio::fs::read_dir(&p)
                .await
                .map_err(|e| failed(e.to_string()))?;
            let mut names = Vec::new();
            while let Ok(Some(e)) = rd.next_entry().await {
                names.push(e.file_name().to_string_lossy().into_owned());
                if names.len() >= 200 {
                    break;
                }
            }
            return Ok(ToolOutput::new(format!(
                "Это папка. Внутри:\n{}",
                names.join("\n")
            )));
        }
        if meta.len() > MAX_READ_BYTES {
            return Err(failed(format!(
                "файл слишком большой: {} МБ",
                meta.len() / 1024 / 1024
            )));
        }
        let bytes = tokio::fs::read(&p)
            .await
            .map_err(|e| failed(e.to_string()))?;
        if bytes.iter().take(4096).any(|b| *b == 0) {
            return Err(failed("это не текстовый файл"));
        }
        let text = String::from_utf8_lossy(&bytes);
        Ok(ToolOutput::new(truncate(
            text.trim_start_matches('\u{feff}'),
            MAX_READ_CHARS,
        )))
    }
}

pub struct WriteFileTool {
    pub user_profile: String,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "write_file",
            "Создать или перезаписать текстовый файл. append=true дописывает в конец.",
            schema(
                &[
                    ("path", "string", "Путь к файлу"),
                    ("text", "string", "Текст"),
                    ("append", "boolean", "Дописать в конец"),
                ],
                &["path", "text"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let p = path_arg(args, "path", &self.user_profile)?;
        arg_str(args, "text")?;
        let append = arg_bool(args, "append");
        let exists = p.exists();
        let summary = match (exists, append) {
            (true, true) => "Дописать в файл",
            (true, false) => "Перезаписать файл",
            _ => "Создать файл",
        };
        let mut a = path_assessment(
            &p,
            &self.user_profile,
            PathAccess::Write,
            summary.into(),
            RiskLevel::Medium,
        );
        if exists && !append {
            a.reason = format!("{}; старое содержимое пропадёт", a.reason);
        }
        Ok(a)
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let p = path_arg(&args, "path", &self.user_profile)?;
        let text = arg_str(&args, "text")?;
        if let Some(dir) = p.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| failed(e.to_string()))?;
        }
        if arg_bool(&args, "append") {
            use tokio::io::AsyncWriteExt;
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .await
                .map_err(|e| failed(e.to_string()))?;
            f.write_all(text.as_bytes())
                .await
                .map_err(|e| failed(e.to_string()))?;
        } else {
            tokio::fs::write(&p, text)
                .await
                .map_err(|e| failed(e.to_string()))?;
        }
        Ok(ToolOutput::new(format!("Записано: {}", p.display())))
    }
}

pub struct MovePathTool {
    pub user_profile: String,
}

#[async_trait]
impl Tool for MovePathTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "move_path",
            "Переместить или переименовать файл или папку.",
            schema(
                &[
                    ("from", "string", "Откуда"),
                    ("to", "string", "Куда (новый путь)"),
                ],
                &["from", "to"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let from = path_arg(args, "from", &self.user_profile)?;
        let to = path_arg(args, "to", &self.user_profile)?;
        let a = path_assessment(
            &from,
            &self.user_profile,
            PathAccess::Write,
            "Переместить".into(),
            RiskLevel::Medium,
        );
        let b = path_assessment(
            &to,
            &self.user_profile,
            PathAccess::Write,
            "Переместить".into(),
            RiskLevel::Medium,
        );
        let (level, reason) = if b.level > a.level {
            (b.level, b.reason)
        } else {
            (a.level, a.reason)
        };
        Ok(Assessment::new(
            level,
            "Переместить или переименовать",
            format!("{} → {}", from.display(), to.display()),
            reason,
        )
        .with_scope(format!("{} -> {}", a.scope, b.scope)))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let from = path_arg(&args, "from", &self.user_profile)?;
        let mut to = path_arg(&args, "to", &self.user_profile)?;
        if to.is_dir() {
            if let Some(name) = from.file_name() {
                to = to.join(name);
            }
        }
        if to.exists() {
            return Err(failed(format!("{} уже существует", to.display())));
        }
        tokio::fs::rename(&from, &to)
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(ToolOutput::new(format!("Готово: {}", to.display())))
    }
}

pub struct CreateFolderTool {
    pub user_profile: String,
}

#[async_trait]
impl Tool for CreateFolderTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "create_folder",
            "Создать папку.",
            schema(&[("path", "string", "Путь к папке")], &["path"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let p = path_arg(args, "path", &self.user_profile)?;
        Ok(path_assessment(
            &p,
            &self.user_profile,
            PathAccess::Write,
            "Создать папку".into(),
            RiskLevel::Medium,
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let p = path_arg(&args, "path", &self.user_profile)?;
        tokio::fs::create_dir_all(&p)
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(ToolOutput::new(format!("Папка создана: {}", p.display())))
    }
}

pub struct DeleteToTrashTool {
    pub trash: Arc<dyn Trash>,
    pub user_profile: String,
}

#[async_trait]
impl Tool for DeleteToTrashTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "delete_to_trash",
            "Удалить файл или папку в корзину. Из корзины можно восстановить.",
            schema(&[("path", "string", "Путь")], &["path"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let p = path_arg(args, "path", &self.user_profile)?;
        let mut a = path_assessment(
            &p,
            &self.user_profile,
            PathAccess::Delete,
            "Удалить в корзину".into(),
            RiskLevel::High,
        );
        a.reason = format!("удаление, {}", a.reason);
        Ok(a)
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let p = path_arg(&args, "path", &self.user_profile)?;
        self.trash.trash(&p).map_err(failed)?;
        Ok(ToolOutput::new(format!("В корзине: {}", p.display())))
    }
}

pub struct OpenPathTool {
    pub launcher: Arc<dyn Launcher>,
    pub user_profile: String,
}

#[async_trait]
impl Tool for OpenPathTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "open_path",
            "Открыть файл в программе по умолчанию или папку в Проводнике.",
            schema(&[("path", "string", "Путь")], &["path"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let p = path_arg(args, "path", &self.user_profile)?;
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let mut a = path_assessment(
            &p,
            &self.user_profile,
            PathAccess::Read,
            "Открыть".into(),
            RiskLevel::Low,
        );
        if EXECUTABLE.contains(&ext.as_str()) {
            a = a.at_least(
                RiskLevel::High,
                format!("файл .{ext} запускает программу или скрипт"),
            );
        }
        Ok(a)
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let p = path_arg(&args, "path", &self.user_profile)?;
        if !p.exists() {
            return Err(failed(format!("нет такого пути: {}", p.display())));
        }
        self.launcher
            .open(&p.display().to_string(), None)
            .map_err(failed)?;
        Ok(ToolOutput::new(format!("Открыла {}", p.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env, run};
    use serde_json::json;

    fn profile() -> (tempfile::TempDir, String) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().display().to_string();
        (d, p)
    }

    #[tokio::test]
    async fn find_files_matches_name_case_insensitive() {
        let (d, prof) = profile();
        std::fs::create_dir_all(d.path().join("Documents\\Отчёты")).unwrap();
        std::fs::write(d.path().join("Documents\\Отчёты\\Годовой ОТЧЁТ.docx"), b"").unwrap();
        std::fs::write(d.path().join("Documents\\other.txt"), b"").unwrap();
        let out = run(
            &FindFilesTool { user_profile: prof },
            json!({"query": "отчёт"}),
        )
        .await
        .unwrap()
        .text;
        assert!(out.contains("Отчёты"), "{out}");
        assert!(out.contains("Годовой ОТЧЁТ.docx"), "{out}");
        assert!(!out.contains("other.txt"));
    }

    #[tokio::test]
    async fn write_then_read_roundtrip_and_append() {
        let (_d, prof) = profile();
        let w = WriteFileTool {
            user_profile: prof.clone(),
        };
        let r = ReadFileTool {
            user_profile: prof.clone(),
        };
        run(
            &w,
            json!({"path": "Documents\\note.txt", "text": "строка 1\n"}),
        )
        .await
        .unwrap();
        let a = w
            .assess(&json!({"path": "Documents\\note.txt", "text": "x"}))
            .unwrap();
        assert!(a.reason.contains("пропадёт"));
        run(
            &w,
            json!({"path": "Documents\\note.txt", "text": "строка 2", "append": true}),
        )
        .await
        .unwrap();
        assert_eq!(
            run(&r, json!({"path": "Documents\\note.txt"}))
                .await
                .unwrap()
                .text,
            "строка 1\nстрока 2"
        );
    }

    #[tokio::test]
    async fn read_file_truncates_large_text_and_rejects_binary() {
        let (d, prof) = profile();
        std::fs::write(d.path().join("big.txt"), "я".repeat(30_000)).unwrap();
        std::fs::write(d.path().join("bin.dat"), [0u8, 1, 2, 3]).unwrap();
        let r = ReadFileTool { user_profile: prof };
        let out = run(&r, json!({"path": "big.txt"})).await.unwrap().text;
        assert_eq!(out.chars().count(), MAX_READ_CHARS + 1);
        assert!(run(&r, json!({"path": "bin.dat"})).await.is_err());
    }

    #[test]
    fn write_into_windows_via_dotdot_is_high() {
        let w = WriteFileTool {
            user_profile: "C:\\Users\\me".into(),
        };
        let a = w
            .assess(
                &json!({"path": "C:\\Users\\me\\..\\..\\Windows\\System32\\x.dll", "text": "x"}),
            )
            .unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(a.target, "C:\\Windows\\System32\\x.dll");
        let user = w
            .assess(&json!({"path": "Documents\\a.txt", "text": "x"}))
            .unwrap();
        assert_eq!(user.level, RiskLevel::Medium);
    }

    #[test]
    fn move_path_risk_is_max_of_both() {
        let t = MovePathTool {
            user_profile: "C:\\Users\\me".into(),
        };
        let a = t
            .assess(&json!({"from": "Documents\\a.txt", "to": "C:\\Program Files\\a.txt"}))
            .unwrap();
        assert_eq!(a.level, RiskLevel::High);
    }

    #[tokio::test]
    async fn delete_goes_to_trash_backend_and_is_high() {
        let (e, f) = env();
        let t = DeleteToTrashTool {
            trash: e.trash,
            user_profile: "C:\\Users\\me".into(),
        };
        assert_eq!(
            t.assess(&json!({"path": "Documents\\a.txt"}))
                .unwrap()
                .level,
            RiskLevel::High
        );
        run(&t, json!({"path": "Documents\\a.txt"})).await.unwrap();
        assert_eq!(
            *f.trashed.lock(),
            vec![PathBuf::from("C:\\Users\\me\\Documents\\a.txt")]
        );
    }

    #[test]
    fn open_path_exe_is_high_and_document_low() {
        let (e, _) = env();
        let t = OpenPathTool {
            launcher: e.launcher,
            user_profile: "C:\\Users\\me".into(),
        };
        assert_eq!(
            t.assess(&json!({"path": "Downloads\\setup.exe"}))
                .unwrap()
                .level,
            RiskLevel::High
        );
        assert_eq!(
            t.assess(&json!({"path": "Downloads\\run.PS1"}))
                .unwrap()
                .level,
            RiskLevel::High
        );
        assert_eq!(
            t.assess(&json!({"path": "Documents\\a.pdf"}))
                .unwrap()
                .level,
            RiskLevel::Low
        );
    }
}
