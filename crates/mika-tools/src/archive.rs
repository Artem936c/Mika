//! Zip archives through `tar.exe`, which ships with Windows 10 and 11.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::paths::PathAccess;
use mika_core::tools::{spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::files::{path_arg, path_assessment};
use crate::sys::Runner;
use crate::util::{failed, schema, truncate};

const TIMEOUT: Duration = Duration::from_secs(600);

async fn tar(
    runner: &dyn Runner,
    args: Vec<String>,
    cancel: CancellationToken,
) -> Result<(), ToolError> {
    let out = runner
        .run("tar.exe", &args, TIMEOUT, cancel)
        .await
        .map_err(failed)?;
    if out.timed_out {
        return Err(failed("архиватор не уложился в 10 минут"));
    }
    if out.code != Some(0) {
        return Err(failed(format!(
            "архиватор сообщил об ошибке: {}",
            truncate(out.stderr.trim(), 300)
        )));
    }
    Ok(())
}

/// The riskier of two assessments, with a common summary and target.
fn pair(a: Assessment, b: Assessment, summary: &str, target: String) -> Assessment {
    let (level, reason) = if b.level > a.level {
        (b.level, b.reason)
    } else {
        (a.level, a.reason)
    };
    Assessment::new(level, summary, target, reason)
        .with_scope(format!("{} -> {}", a.scope, b.scope))
}

pub struct ArchiveCreateTool {
    pub runner: Arc<dyn Runner>,
    pub user_profile: String,
}

#[async_trait]
impl Tool for ArchiveCreateTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "archive_create",
            "Упаковать файл или папку в архив zip.",
            schema(
                &[
                    ("source", "string", "Файл или папка"),
                    ("archive", "string", "Путь нового архива .zip"),
                ],
                &["source", "archive"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let source = path_arg(args, "source", &self.user_profile)?;
        let archive = path_arg(args, "archive", &self.user_profile)?;
        let is_zip = archive
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
        if !is_zip {
            return Err(ToolError::BadArguments(
                "имя архива должно кончаться на .zip".into(),
            ));
        }
        let target = format!("{} → {}", source.display(), archive.display());
        Ok(pair(
            path_assessment(
                &source,
                &self.user_profile,
                PathAccess::Read,
                String::new(),
                RiskLevel::Low,
            ),
            path_assessment(
                &archive,
                &self.user_profile,
                PathAccess::Write,
                String::new(),
                RiskLevel::Medium,
            ),
            "Упаковать в архив",
            target,
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let source = path_arg(&args, "source", &self.user_profile)?;
        let archive = path_arg(&args, "archive", &self.user_profile)?;
        if !source.exists() {
            return Err(failed(format!("нет {}", source.display())));
        }
        if archive.exists() {
            return Err(failed(format!("{} уже существует", archive.display())));
        }
        let (Some(parent), Some(name)) = (source.parent(), source.file_name()) else {
            return Err(failed("нельзя упаковать корень диска"));
        };
        tar(
            self.runner.as_ref(),
            vec![
                "-a".into(),
                "-c".into(),
                "-f".into(),
                archive.display().to_string(),
                "-C".into(),
                parent.display().to_string(),
                name.to_string_lossy().into_owned(),
            ],
            cancel,
        )
        .await?;
        Ok(ToolOutput::new(format!(
            "Архив готов: {}",
            archive.display()
        )))
    }
}

pub struct ArchiveExtractTool {
    pub runner: Arc<dyn Runner>,
    pub user_profile: String,
}

#[async_trait]
impl Tool for ArchiveExtractTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "archive_extract",
            "Распаковать архив zip, tar, tar.gz или 7z в папку.",
            schema(
                &[
                    ("archive", "string", "Путь к архиву"),
                    ("folder", "string", "Куда распаковать"),
                ],
                &["archive", "folder"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let archive = path_arg(args, "archive", &self.user_profile)?;
        let folder = path_arg(args, "folder", &self.user_profile)?;
        let target = format!("{} → {}", archive.display(), folder.display());
        Ok(pair(
            path_assessment(
                &archive,
                &self.user_profile,
                PathAccess::Read,
                String::new(),
                RiskLevel::Low,
            ),
            path_assessment(
                &folder,
                &self.user_profile,
                PathAccess::Write,
                String::new(),
                RiskLevel::Medium,
            ),
            "Распаковать архив",
            target,
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let archive = path_arg(&args, "archive", &self.user_profile)?;
        let folder = path_arg(&args, "folder", &self.user_profile)?;
        if !archive.is_file() {
            return Err(failed(format!("нет архива {}", archive.display())));
        }
        tokio::fs::create_dir_all(&folder)
            .await
            .map_err(|e| failed(e.to_string()))?;
        // -k: files that already exist stay as they are.
        tar(
            self.runner.as_ref(),
            vec![
                "-x".into(),
                "-k".into(),
                "-f".into(),
                archive.display().to_string(),
                "-C".into(),
                folder.display().to_string(),
            ],
            cancel,
        )
        .await?;
        Ok(ToolOutput::new(format!(
            "Распаковано в {}",
            folder.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::RunOutput;
    use crate::testing::{env_with, run};
    use mika_core::storage::Db;
    use serde_json::json;

    fn ok() -> RunOutput {
        RunOutput {
            code: Some(0),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn create_packs_the_item_relative_to_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().to_string_lossy().into_owned();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        let (e, f) = env_with(Arc::new(Db::open_in_memory().unwrap()), &profile);
        *f.run_output.lock() = ok();
        let tool = ArchiveCreateTool {
            runner: e.runner.clone(),
            user_profile: profile.clone(),
        };
        run(&tool, json!({"source": "docs", "archive": "docs.zip"}))
            .await
            .unwrap();
        let runs = f.runs.lock();
        assert_eq!(runs[0].0, "tar.exe");
        assert_eq!(runs[0].1[..3], ["-a", "-c", "-f"]);
        assert!(runs[0].1[3].ends_with("docs.zip"));
        assert_eq!(runs[0].1[6], "docs");
    }

    #[tokio::test]
    async fn create_refuses_missing_source_and_wrong_extension() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().to_string_lossy().into_owned();
        let (e, f) = env_with(Arc::new(Db::open_in_memory().unwrap()), &profile);
        let tool = ArchiveCreateTool {
            runner: e.runner.clone(),
            user_profile: profile,
        };
        assert!(run(&tool, json!({"source": "nope", "archive": "a.zip"}))
            .await
            .is_err());
        assert!(matches!(
            tool.assess(&json!({"source": "x", "archive": "a.rar"})),
            Err(ToolError::BadArguments(_))
        ));
        assert!(f.runs.lock().is_empty());
    }

    #[tokio::test]
    async fn extract_reports_archiver_error() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().to_string_lossy().into_owned();
        std::fs::write(dir.path().join("a.zip"), b"PK").unwrap();
        let (e, f) = env_with(Arc::new(Db::open_in_memory().unwrap()), &profile);
        let tool = ArchiveExtractTool {
            runner: e.runner.clone(),
            user_profile: profile,
        };
        *f.run_output.lock() = RunOutput {
            code: Some(1),
            stderr: "Unrecognized archive format".into(),
            ..Default::default()
        };
        let err = run(&tool, json!({"archive": "a.zip", "folder": "out"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Unrecognized archive format"));
        *f.run_output.lock() = ok();
        run(&tool, json!({"archive": "a.zip", "folder": "out"}))
            .await
            .unwrap();
        assert!(dir.path().join("out").is_dir());
    }

    #[test]
    fn writing_into_system_folder_is_high_risk() {
        let (e, _) = env_with(Arc::new(Db::open_in_memory().unwrap()), "C:\\Users\\me");
        let tool = ArchiveExtractTool {
            runner: e.runner.clone(),
            user_profile: "C:\\Users\\me".into(),
        };
        let a = tool
            .assess(&json!({"archive": "a.zip", "folder": "C:\\Windows\\x"}))
            .unwrap();
        assert_eq!(a.level, RiskLevel::High);
        let b = tool
            .assess(&json!({"archive": "a.zip", "folder": "Documents\\x"}))
            .unwrap();
        assert_eq!(b.level, RiskLevel::Medium);
    }
}
