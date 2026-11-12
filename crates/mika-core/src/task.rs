//! Multi-step tasks: a plan the model writes, steps it marks as it goes, and the
//! board the chat shows. Both tools only change the board, so they are low risk.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::events::{Event, EventBus};
use crate::gate::Approval;
use crate::llm::ToolSpec;
use crate::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};

/// Most steps in one plan.
pub const MAX_PLAN_STEPS: usize = 20;
/// Longest step text kept.
const MAX_STEP_CHARS: usize = 100;
/// LLM calls per message while a task has a plan. Without a plan the limit is `agent::MAX_STEPS`.
pub const TASK_MAX_STEPS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Active,
    Done,
    Failed,
    Skipped,
}

impl StepStatus {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_lowercase().as_str() {
            "active" | "start" | "started" | "in_progress" => StepStatus::Active,
            "done" | "ok" | "completed" | "success" => StepStatus::Done,
            "failed" | "fail" | "error" => StepStatus::Failed,
            "skipped" | "skip" => StepStatus::Skipped,
            "pending" => StepStatus::Pending,
            _ => return None,
        })
    }

    fn open(self) -> bool {
        matches!(self, StepStatus::Pending | StepStatus::Active)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStep {
    pub text: String,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub title: String,
    pub steps: Vec<TaskStep>,
    pub finished: bool,
    /// Every step done or skipped; meaningful once `finished`.
    pub ok: bool,
}

/// The task of the message being answered. Shared by the agent and the two tools.
#[derive(Clone)]
pub struct TaskBoard {
    task: Arc<Mutex<Option<Task>>>,
    bus: EventBus,
}

impl TaskBoard {
    pub fn new(bus: EventBus) -> Self {
        Self {
            task: Arc::new(Mutex::new(None)),
            bus,
        }
    }

    pub fn get(&self) -> Option<Task> {
        self.task.lock().clone()
    }

    /// True while an unfinished task has a plan.
    pub fn active(&self) -> bool {
        self.task.lock().as_ref().is_some_and(|t| !t.finished)
    }

    /// Numbers (from 1) of steps not done, failed or skipped yet.
    pub fn open_steps(&self) -> Vec<usize> {
        self.task
            .lock()
            .as_ref()
            .filter(|t| !t.finished)
            .map(|t| {
                t.steps
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.status.open())
                    .map(|(i, _)| i + 1)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Forgets the previous task: a new message starts clean.
    pub fn clear(&self) {
        *self.task.lock() = None;
    }

    fn publish(&self, task: &Task) {
        self.bus.publish(Event::TaskUpdated { task: task.clone() });
    }

    pub fn plan(&self, title: &str, steps: Vec<String>) -> Result<Task, String> {
        let steps: Vec<String> = steps
            .into_iter()
            // The panel shows one line per step.
            .map(|s| {
                let s = s.trim();
                if s.chars().count() > MAX_STEP_CHARS {
                    let cut: String = s.chars().take(MAX_STEP_CHARS - 1).collect();
                    format!("{}…", cut.trim_end())
                } else {
                    s.to_string()
                }
            })
            .filter(|s| !s.is_empty())
            .collect();
        if steps.is_empty() {
            return Err("в плане нет шагов".into());
        }
        if steps.len() > MAX_PLAN_STEPS {
            return Err(format!(
                "слишком длинный план: не больше {MAX_PLAN_STEPS} шагов"
            ));
        }
        let task = Task {
            title: title.trim().to_string(),
            steps: steps
                .into_iter()
                .map(|text| TaskStep {
                    text,
                    status: StepStatus::Pending,
                    note: String::new(),
                })
                .collect(),
            finished: false,
            ok: false,
        };
        *self.task.lock() = Some(task.clone());
        self.publish(&task);
        Ok(task)
    }

    /// `index` counts from 1, as the model sees the plan.
    pub fn mark(&self, index: usize, status: StepStatus, note: &str) -> Result<Task, String> {
        let mut guard = self.task.lock();
        let task = guard
            .as_mut()
            .ok_or("плана нет: сначала вызови task_plan")?;
        if task.finished {
            return Err("задача уже закончена".into());
        }
        let n = task.steps.len();
        if index == 0 || index > n {
            return Err(format!("нет шага {index}: в плане шагов {n}"));
        }
        if status == StepStatus::Active {
            // One active step at a time.
            for s in task.steps.iter_mut() {
                if s.status == StepStatus::Active {
                    s.status = StepStatus::Pending;
                }
            }
        }
        let step = &mut task.steps[index - 1];
        step.status = status;
        step.note = note.trim().chars().take(200).collect();
        let task = task.clone();
        drop(guard);
        self.publish(&task);
        Ok(task)
    }

    /// Closes the task at the end of the message. A stop marks the running step failed;
    /// steps never reached become skipped.
    pub fn finish(&self, stopped: bool) {
        let mut guard = self.task.lock();
        let Some(task) = guard.as_mut() else {
            return;
        };
        if task.finished {
            return;
        }
        // A step the model never closed means the task is not done.
        let all_closed = task.steps.iter().all(|s| !s.status.open());
        for s in task.steps.iter_mut() {
            match s.status {
                StepStatus::Active => {
                    s.status = StepStatus::Failed;
                    s.note = if stopped {
                        "остановлено".into()
                    } else {
                        "не закончен".into()
                    };
                }
                StepStatus::Pending => s.status = StepStatus::Skipped,
                _ => {}
            }
        }
        task.finished = true;
        task.ok = !stopped
            && all_closed
            && task
                .steps
                .iter()
                .all(|s| matches!(s.status, StepStatus::Done | StepStatus::Skipped))
            && task.steps.iter().any(|s| s.status == StepStatus::Done);
        let task = task.clone();
        drop(guard);
        self.publish(&task);
    }
}

/// Short text of the board for the model: what is done and what is next.
pub fn render(task: &Task) -> String {
    let mut out = format!("План «{}»:", task.title);
    for (i, s) in task.steps.iter().enumerate() {
        let mark = match s.status {
            StepStatus::Pending => " ",
            StepStatus::Active => ">",
            StepStatus::Done => "x",
            StepStatus::Failed => "!",
            StepStatus::Skipped => "-",
        };
        out.push_str(&format!("\n[{mark}] {}. {}", i + 1, s.text));
        if !s.note.is_empty() {
            out.push_str(&format!(" — {}", s.note));
        }
    }
    if let Some(next) = task.steps.iter().position(|s| s.status.open()) {
        out.push_str(&format!("\nДальше шаг {}.", next + 1));
    } else {
        out.push_str("\nВсе шаги отмечены: коротко отчитайся пользователю.");
    }
    out
}

/// Text for the system prompt: when and how to plan.
pub const TASK_RULES: &str = "Задачу больше чем из трёх действий сначала разбей на шаги инструментом task_plan. \
Шаги пиши по-русски короткими фразами для человека, без путей и названий инструментов. \
Перед шагом отметь его task_step со статусом active, после — done или failed с короткой заметкой. \
После каждого действия проверь результат (ui_read, browser_read, screen_look, list_folder) и только потом отмечай done. \
Если шаг не получился два раза подряд, остановись и спроси пользователя, как быть. \
В конце коротко скажи, что сделано и что нет. \
Не обещай сделать что-то потом и не спрашивай «подождёшь?»: собираешься действовать — вызывай инструмент в этом же ответе. \
Не заканчивай ответ, пока просьба не выполнена или не стало ясно, что мешает.";

pub struct TaskPlanTool(pub TaskBoard);

#[async_trait]
impl Tool for TaskPlanTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "task_plan",
            "Составить план задачи из нескольких шагов. Пользователь видит план и ход выполнения. Шаги — короткие фразы для человека, например «Посмотреть, что в Загрузках», без путей и названий инструментов. Новый вызов заменяет план.",
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Короткое название задачи" },
                    "steps": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Шаги по порядку, каждый — одно действие с проверкой"
                    }
                },
                "required": ["title", "steps"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let title = arg_str(args, "title")?;
        steps_arg(args)?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Составить план",
            title.to_string(),
            "только план в чате",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _approval: Approval,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let title = arg_str(&args, "title")?;
        let task = self
            .0
            .plan(title, steps_arg(&args)?)
            .map_err(ToolError::Failed)?;
        Ok(ToolOutput::new(render(&task)))
    }
}

