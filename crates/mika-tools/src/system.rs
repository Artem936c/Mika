//! Volume, power, clipboard, system facts, processes.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::{Clipboard, Power, PowerAction, Runner, SystemProbe, Volume};
use crate::util::{arg_u64, failed, schema, truncate};

pub struct VolumeTool(pub Arc<dyn Volume>);

#[async_trait]
impl Tool for VolumeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "volume",
            "Громкость звука Windows: узнать, установить в процентах, громче, тише, выключить или включить звук.",
            schema(
                &[
                    ("action", "string", "one of: get, set, up, down, mute, unmute"),
                    ("level", "integer", "Громкость 0–100 для set"),
                ],
                &["action"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let action = arg_str(args, "action")?;
        let summary = match action {
            "get" => "Узнать громкость".to_string(),
            "set" => {
                let level = arg_u64(args, "level")
                    .ok_or_else(|| ToolError::BadArguments("нужен level 0–100".into()))?;
                format!("Громкость {}%", level.min(100))
            }
            "up" => "Сделать громче".into(),
            "down" => "Сделать тише".into(),
            "mute" => "Выключить звук".into(),
            "unmute" => "Включить звук".into(),
            other => {
                return Err(ToolError::BadArguments(format!(
                    "неизвестное действие {other}"
                )))
            }
        };
        Ok(Assessment::new(
            RiskLevel::Low,
            summary,
            "громкость Windows",
            "легко вернуть как было",
        )
        .with_scope(action))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let (level, muted) = self.0.get().map_err(failed)?;
        let action = arg_str(&args, "action")?;
        let target = match action {
            "set" => arg_u64(&args, "level").unwrap_or(level as u64).min(100) as u8,
            "up" => level.saturating_add(10).min(100),
            "down" => level.saturating_sub(10),
            "mute" | "unmute" => {
                self.0.set_mute(action == "mute").map_err(failed)?;
                let state = if action == "mute" {
                    "выключен"
                } else {
                    "включён"
                };
                return Ok(ToolOutput::new(format!(
                    "Звук {state}, громкость {level}%."
                )));
            }
            _ => {
                let m = if muted {
                    ", звук выключен"
                } else {
                    ""
                };
                return Ok(ToolOutput::new(format!("Громкость {level}%{m}.")));
            }
        };
        self.0.set(target).map_err(failed)?;
        if muted && target > 0 {
            self.0.set_mute(false).map_err(failed)?;
        }
        Ok(ToolOutput::new(format!(
            "Громкость {target}% (была {level}%)."
        )))
    }
}

pub struct PowerTool(pub Arc<dyn Power>);

fn power_action(s: &str) -> Result<(PowerAction, RiskLevel, &'static str), ToolError> {
    Ok(match s {
        "lock" => (
            PowerAction::Lock,
            RiskLevel::Medium,
            "Заблокировать компьютер",
        ),
        "sleep" => (
            PowerAction::Sleep,
            RiskLevel::Medium,
            "Перевести компьютер в сон",
        ),
        "restart" => (
            PowerAction::Restart,
            RiskLevel::High,
            "Перезагрузить компьютер",
        ),
        "shutdown" => (
            PowerAction::Shutdown,
            RiskLevel::High,
            "Выключить компьютер",
        ),
        "logoff" => (
            PowerAction::Logoff,
            RiskLevel::High,
            "Выйти из учётной записи",
        ),
        other => {
            return Err(ToolError::BadArguments(format!(
                "неизвестное действие {other}"
            )))
        }
    })
}

#[async_trait]
impl Tool for PowerTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "power",
            "Блокировка, сон, перезагрузка, выключение компьютера, выход из учётной записи.",
            schema(
                &[(
                    "action",
                    "string",
                    "one of: lock, sleep, restart, shutdown, logoff",
                )],
                &["action"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let name = arg_str(args, "action")?;
        let (_, level, summary) = power_action(name)?;
        let reason = if level == RiskLevel::High {
            "закроет все программы, несохранённые данные пропадут"
        } else {
            "прервёт текущую работу"
        };
        Ok(Assessment::new(level, summary, "этот компьютер", reason).with_scope(name))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let (action, _, summary) = power_action(arg_str(&args, "action")?)?;
        self.0.act(action).map_err(failed)?;
        let when = match action {
            PowerAction::Restart | PowerAction::Shutdown => " через 5 секунд",
            _ => "",
        };
        Ok(ToolOutput::new(format!("{summary}{when}: выполняю.")))
    }
}

