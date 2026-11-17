//! Tool registry. Every tool reports its risk and runs only with an `Approval`.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::events::{Event, EventBus};
use crate::gate::Approval;
use crate::llm::ToolSpec;
use crate::policy::RuleClass;

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
    /// The action's name aloud, when it differs from `summary`: the voice
    /// asks with it, and a high-risk action is approved by saying it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spoken: Option<String>,
    /// The class of calls this one belongs to. "Always" then allows the
    /// whole class with one rule (decision 0044).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_class: Option<RuleClass>,
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
            spoken: None,
            rule_class: None,
        }
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    pub fn with_spoken(mut self, spoken: impl Into<String>) -> Self {
        self.spoken = Some(spoken.into());
        self
    }

    pub fn with_rule_class(mut self, class: RuleClass) -> Self {
        self.rule_class = Some(class);
        self
    }

    /// The action's name aloud: `spoken` or else `summary`.
    pub fn spoken_name(&self) -> &str {
        self.spoken.as_deref().unwrap_or(&self.summary)
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
    /// Structured result for a chat card. The LLM gets only `text`.
    pub data: Option<serde_json::Value>,
}

impl ToolOutput {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
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
    /// The tool cannot work yet; `data` is a chat card that helps: a button
    /// to download FFmpeg.
    #[error("{message}")]
    NotReady {
        message: String,
        data: serde_json::Value,
    },
}

/// Least time between two progress events of one call.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

struct ProgressInner {
    /// Gets every share that goes out.
    sink: Box<dyn Fn(u8) + Send + Sync>,
    last: Option<u8>,
    at: Option<Instant>,
}

/// How far the running tool call has got. Empty outside a tool call: then
/// `set` does nothing.
#[derive(Clone, Default)]
pub struct Progress {
    inner: Option<Arc<parking_lot::Mutex<ProgressInner>>>,
}

impl Progress {
    /// Progress of the tool call `id`: every share goes out as `ToolProgress`.
    pub fn new(id: String, bus: EventBus) -> Self {
        Self::to(move |share| {
            bus.publish(Event::ToolProgress {
                id: id.clone(),
                share,
            });
        })
    }

    /// Progress that goes to `sink` in place of the bus, by the same rule as
    /// `set`: a background job shows it on its own card.
    pub fn to(sink: impl Fn(u8) + Send + Sync + 'static) -> Self {
        Self {
            inner: Some(Arc::new(parking_lot::Mutex::new(ProgressInner {
                sink: Box::new(sink),
                last: None,
                at: None,
            }))),
        }
    }

    /// Reports the share when it changed, at most every 250 ms; 100 always
    /// goes out. A call's own progress is published as `ToolProgress`.
    pub fn set(&self, share: u8) {
        let Some(inner) = &self.inner else {
            return;
        };
        let share = share.min(100);
        let mut p = inner.lock();
        if p.last == Some(share) {
            return;
        }
        let due = p.at.is_none_or(|at| at.elapsed() >= PROGRESS_EVERY);
        if !due && share != 100 {
            return;
        }
        p.last = Some(share);
        p.at = Some(Instant::now());
        (p.sink)(share);
    }
}

tokio::task_local! {
    static PROGRESS: Progress;
}

/// Progress of the tool call that runs in this task. Take it at the start of
/// `run`: callbacks may run in other tasks.
pub fn progress_handle() -> Progress {
    PROGRESS.try_with(Clone::clone).unwrap_or_default()
}

/// Runs `fut` with the progress of the call `id`.
pub async fn with_progress<F: Future>(id: String, bus: EventBus, fut: F) -> F::Output {
    PROGRESS.scope(Progress::new(id, bus), fut).await
}

/// Runs `fut` with `progress` as what `progress_handle` gives inside it.
pub async fn with_progress_handle<F: Future>(progress: Progress, fut: F) -> F::Output {
    PROGRESS.scope(progress, fut).await
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

    #[tokio::test]
    async fn progress_publishes_changes_with_the_call_id() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        assert!(progress_handle().inner.is_none(), "empty outside a call");
        progress_handle().set(10);
        with_progress("c1".into(), bus.clone(), async {
            let p = progress_handle();
            p.set(5);
            p.set(5);
            p.set(7);
            p.set(200);
        })
        .await;
        let mut got = Vec::new();
        while let Ok(e) = rx.try_recv() {
            got.push(e);
        }
        assert_eq!(
            got,
            vec![
                Event::ToolProgress {
                    id: "c1".into(),
                    share: 5
                },
                Event::ToolProgress {
                    id: "c1".into(),
                    share: 100
                },
            ],
            "7 came too soon, 100 always goes out"
        );
    }

    #[tokio::test]
    async fn progress_to_a_sink_follows_the_same_throttle() {
        let got = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let seen = got.clone();
        let p = Progress::to(move |share| seen.lock().push(share));
        p.set(5);
        p.set(5);
        p.set(7);
        p.set(200);
        assert_eq!(
            *got.lock(),
            vec![5, 100],
            "7 came too soon, 100 always goes out"
        );
        tokio::time::sleep(PROGRESS_EVERY + Duration::from_millis(30)).await;
        p.set(100);
        p.set(60);
        assert_eq!(*got.lock(), vec![5, 100, 60], "the same share twice is one");

        // Inside a scope the tool gets this very progress, not the bus.
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        let inner = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let seen = inner.clone();
        with_progress_handle(Progress::to(move |s| seen.lock().push(s)), async {
            progress_handle().set(42);
        })
        .await;
        assert_eq!(*inner.lock(), vec![42]);
        assert!(rx.try_recv().is_err(), "nothing goes to the bus");
    }

    #[test]
    fn risk_levels_are_ordered() {
        assert!(RiskLevel::Low < RiskLevel::Medium && RiskLevel::Medium < RiskLevel::High);
    }
}
