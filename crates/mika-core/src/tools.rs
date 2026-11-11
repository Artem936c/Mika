//! Tool registry. Every tool reports its risk and runs only with an `Approval`.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::gate::Approval;
use crate::llm::ToolSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            RiskLevel::Low => "low",
            RiskLevel::Medium => "medium",
            RiskLevel::High => "high",
        }
    }
}

/// What a call will do, shown on the confirm card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assessment {
    pub level: RiskLevel,
    /// Plain words: "Открыть сайт".
    pub summary: String,
    /// Exact command, path or address.
    pub target: String,
    /// Why this risk level.
    pub reason: String,
    /// Key for permanent rules: `*`, a normalized command, an app name, a host.
    pub scope: String,
}

impl Assessment {
    pub fn new(
        level: RiskLevel,
        summary: impl Into<String>,
        target: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            level,
            summary: summary.into(),
            target: target.into(),
            reason: reason.into(),
            scope: "*".into(),
        }
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    /// Raises the level, never lowers it.
    pub fn at_least(mut self, level: RiskLevel, reason: impl Into<String>) -> Self {
        if level > self.level {
            self.level = level;
            self.reason = reason.into();
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub text: String,
}

impl ToolOutput {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolError {
    #[error("неверные аргументы: {0}")]
    BadArguments(String),
    #[error("{0}")]
    Failed(String),
    #[error("остановлено")]
    Cancelled,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    /// Risk of this exact call. Arguments are checked here first.
    fn assess(&self, args: &Value) -> Result<Assessment, ToolError>;
    async fn run(
        &self,
        args: Value,
        approval: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError>;
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.spec().name, tool);
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Helpers for reading tool arguments.
pub fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::BadArguments(format!("нужен строковый аргумент `{key}`")))
}

pub fn arg_str_opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

pub fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters,
    }
}

/// Current local date and time.
pub struct TimeTool;

#[async_trait]
impl Tool for TimeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "get_time",
            "Текущие локальные дата, время и день недели.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Узнать время",
            "часы компьютера",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _approval: Approval,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(format_time(chrono::Local::now())))
    }
}

/// `2026-11-10 23:51:46, вторник, UTC+03:00`. Russian weekday: small models
/// mistranslate English day names.
pub fn format_time<Tz: chrono::TimeZone>(t: chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    use chrono::Datelike;
    const DAYS: [&str; 7] = [
        "понедельник",
        "вторник",
        "среда",
        "четверг",
        "пятница",
        "суббота",
        "воскресенье",
    ];
    let day = DAYS[t.weekday().num_days_from_monday() as usize];
    format!(
        "{}, {day}, UTC{}",
        t.format("%Y-%m-%d %H:%M:%S"),
        t.format("%:z")
    )
}

/// Returns its argument. For checking the tool loop.
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "echo",
            "Возвращает переданный текст. Для проверки связи.",
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let text = arg_str(args, "text")?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Повторить текст",
            text,
            "ничего не меняет",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _approval: Approval,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(arg_str(&args, "text")?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(TimeTool));
        r.register(Arc::new(EchoTool));
        r
    }

    #[test]
    fn registry_lists_specs_sorted_by_name() {
        let names: Vec<_> = registry().specs().into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["echo", "get_time"]);
    }

    #[test]
    fn registry_get_unknown_is_none() {
        assert!(registry().get("nope").is_none());
    }

    #[tokio::test]
    async fn time_tool_returns_date_and_time() {
        let out = TimeTool
            .run(json!({}), Approval::for_tests(), CancellationToken::new())
            .await
            .unwrap();
        let re =
            regex::Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}, \w+, UTC[+-]\d{2}:\d{2}$")
                .unwrap();
        assert!(re.is_match(&out.text), "{}", out.text);
    }

    #[test]
    fn format_time_uses_russian_weekday() {
        use chrono::TimeZone;
        let t = chrono::FixedOffset::east_opt(3 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 11, 10, 23, 51, 46)
            .unwrap();
        assert_eq!(format_time(t), "2026-11-10 23:51:46, вторник, UTC+03:00");
    }

    #[tokio::test]
    async fn echo_tool_returns_text_argument() {
        let out = EchoTool
            .run(
                json!({ "text": "привет" }),
                Approval::for_tests(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.text, "привет");
    }

    #[test]
    fn echo_tool_rejects_missing_text() {
        assert!(matches!(
            EchoTool.assess(&json!({})),
            Err(ToolError::BadArguments(_))
        ));
    }

    #[test]
    fn risk_levels_are_ordered() {
        assert!(RiskLevel::Low < RiskLevel::Medium && RiskLevel::Medium < RiskLevel::High);
    }
}
