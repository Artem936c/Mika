//! PowerShell and cmd with a time limit, captured output and stop.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::command::{classify_command, Shell};
use mika_core::tools::{arg_str, arg_str_opt, spec, Assessment, Tool, ToolError, ToolOutput};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::Runner;
use crate::util::{arg_u64, failed, schema, truncate};

pub const DEFAULT_TIMEOUT_S: u64 = 60;
pub const MAX_TIMEOUT_S: u64 = 600;
const MAX_OUTPUT: usize = 16_000;

pub struct RunShellTool(pub Arc<dyn Runner>);

fn shell_of(args: &Value) -> Result<Shell, ToolError> {
    match arg_str_opt(args, "shell").unwrap_or("powershell") {
        "powershell" | "pwsh" => Ok(Shell::PowerShell),
        "cmd" => Ok(Shell::Cmd),
        other => Err(ToolError::BadArguments(format!(
            "неизвестная оболочка {other}"
        ))),
    }
}

/// Program and arguments. PowerShell output is forced to UTF-8; cmd output is decoded by the runner.
pub fn command_line(shell: Shell, command: &str) -> (String, Vec<String>) {
    match shell {
        Shell::PowerShell => (
            "powershell.exe".into(),
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-Command".into(),
                format!("[Console]::OutputEncoding=[Text.Encoding]::UTF8; $ProgressPreference='SilentlyContinue'; {command}"),
            ],
        ),
        // `/U`: cmd's own commands write UTF-16 to the pipe. Without it, on a Windows
        // with another language the OEM code page has no Cyrillic and turns it into
        // question marks; `chcp` does not help a cmd that has no console.
        Shell::Cmd => (
            "cmd.exe".into(),
            vec!["/D".into(), "/U".into(), "/C".into(), command.to_string()],
        ),
    }
}

#[async_trait]
impl Tool for RunShellTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "run_shell",
            "Выполнить команду PowerShell или cmd и вернуть вывод. Для чтения состояния и действий, для которых нет отдельного инструмента.",
            schema(
                &[
                    ("shell", "string", "one of: powershell, cmd"),
                    ("command", "string", "Команда"),
                    ("timeout_s", "integer", "Лимит времени в секундах, по умолчанию 60, до 600"),
                ],
                &["command"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let shell = shell_of(args)?;
        let command = arg_str(args, "command")?;
        let risk = classify_command(shell, command);
        let name = match shell {
            Shell::PowerShell => "PowerShell",
            Shell::Cmd => "cmd",
        };
        Ok(Assessment::new(
            risk.level,
            format!("Выполнить команду {name}"),
            command,
            risk.reasons.join("; "),
        )
        .with_scope(risk.normalized)
        .with_spoken("Выполнить команду"))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let shell = shell_of(&args)?;
        let command = arg_str(&args, "command")?;
        let timeout = arg_u64(&args, "timeout_s")
            .unwrap_or(DEFAULT_TIMEOUT_S)
            .clamp(1, MAX_TIMEOUT_S);
        let (program, argv) = command_line(shell, command);
        let out = self
            .0
            .run(&program, &argv, Duration::from_secs(timeout), cancel)
            .await
            .map_err(|e| {
                if e == "остановлено" {
                    ToolError::Cancelled
                } else {
                    failed(e)
                }
            })?;
        if out.timed_out {
            return Err(failed(format!(
                "превышено время {timeout} с, команда остановлена"
            )));
        }
        let mut text = format!(
            "код выхода {}",
            out.code.map_or("?".into(), |c| c.to_string())
        );
        if !out.stdout.trim().is_empty() {
            text.push_str(&format!("\n--- вывод ---\n{}", out.stdout.trim_end()));
        }
        if !out.stderr.trim().is_empty() {
            text.push_str(&format!("\n--- ошибки ---\n{}", out.stderr.trim_end()));
        }
        Ok(ToolOutput::new(truncate(&text, MAX_OUTPUT)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::RunOutput;
    use crate::testing::{env, run};
    use crate::win::ProcessRunner;
    use mika_core::tools::RiskLevel;
    use serde_json::json;

    #[test]
    fn shell_risk_comes_from_classifier() {
        let (e, _) = env();
        let t = RunShellTool(e.runner);
        let read = t.assess(&json!({"command": "Get-Process"})).unwrap();
        assert_eq!(read.level, RiskLevel::Medium);
        assert_eq!(read.scope, "get-process");
        let bad = t
            .assess(&json!({"shell": "cmd", "command": "del /s /q C:\\x"}))
            .unwrap();
        assert_eq!(bad.level, RiskLevel::High);
        // The card names the shell; aloud it is just «a command».
        assert_eq!(read.summary, "Выполнить команду PowerShell");
        assert_eq!(bad.summary, "Выполнить команду cmd");
        assert_eq!(read.spoken_name(), "Выполнить команду");
        assert_eq!(bad.spoken_name(), "Выполнить команду");
        assert!(t
            .assess(&json!({"shell": "bash", "command": "ls"}))
            .is_err());
    }

    #[tokio::test]
    async fn shell_output_contains_code_stdout_stderr() {
        let (e, f) = env();
        *f.run_output.lock() = RunOutput {
            code: Some(1),
            stdout: "out\n".into(),
            stderr: "err\n".into(),
            timed_out: false,
        };
        let text = run(&RunShellTool(e.runner), json!({"command": "Get-Date"}))
            .await
            .unwrap()
            .text;
        assert_eq!(
            text,
            "код выхода 1\n--- вывод ---\nout\n--- ошибки ---\nerr"
        );
        let runs = f.runs.lock();
        assert_eq!(runs[0].0, "powershell.exe");
        assert!(runs[0].1.last().unwrap().ends_with("Get-Date"));
        assert_eq!(runs[0].2, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn timeout_is_reported_and_capped() {
        let (e, f) = env();
        f.run_output.lock().timed_out = true;
        let err = run(
            &RunShellTool(e.runner),
            json!({"command": "Get-Date", "timeout_s": 5000}),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("превышено время 600"), "{err}");
        assert_eq!(f.runs.lock()[0].2, Duration::from_secs(600));
    }

    #[tokio::test]
    async fn real_runner_echo_works() {
        let (program, args) = command_line(Shell::Cmd, "echo привет");
        let out = ProcessRunner
            .run(
                &program,
                &args,
                Duration::from_secs(20),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.code, Some(0));
        assert_eq!(out.stdout.trim(), "привет");
    }

    #[tokio::test]
    async fn real_runner_passes_quoted_paths_to_cmd() {
        let (program, args) =
            command_line(Shell::Cmd, r#"dir /b "C:\Program Files" >nul && echo ok"#);
        let out = ProcessRunner
            .run(
                &program,
                &args,
                Duration::from_secs(20),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout.trim(), "ok", "{out:?}");
    }

    #[tokio::test]
    async fn real_runner_powershell_utf8() {
        let (program, args) = command_line(Shell::PowerShell, "Write-Output 'привет \"мир\"'");
        let out = ProcessRunner
            .run(
                &program,
                &args,
                Duration::from_secs(30),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout.trim(), "привет \"мир\"", "{out:?}");
    }

    #[tokio::test]
    async fn real_runner_kills_on_timeout() {
        let (program, args) = command_line(Shell::Cmd, "ping -n 30 127.0.0.1 >nul");
        let started = std::time::Instant::now();
        let out = ProcessRunner
            .run(
                &program,
                &args,
                Duration::from_millis(500),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
