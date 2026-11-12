//! Wi-Fi, Bluetooth and screen brightness.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::{RadioKind, Radios, Runner};
use crate::util::{arg_u64, failed, schema};

fn radio_kind(s: &str) -> Result<(RadioKind, &'static str), ToolError> {
    match s {
        "wifi" => Ok((RadioKind::Wifi, "Wi-Fi")),
        "bluetooth" => Ok((RadioKind::Bluetooth, "Bluetooth")),
        other => Err(ToolError::BadArguments(format!(
            "неизвестное устройство {other}"
        ))),
    }
}

pub struct RadioTool(pub Arc<dyn Radios>);

#[async_trait]
impl Tool for RadioTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "radio",
            "Wi-Fi и Bluetooth: узнать состояние, включить или выключить.",
            schema(
                &[
                    ("device", "string", "one of: wifi, bluetooth"),
                    ("action", "string", "one of: get, on, off"),
                ],
                &["device", "action"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let (_, name) = radio_kind(arg_str(args, "device")?)?;
        let action = arg_str(args, "action")?;
        let a = match action {
            "get" => Assessment::new(
                RiskLevel::Low,
                format!("Узнать состояние {name}"),
                name,
                "только чтение",
            ),
            "on" => Assessment::new(
                RiskLevel::Medium,
                format!("Включить {name}"),
                name,
                "меняет настройку связи, легко вернуть",
            ),
            "off" => Assessment::new(
                RiskLevel::Medium,
                format!("Выключить {name}"),
                name,
                "связь через это устройство пропадёт",
            ),
            other => {
                return Err(ToolError::BadArguments(format!(
                    "неизвестное действие {other}"
                )))
            }
        };
        Ok(a.with_scope(format!("{} {action}", name.to_lowercase())))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let (kind, name) = radio_kind(arg_str(&args, "device")?)?;
        let action = arg_str(&args, "action")?;
        let radios = self.0.clone();
        let want = match action {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        };
        // WinRT calls block; keep them off the async workers.
        let state = tokio::task::spawn_blocking(move || {
            if let Some(on) = want {
                radios.set(kind, on)?;
            }
            radios.get(kind)
        })
        .await
        .map_err(|e| failed(e.to_string()))?
        .map_err(failed)?;
        Ok(ToolOutput::new(match state {
            None => format!("На этом компьютере нет {name}."),
            Some(true) => format!("{name} включён."),
            Some(false) => format!("{name} выключен."),
        }))
    }
}

pub struct BrightnessTool(pub Arc<dyn Runner>);

const WMI_GET: &str = "(Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightness -ErrorAction Stop | Select-Object -First 1).CurrentBrightness";

fn wmi_set(level: u64) -> String {
    format!(
        "Invoke-CimMethod -InputObject (Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods -ErrorAction Stop | Select-Object -First 1) -MethodName WmiSetBrightness -Arguments @{{Timeout=1;Brightness={level}}} | Out-Null"
    )
}

impl BrightnessTool {
    async fn powershell(
        &self,
        script: &str,
        cancel: CancellationToken,
    ) -> Result<String, ToolError> {
        let args = vec![
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            script.to_string(),
        ];
        let out = self
            .0
            .run("powershell.exe", &args, Duration::from_secs(15), cancel)
            .await
            .map_err(failed)?;
        if out.code != Some(0) {
            return Err(failed(
                "яркость этим экраном не управляется: так бывает у внешних мониторов и в удалённом сеансе",
            ));
        }
        Ok(out.stdout.trim().to_string())
    }
}

#[async_trait]
impl Tool for BrightnessTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "brightness",
            "Яркость встроенного экрана: узнать или установить в процентах.",
            schema(
                &[
                    ("action", "string", "one of: get, set"),
                    ("level", "integer", "Яркость 0–100 для set"),
                ],
                &["action"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let summary = match arg_str(args, "action")? {
            "get" => "Узнать яркость".to_string(),
            "set" => {
                let level = arg_u64(args, "level")
                    .ok_or_else(|| ToolError::BadArguments("нужен level 0–100".into()))?;
                format!("Яркость {}%", level.min(100))
            }
            other => {
                return Err(ToolError::BadArguments(format!(
                    "неизвестное действие {other}"
                )))
            }
        };
        Ok(Assessment::new(
            RiskLevel::Low,
            summary,
            "яркость экрана",
            "легко вернуть как было",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        if arg_str(&args, "action")? == "set" {
            let level = arg_u64(&args, "level").unwrap_or(50).min(100);
            self.powershell(&wmi_set(level), cancel).await?;
            return Ok(ToolOutput::new(format!("Яркость {level}%.")));
        }
        let now = self.powershell(WMI_GET, cancel).await?;
        Ok(ToolOutput::new(format!("Яркость {now}%.")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::RunOutput;
    use crate::testing::{env, run};
    use serde_json::json;

    #[tokio::test]
    async fn radio_reads_and_switches() {
        let (e, f) = env();
        let tool = RadioTool(e.radios.clone());
        assert_eq!(
            run(&tool, json!({"device": "wifi", "action": "get"}))
                .await
                .unwrap()
                .text,
            "Wi-Fi включён."
        );
        assert_eq!(
            run(&tool, json!({"device": "wifi", "action": "off"}))
                .await
                .unwrap()
                .text,
            "Wi-Fi выключен."
        );
        assert_eq!(f.radios.lock().0, Some(false));
        assert_eq!(
            run(&tool, json!({"device": "bluetooth", "action": "get"}))
                .await
                .unwrap()
                .text,
            "На этом компьютере нет Bluetooth."
        );
    }

    #[test]
    fn radio_change_is_medium_risk_with_its_own_scope() {
        let (e, _) = env();
        let tool = RadioTool(e.radios.clone());
        let get = tool
            .assess(&json!({"device": "wifi", "action": "get"}))
            .unwrap();
        let off = tool
            .assess(&json!({"device": "wifi", "action": "off"}))
            .unwrap();
        assert_eq!(get.level, RiskLevel::Low);
        assert_eq!(off.level, RiskLevel::Medium);
        assert_eq!(off.scope, "wi-fi off");
        assert!(tool
            .assess(&json!({"device": "nfc", "action": "get"}))
            .is_err());
    }

    #[tokio::test]
    async fn brightness_runs_wmi_through_powershell() {
        let (e, f) = env();
        *f.run_output.lock() = RunOutput {
            code: Some(0),
            stdout: "70\r\n".into(),
            ..Default::default()
        };
        let tool = BrightnessTool(e.runner.clone());
        assert_eq!(
            run(&tool, json!({"action": "get"})).await.unwrap().text,
            "Яркость 70%."
        );
        assert_eq!(
            run(&tool, json!({"action": "set", "level": 250}))
                .await
                .unwrap()
                .text,
            "Яркость 100%."
        );
        let runs = f.runs.lock();
        assert_eq!(runs[0].0, "powershell.exe");
        assert!(runs[1].1[3].contains("Brightness=100"));
    }

    #[tokio::test]
    async fn brightness_explains_unsupported_screen() {
        let (e, f) = env();
        *f.run_output.lock() = RunOutput {
            code: Some(1),
            ..Default::default()
        };
        let err = run(&BrightnessTool(e.runner.clone()), json!({"action": "get"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("не управляется"));
    }
}