fn steps_arg(args: &Value) -> Result<Vec<String>, ToolError> {
    let steps = args
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| ToolError::BadArguments("нужен список шагов `steps`".into()))?;
    Ok(steps
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect())
}

pub struct TaskStepTool(pub TaskBoard);

#[async_trait]
impl Tool for TaskStepTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "task_step",
            "Отметить шаг плана: active — начала, done — сделано и проверено, failed — не вышло, skipped — не нужен.",
            json!({
                "type": "object",
                "properties": {
                    "step": { "type": "integer", "description": "Номер шага, с 1" },
                    "status": { "type": "string", "enum": ["active", "done", "failed", "skipped"] },
                    "note": { "type": "string", "description": "Коротко: что получилось или почему нет" }
                },
                "required": ["step", "status"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let (index, status) = step_args(args)?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Отметить шаг плана",
            format!("шаг {index}: {status:?}"),
            "только план в чате",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _approval: Approval,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let (index, status) = step_args(&args)?;
        let note = args.get("note").and_then(Value::as_str).unwrap_or("");
        let task = self
            .0
            .mark(index, status, note)
            .map_err(ToolError::Failed)?;
        Ok(ToolOutput::new(render(&task)))
    }
}

fn step_args(args: &Value) -> Result<(usize, StepStatus), ToolError> {
    let index = args
        .get("step")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .ok_or_else(|| ToolError::BadArguments("нужен номер шага `step`".into()))?;
    let status = StepStatus::parse(arg_str(args, "status")?).ok_or_else(|| {
        ToolError::BadArguments("статус: active, done, failed или skipped".into())
    })?;
    Ok((index as usize, status))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> (TaskBoard, tokio::sync::broadcast::Receiver<Event>) {
        let bus = EventBus::new(64);
        let rx = bus.subscribe();
        (TaskBoard::new(bus), rx)
    }

    #[test]
    fn plan_mark_and_finish() {
        let (b, mut rx) = board();
        assert!(!b.active());
        b.plan(
            "Разобрать загрузки",
            vec!["Список".into(), " ".into(), "Перенос".into()],
        )
        .unwrap();
        assert!(b.active());
        assert_eq!(b.get().unwrap().steps.len(), 2);
        b.mark(1, StepStatus::Active, "").unwrap();
        b.mark(1, StepStatus::Done, "12 файлов").unwrap();
        assert_eq!(b.open_steps(), vec![2]);
        b.mark(2, StepStatus::Skipped, "не нужен").unwrap();
        b.finish(false);
        let t = b.get().unwrap();
        assert!(t.finished && t.ok);
        assert!(!b.active() && b.open_steps().is_empty());
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(events.len(), 5);
        assert!(matches!(&events[4], Event::TaskUpdated { task } if task.finished));
    }

    #[test]
    fn steps_left_open_make_the_task_not_ok() {
        let (b, _rx) = board();
        b.plan("t", vec!["a".into(), "b".into()]).unwrap();
        b.mark(1, StepStatus::Done, "").unwrap();
        b.finish(false);
        let t = b.get().unwrap();
        assert!(t.finished && !t.ok);
        assert_eq!(t.steps[1].status, StepStatus::Skipped);
    }

    #[test]
    fn one_active_step_and_bounds() {
        let (b, _rx) = board();
        assert!(b.mark(1, StepStatus::Done, "").is_err());
        b.plan("t", vec!["a".into(), "b".into()]).unwrap();
        b.mark(1, StepStatus::Active, "").unwrap();
        let t = b.mark(2, StepStatus::Active, "").unwrap();
        assert_eq!(t.steps[0].status, StepStatus::Pending);
        assert_eq!(t.steps[1].status, StepStatus::Active);
        assert!(b.mark(3, StepStatus::Done, "").is_err());
        assert!(b.mark(0, StepStatus::Done, "").is_err());
        assert!(b.plan("t", vec![]).is_err());
        let long = b.plan("t", vec!["ш".repeat(300)]).unwrap();
        assert_eq!(long.steps[0].text.chars().count(), MAX_STEP_CHARS);
        assert!(long.steps[0].text.ends_with('…'));
        assert!(b.plan("t", vec!["x".into(); MAX_PLAN_STEPS + 1]).is_err());
    }

    #[test]
    fn stop_fails_the_running_step() {
        let (b, _rx) = board();
        b.plan("t", vec!["a".into(), "b".into()]).unwrap();
        b.mark(1, StepStatus::Active, "").unwrap();
        b.finish(true);
        let t = b.get().unwrap();
        assert!(!t.ok);
        assert_eq!(t.steps[0].status, StepStatus::Failed);
        assert_eq!(t.steps[0].note, "остановлено");
        assert_eq!(t.steps[1].status, StepStatus::Skipped);
    }

    #[test]
    fn render_shows_marks_and_next_step() {
        let (b, _rx) = board();
        b.plan("Т", vec!["a".into(), "b".into()]).unwrap();
        let t = b.mark(1, StepStatus::Done, "ок").unwrap();
        let text = render(&t);
        assert!(text.contains("[x] 1. a — ок"));
        assert!(text.contains("[ ] 2. b"));
        assert!(text.ends_with("Дальше шаг 2."));
    }

    #[tokio::test]
    async fn tools_are_low_risk_and_parse_loose_arguments() {
        let (b, _rx) = board();
        let plan = TaskPlanTool(b.clone());
        let a = plan
            .assess(&json!({"title": "t", "steps": ["a", "b"]}))
            .unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        plan.run(
            json!({"title": "t", "steps": ["a", "b"]}),
            Approval::for_tests(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let step = TaskStepTool(b.clone());
        let out = step
            .run(
                json!({"step": "2", "status": "ok"}),
                Approval::for_tests(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(out.text.contains("[x] 2. b"));
        assert!(step.assess(&json!({"step": 1, "status": "maybe"})).is_err());
    }
}