pub struct ClipboardTool(pub Arc<dyn Clipboard>);

#[async_trait]
impl Tool for ClipboardTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "clipboard",
            "Прочитать текст из буфера обмена или положить текст в буфер обмена.",
            schema(
                &[
                    ("action", "string", "one of: get, set"),
                    ("text", "string", "Текст для set"),
                ],
                &["action"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        match arg_str(args, "action")? {
            "get" => Ok(Assessment::new(
                RiskLevel::Low,
                "Прочитать буфер обмена",
                "буфер обмена",
                "только чтение",
            )
            .with_scope("get")),
            "set" => {
                let text = arg_str(args, "text")?;
                Ok(Assessment::new(
                    RiskLevel::Medium,
                    "Заменить текст в буфере обмена",
                    truncate(text, 200),
                    "старое содержимое буфера пропадёт",
                )
                .with_scope("set"))
            }
            other => Err(ToolError::BadArguments(format!(
                "неизвестное действие {other}"
            ))),
        }
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        if arg_str(&args, "action")? == "set" {
            self.0.set(arg_str(&args, "text")?).map_err(failed)?;
            return Ok(ToolOutput::new("Текст в буфере обмена."));
        }
        let text = self.0.get().map_err(failed)?;
        if text.is_empty() {
            return Ok(ToolOutput::new("В буфере обмена нет текста."));
        }
        Ok(ToolOutput::new(truncate(&text, 4000)))
    }
}

pub struct SystemInfoTool(pub Arc<dyn SystemProbe>);

#[async_trait]
impl Tool for SystemInfoTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "system_info",
            "Нагрузка процессора, память, свободное место на дисках, время работы, версия Windows.",
            schema(&[], &[]),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть состояние системы",
            "этот компьютер",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let probe = self.0.clone();
        let f = tokio::task::spawn_blocking(move || probe.facts())
            .await
            .map_err(|e| failed(e.to_string()))?;
        let disks: Vec<String> = f
            .disks
            .iter()
            .map(|(m, free, total)| format!("{m} свободно {free:.0} из {total:.0} ГБ"))
            .collect();
        Ok(ToolOutput::new(format!(
            "{} ({}). Процессор {:.0}%. Память {} из {} МБ. Работает {} ч {} мин. Диски: {}.",
            f.os,
            f.host,
            f.cpu_percent,
            f.memory_used_mb,
            f.memory_total_mb,
            f.uptime_s / 3600,
            f.uptime_s % 3600 / 60,
            disks.join("; ")
        )))
    }
}

pub struct ProcessesTool(pub Arc<dyn SystemProbe>);

#[async_trait]
impl Tool for ProcessesTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "list_processes",
            "Самые тяжёлые процессы по памяти или процессору.",
            schema(
                &[
                    ("sort", "string", "one of: memory, cpu"),
                    ("limit", "integer", "Сколько показать, до 30"),
                ],
                &[],
            ),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть процессы",
            "список процессов",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let probe = self.0.clone();
        let mut list = tokio::task::spawn_blocking(move || probe.processes())
            .await
            .map_err(|e| failed(e.to_string()))?;
        let by_cpu = arg_str_opt(&args, "sort") == Some("cpu");
        if by_cpu {
            list.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
        } else {
            list.sort_by_key(|p| std::cmp::Reverse(p.memory_mb));
        }
        let limit = arg_u64(&args, "limit").unwrap_or(10).clamp(1, 30) as usize;
        let lines: Vec<String> = list
            .iter()
            .take(limit)
            .map(|p| {
                format!(
                    "{} (pid {}): {} МБ, ЦП {:.1}%",
                    p.name, p.pid, p.memory_mb, p.cpu_percent
                )
            })
            .collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

pub struct KillProcessTool(pub Arc<dyn Runner>);

