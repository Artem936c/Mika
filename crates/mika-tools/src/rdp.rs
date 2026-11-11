//! Saved Remote Desktop connections.

use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::{Launcher, RdpSource};
use crate::util::{failed, schema};

pub struct RdpListTool(pub Arc<dyn RdpSource>);

#[async_trait]
impl Tool for RdpListTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "rdp_list",
            "Список сохранённых подключений к удалённому рабочему столу.",
            schema(&[], &[]),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть подключения RDP",
            "история подключений",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let mut lines: Vec<String> = self
            .0
            .saved_hosts()
            .into_iter()
            .map(|h| format!("сервер: {h}"))
            .collect();
        lines.extend(
            self.0
                .rdp_files()
                .into_iter()
                .map(|p| format!("файл: {}", p.display())),
        );
        if lines.is_empty() {
            return Ok(ToolOutput::new("Сохранённых подключений нет."));
        }
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

pub struct RdpOpenTool {
    pub source: Arc<dyn RdpSource>,
    pub launcher: Arc<dyn Launcher>,
}

impl RdpOpenTool {
    /// Arguments for mstsc: a matching `.rdp` file, or `/v:host`.
    fn mstsc_args(&self, target: &str) -> Result<String, ToolError> {
        let t = target.trim();
        if t.is_empty() || t.contains(['"', ' ', '&', '|']) && !t.to_lowercase().ends_with(".rdp") {
            return Err(ToolError::BadArguments("неверное имя сервера".into()));
        }
        let lower = t.to_lowercase();
        if let Some(file) = self.source.rdp_files().into_iter().find(|p| {
            p.file_stem()
                .is_some_and(|s| s.to_string_lossy().to_lowercase() == lower)
                || p.display().to_string().to_lowercase() == lower
        }) {
            return Ok(format!("\"{}\"", file.display()));
        }
        Ok(format!("/v:{t}"))
    }
}

#[async_trait]
impl Tool for RdpOpenTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "rdp_open",
            "Подключиться к удалённому рабочему столу: имя сервера, адрес или название сохранённого .rdp файла.",
            schema(&[("target", "string", "Сервер или имя подключения")], &["target"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let target = arg_str(args, "target")?;
        let a = self.mstsc_args(target)?;
        Ok(Assessment::new(
            RiskLevel::Medium,
            "Подключиться к удалённому рабочему столу",
            format!("mstsc {a}"),
            "откроет сеанс на другом компьютере",
        )
        .with_scope(target.trim().to_lowercase()))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let target = arg_str(&args, "target")?;
        let a = self.mstsc_args(target)?;
        self.launcher.open("mstsc.exe", Some(&a)).map_err(failed)?;
        Ok(ToolOutput::new(format!("Открываю подключение к {target}.")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env, run};
    use serde_json::json;
    use std::path::PathBuf;

    #[tokio::test]
    async fn rdp_list_merges_hosts_and_files() {
        let (e, f) = env();
        *f.hosts.lock() = vec!["srv-01".into()];
        *f.rdp_files.lock() = vec![PathBuf::from("C:\\Users\\me\\Documents\\Работа.rdp")];
        let out = run(&RdpListTool(e.rdp), json!({})).await.unwrap().text;
        assert_eq!(
            out,
            "сервер: srv-01\nфайл: C:\\Users\\me\\Documents\\Работа.rdp"
        );
    }

    #[tokio::test]
    async fn rdp_open_passes_host_or_file_to_mstsc() {
        let (e, f) = env();
        *f.rdp_files.lock() = vec![PathBuf::from("C:\\Users\\me\\Documents\\Работа.rdp")];
        let t = RdpOpenTool {
            source: e.rdp,
            launcher: e.launcher,
        };
        assert_eq!(
            t.assess(&json!({"target": "srv-01"})).unwrap().level,
            RiskLevel::Medium
        );
        run(&t, json!({"target": "srv-01"})).await.unwrap();
        run(&t, json!({"target": "работа"})).await.unwrap();
        let opened = f.opened.lock();
        assert_eq!(opened[0], ("mstsc.exe".into(), Some("/v:srv-01".into())));
        assert_eq!(
            opened[1].1.as_deref(),
            Some("\"C:\\Users\\me\\Documents\\Работа.rdp\"")
        );
        assert!(t.assess(&json!({"target": "x & calc"})).is_err());
    }
}