#[async_trait]
impl Tool for KillProcessTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "kill_process",
            "Принудительно завершить процесс по имени (chrome.exe) или pid.",
            schema(&[("name", "string", "Имя процесса или pid")], &["name"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let name = arg_str(args, "name")?.trim();
        if name.is_empty() || name.contains(['"', '&', '|', ';']) {
            return Err(ToolError::BadArguments("неверное имя процесса".into()));
        }
        Ok(Assessment::new(
            RiskLevel::High,
            "Принудительно завершить процесс",
            name,
            "несохранённые данные программы пропадут",
        )
        .with_scope(name.to_lowercase()))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let name = arg_str(&args, "name")?.trim().to_string();
        let mut a = vec!["/F".to_string()];
        if name.chars().all(|c| c.is_ascii_digit()) {
            a.extend(["/PID".into(), name]);
        } else {
            let exe = if name.to_lowercase().ends_with(".exe") {
                name
            } else {
                format!("{name}.exe")
            };
            a.extend(["/IM".into(), exe]);
        }
        let out = self
            .0
            .run("taskkill", &a, Duration::from_secs(20), cancel)
            .await
            .map_err(failed)?;
        let text = format!("{}{}", out.stdout.trim(), out.stderr.trim());
        if out.code == Some(0) {
            Ok(ToolOutput::new(text))
        } else {
            Err(failed(text))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env, run};
    use serde_json::json;

    #[tokio::test]
    async fn volume_up_adds_ten_and_clamps() {
        let (e, f) = env();
        *f.volume.lock() = (95, true);
        let t = VolumeTool(e.volume.clone());
        let out = run(&t, json!({"action": "up"})).await.unwrap();
        assert_eq!(*f.volume.lock(), (100, false));
        assert!(out.text.contains("100%"));
        run(&t, json!({"action": "set", "level": 30}))
            .await
            .unwrap();
        assert_eq!(f.volume.lock().0, 30);
        run(&t, json!({"action": "down"})).await.unwrap();
        assert_eq!(f.volume.lock().0, 20);
    }

    #[test]
    fn volume_assess_is_low_and_checks_level() {
        let (e, _) = env();
        let t = VolumeTool(e.volume);
        assert_eq!(
            t.assess(&json!({"action": "mute"})).unwrap().level,
            RiskLevel::Low
        );
        assert!(t.assess(&json!({"action": "set"})).is_err());
        assert!(t.assess(&json!({"action": "explode"})).is_err());
    }

    #[test]
    fn power_restart_is_high_and_lock_is_medium() {
        let (e, _) = env();
        let t = PowerTool(e.power);
        assert_eq!(
            t.assess(&json!({"action": "restart"})).unwrap().level,
            RiskLevel::High
        );
        assert_eq!(
            t.assess(&json!({"action": "shutdown"})).unwrap().level,
            RiskLevel::High
        );
        assert_eq!(
            t.assess(&json!({"action": "lock"})).unwrap().level,
            RiskLevel::Medium
        );
    }

    #[tokio::test]
    async fn power_run_calls_backend() {
        let (e, f) = env();
        run(&PowerTool(e.power), json!({"action": "restart"}))
            .await
            .unwrap();
        assert_eq!(*f.power.lock(), vec![PowerAction::Restart]);
    }

    #[tokio::test]
    async fn clipboard_set_is_medium_and_get_low() {
        let (e, f) = env();
        let t = ClipboardTool(e.clipboard);
        assert_eq!(
            t.assess(&json!({"action": "set", "text": "x"}))
                .unwrap()
                .level,
            RiskLevel::Medium
        );
        assert_eq!(
            t.assess(&json!({"action": "get"})).unwrap().level,
            RiskLevel::Low
        );
        run(&t, json!({"action": "set", "text": "привет"}))
            .await
            .unwrap();
        assert_eq!(*f.clipboard.lock(), "привет");
        assert_eq!(
            run(&t, json!({"action": "get"})).await.unwrap().text,
            "привет"
        );
    }

    #[tokio::test]
    async fn system_info_and_processes_format() {
        let (e, _) = env();
        let info = run(&SystemInfoTool(e.probe.clone()), json!({}))
            .await
            .unwrap()
            .text;
        assert!(
            info.contains("Процессор 12%") || info.contains("Процессор 13%"),
            "{info}"
        );
        assert!(info.contains("C:\\ свободно 100 из 476 ГБ"));
        let p = run(&ProcessesTool(e.probe), json!({"sort": "cpu", "limit": 1}))
            .await
            .unwrap()
            .text;
        assert!(p.starts_with("game.exe"), "{p}");
    }

    #[tokio::test]
    async fn kill_process_is_high_and_uses_taskkill() {
        let (e, f) = env();
        let t = KillProcessTool(e.runner);
        assert_eq!(
            t.assess(&json!({"name": "chrome"})).unwrap().level,
            RiskLevel::High
        );
        assert!(t.assess(&json!({"name": "a & calc"})).is_err());
        f.run_output.lock().code = Some(0);
        run(&t, json!({"name": "chrome"})).await.unwrap();
        let runs = f.runs.lock();
        assert_eq!(runs[0].0, "taskkill");
        assert_eq!(runs[0].1, vec!["/F", "/IM", "chrome.exe"]);
    }
}
