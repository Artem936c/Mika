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
        Ok(
            ToolOutput::new(format!("Громкость {target}% (была {level}%)."))
                .with_data(serde_json::json!({ "kind": "volume", "level": target, "was": level })),
        )
    }
}

/// Spoken names of programs and the executables they mean.
const APP_ALIASES: &[(&str, &[&str])] = &[
    (
        "браузер",
        &["chrome", "msedge", "firefox", "browser", "opera", "brave"],
    ),
    ("хром", &["chrome"]),
    ("эдж", &["msedge"]),
    ("яндекс", &["browser", "yandex"]),
    ("файрфокс", &["firefox"]),
    ("телеграм", &["telegram"]),
    ("дискорд", &["discord"]),
    ("спотифай", &["spotify"]),
    ("зум", &["zoom"]),
    ("скайп", &["skype"]),
    ("стим", &["steam"]),
    ("вк", &["vk"]),
    (
        "музык",
        &[
            "music",
            "spotify",
            "yandexmusic",
            "aimp",
            "foobar",
            "winamp",
        ],
    ),
    ("видео", &["vlc", "mpc", "potplayer", "video"]),
];

/// Whether the mixer entry belongs to the program the user named.
fn app_matches(process: &str, query: &str) -> bool {
    let p = process.trim_end_matches(".exe");
    let q = query.trim().to_lowercase();
    let q = q.trim_end_matches(".exe");
    if q.is_empty() {
        return false;
    }
    if p.contains(q) {
        return true;
    }
    APP_ALIASES
        .iter()
        .filter(|(alias, _)| q.contains(alias))
        .any(|(_, exes)| exes.iter().any(|e| p.contains(e)))
}

pub struct AppVolumeTool(pub Arc<dyn Volume>);

#[async_trait]
impl Tool for AppVolumeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "app_volume",
            "Громкость отдельной программы в микшере Windows: какие программы звучат, сделать браузер или игру тише, громче, выключить или включить звук только у неё. Общую громкость меняет volume.",
            schema(
                &[
                    ("action", "string", "one of: list, set, up, down, mute, unmute"),
                    ("app", "string", "Программа: chrome, Telegram, браузер, игра"),
                    ("level", "integer", "Громкость программы 0–100 для set"),
                ],
                &["action"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let action = arg_str(args, "action")?;
        if action == "list" {
            return Ok(Assessment::new(
                RiskLevel::Low,
                "Посмотреть микшер громкости",
                "микшер громкости",
                "только чтение",
            )
            .with_scope("list"));
        }
        let app = arg_str(args, "app")?.trim();
        if app.is_empty() {
            return Err(ToolError::BadArguments("нужна программа app".into()));
        }
        let summary = match action {
            "set" => {
                let level = arg_u64(args, "level")
                    .ok_or_else(|| ToolError::BadArguments("нужен level 0–100".into()))?;
                format!("Громкость {app}: {}%", level.min(100))
            }
            "up" => format!("Сделать {app} громче"),
            "down" => format!("Сделать {app} тише"),
            "mute" => format!("Выключить звук {app}"),
            "unmute" => format!("Включить звук {app}"),
            other => {
                return Err(ToolError::BadArguments(format!(
                    "неизвестное действие {other}"
                )))
            }
        };
        Ok(Assessment::new(
            RiskLevel::Low,
            summary,
            "микшер громкости",
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
        let volume = self.0.clone();
        let apps = tokio::task::spawn_blocking(move || volume.apps())
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(failed)?;
        let action = arg_str(&args, "action")?;
        let line = |a: &crate::sys::AppVolume| {
            format!(
                "{} {}%{}",
                a.process,
                a.level,
                if a.muted {
                    ", звук выключен"
                } else {
                    ""
                }
            )
        };
        if action == "list" {
            if apps.is_empty() {
                return Ok(ToolOutput::new("Сейчас ни одна программа не звучит."));
            }
            let lines: Vec<String> = apps.iter().map(line).collect();
            return Ok(ToolOutput::new(format!(
                "Программы в микшере: {}.",
                lines.join("; ")
            )));
        }
        let query = arg_str(&args, "app")?;
        let hit: Vec<&crate::sys::AppVolume> = apps
            .iter()
            .filter(|a| app_matches(&a.process, query))
            .collect();
        if hit.is_empty() {
            let known: Vec<String> = apps.iter().map(|a| a.process.clone()).collect();
            return Err(failed(format!(
                "в микшере нет программы «{query}». Сейчас звучат: {}",
                if known.is_empty() {
                    "ничего".to_string()
                } else {
                    known.join(", ")
                }
            )));
        }
        let mut done = Vec::new();
        for a in hit {
            let (level, mute) = match action {
                "set" => (
                    Some(arg_u64(&args, "level").unwrap_or(a.level as u64).min(100) as u8),
                    None,
                ),
                "up" => (Some(a.level.saturating_add(20).min(100)), Some(false)),
                "down" => (Some(a.level.saturating_sub(20)), None),
                "mute" => (None, Some(true)),
                "unmute" => (None, Some(false)),
                _ => (None, None),
            };
            let (volume, pid) = (self.0.clone(), a.pid);
            tokio::task::spawn_blocking(move || volume.set_app(pid, level, mute))
                .await
                .map_err(|e| failed(e.to_string()))?
                .map_err(failed)?;
            let now = crate::sys::AppVolume {
                level: level.unwrap_or(a.level),
                muted: mute.unwrap_or(a.muted),
                ..a.clone()
            };
            done.push(format!("{} (было {}%)", line(&now), a.level));
        }
        Ok(ToolOutput::new(format!("Готово: {}.", done.join("; "))))
    }
}

/// Makes other programs quieter while she listens or speaks, and puts their
/// levels back afterwards. Programs the user changed meanwhile keep the new level.
pub struct Ducker {
    volume: Arc<dyn Volume>,
    own_pid: u32,
    /// Share of the old level that stays while ducked.
    keep: f32,
    /// (pid, level before, level set).
    saved: parking_lot::Mutex<Vec<(u32, u8, u8)>>,
}

impl Ducker {
    pub fn new(volume: Arc<dyn Volume>, own_pid: u32) -> Self {
        Self {
            volume,
            own_pid,
            keep: 0.3,
            saved: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// True while ducked, and while a duck or restore is under way.
    pub fn ducked(&self) -> bool {
        self.saved.try_lock().is_none_or(|s| !s.is_empty())
    }

    /// Lowers every other program that has sound and is not muted. Blocking.
    pub fn duck(&self) {
        // Held for the whole call: a second call waits and then sees the first.
        let mut saved = self.saved.lock();
        if !saved.is_empty() {
            return;
        }
        let Ok(apps) = self.volume.apps() else {
            return;
        };
        for a in apps {
            if a.pid == self.own_pid || a.muted || a.level == 0 {
                continue;
            }
            let low = ((a.level as f32) * self.keep).round() as u8;
            if self.volume.set_app(a.pid, Some(low), None).is_ok() {
                saved.push((a.pid, a.level, low));
            }
        }
    }

    /// Puts back the levels changed by `duck`. Blocking.
    pub fn restore(&self) {
        let mut slot = self.saved.lock();
        let saved = std::mem::take(&mut *slot);
        if saved.is_empty() {
            return;
        }
        let now = self.volume.apps().unwrap_or_default();
        for (pid, before, set) in saved {
            // Changed by the user while ducked, or gone: leave it.
            match now.iter().find(|a| a.pid == pid) {
                Some(a) if a.level == set => {
                    let _ = self.volume.set_app(pid, Some(before), None);
                }
                _ => {}
            }
        }
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

/// One program with all its processes: Chrome runs dozens of them.
#[derive(Debug, Clone, PartialEq)]
struct Program {
    name: String,
    count: usize,
    memory_mb: u64,
    cpu_percent: f32,
}

fn programs(list: &[crate::sys::ProcessInfo]) -> Vec<Program> {
    let mut out: Vec<Program> = Vec::new();
    for p in list {
        let name = p.name.to_lowercase();
        // The idle process is not a load.
        if name.is_empty() || name == "system idle process" || p.pid == 0 {
            continue;
        }
        match out.iter_mut().find(|g| g.name == name) {
            Some(g) => {
                g.count += 1;
                g.memory_mb += p.memory_mb;
                g.cpu_percent += p.cpu_percent;
            }
            None => out.push(Program {
                name,
                count: 1,
                memory_mb: p.memory_mb,
                cpu_percent: p.cpu_percent,
            }),
        }
    }
    out
}

fn program_line(p: &Program, what: &str) -> String {
    let many = if p.count > 1 {
        format!(", {} процессов", p.count)
    } else {
        String::new()
    };
    match what {
        "cpu" => format!("{} — ЦП {:.0}%{many}", p.name, p.cpu_percent),
        _ => format!("{} — {:.1} ГБ{many}", p.name, p.memory_mb as f64 / 1024.0),
    }
}

/// Plain-words answer to «почему тормозит» from the measured numbers.
pub fn diagnose(
    facts: &crate::sys::SystemFacts,
    list: &[crate::sys::ProcessInfo],
    battery: Option<(u8, bool)>,
) -> String {
    let mut by_cpu = programs(list);
    by_cpu.sort_by(|a, b| b.cpu_percent.total_cmp(&a.cpu_percent));
    let mut by_mem = by_cpu.clone();
    by_mem.sort_by_key(|p| std::cmp::Reverse(p.memory_mb));
    let mem_share = if facts.memory_total_mb > 0 {
        facts.memory_used_mb as f64 * 100.0 / facts.memory_total_mb as f64
    } else {
        0.0
    };
    let mut causes = Vec::new();
    if facts.cpu_percent >= 85.0 {
        let who = by_cpu
            .first()
            .map(|p| format!(", больше всех — {}", program_line(p, "cpu")))
            .unwrap_or_default();
        causes.push(format!(
            "процессор загружен на {:.0}%{who}",
            facts.cpu_percent
        ));
    }
    if mem_share >= 90.0 {
        let who = by_mem
            .first()
            .map(|p| format!(", больше всех — {}", program_line(p, "mem")))
            .unwrap_or_default();
        causes.push(format!(
            "память почти кончилась: занято {mem_share:.0}%, Windows пишет на диск{who}"
        ));
    }
    for (mount, free, total) in &facts.disks {
        // A small stick is often full by design: only big disks count by gigabytes.
        let low = *total > 0.0 && (free / total < 0.07 || (*free < 5.0 && *total >= 64.0));
        if low {
            causes.push(format!(
                "на диске {mount} осталось {free:.1} ГБ из {total:.0}"
            ));
        }
    }
    let days = facts.uptime_s / 86_400;
    if days >= 7 {
        causes.push(format!(
            "компьютер работает без перезагрузки {days} дн., перезагрузка часто помогает"
        ));
    }
    if let Some((charge, false)) = battery {
        if charge <= 20 {
            causes.push(format!(
                "заряд {charge}% без зарядки: Windows могла включить экономию энергии"
            ));
        }
    }
    let mut text = if causes.is_empty() {
        format!(
            "Явной причины не видно: процессор {:.0}%, память занята на {mem_share:.0}%, места на дисках хватает. \
Если тормозит одна программа или сайт, дело скорее в ней или в интернете.",
            facts.cpu_percent
        )
    } else {
        format!("Похоже, причина в этом: {}.", causes.join("; "))
    };
    let top = |list: &[Program], what: &str| {
        list.iter()
            .take(3)
            .map(|p| program_line(p, what))
            .collect::<Vec<_>>()
            .join("; ")
    };
    text.push_str(&format!(
        "\nБольше всех процессора: {}.\nБольше всех памяти: {}.",
        top(&by_cpu, "cpu"),
        top(&by_mem, "mem")
    ));
    if let Some((charge, on)) = battery {
        text.push_str(&format!(
            "\nБатарея {charge}%{}.",
            if on { ", заряжается" } else { "" }
        ));
    }
    text.push_str(
        "\nЗакрыть программу можно через kill_process, только с подтверждения пользователя.",
    );
    text
}

pub struct SystemHealthTool(pub Arc<dyn SystemProbe>);

#[async_trait]
impl Tool for SystemHealthTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "system_health",
            "Почему компьютер тормозит: замер процессора и памяти, самые тяжёлые программы, место на дисках, батарея, время без перезагрузки и выводы.",
            schema(&[], &[]),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Проверить, почему тормозит",
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
        let text = tokio::task::spawn_blocking(move || {
            let facts = probe.facts();
            let list = probe.processes();
            diagnose(&facts, &list, probe.battery())
        })
        .await
        .map_err(|e| failed(e.to_string()))?;
        Ok(ToolOutput::new(text))
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
        let out = run(&t, json!({"action": "set", "level": 30}))
            .await
            .unwrap();
        assert_eq!(f.volume.lock().0, 30);
        assert_eq!(
            out.data,
            Some(json!({"kind": "volume", "level": 30, "was": 100}))
        );
        let read = run(&t, json!({"action": "get"})).await.unwrap();
        assert_eq!(read.data, None, "reading the level draws no card");
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

    fn app(pid: u32, process: &str, level: u8) -> crate::sys::AppVolume {
        crate::sys::AppVolume {
            pid,
            process: process.into(),
            level,
            muted: false,
        }
    }

    #[tokio::test]
    async fn app_volume_finds_the_program_by_name_or_alias() {
        let (e, f) = env();
        *f.app_volumes.lock() = vec![app(1, "chrome.exe", 80), app(2, "telegram.exe", 50)];
        let t = AppVolumeTool(e.volume.clone());
        let out = run(&t, json!({"action": "down", "app": "браузер"}))
            .await
            .unwrap()
            .text;
        assert!(out.contains("chrome.exe 60%"), "{out}");
        run(&t, json!({"action": "mute", "app": "Telegram"}))
            .await
            .unwrap();
        assert!(f.app_volumes.lock()[1].muted);
        let list = run(&t, json!({"action": "list"})).await.unwrap().text;
        assert!(list.contains("telegram.exe 50%, звук выключен"), "{list}");
        let err = run(&t, json!({"action": "mute", "app": "steam"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("chrome.exe, telegram.exe"), "{err}");
        assert!(t.assess(&json!({"action": "set", "app": "x"})).is_err());
        assert_eq!(
            t.assess(&json!({"action": "list"})).unwrap().level,
            RiskLevel::Low
        );
    }

    #[test]
    fn ducker_lowers_others_and_puts_back_what_the_user_left() {
        let (e, f) = env();
        *f.app_volumes.lock() = vec![
            app(1, "chrome.exe", 80),
            app(2, "mika.exe", 100),
            app(3, "spotify.exe", 50),
        ];
        let d = Ducker::new(e.volume.clone(), 2);
        d.duck();
        let levels: Vec<u8> = f.app_volumes.lock().iter().map(|a| a.level).collect();
        assert_eq!(levels, vec![24, 100, 15]);
        // The user turned Spotify up while ducked: it stays.
        f.app_volumes.lock()[2].level = 70;
        d.restore();
        let levels: Vec<u8> = f.app_volumes.lock().iter().map(|a| a.level).collect();
        assert_eq!(levels, vec![80, 100, 70]);
        assert!(!d.ducked());
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

    fn proc(pid: u32, name: &str, memory_mb: u64, cpu: f32) -> crate::sys::ProcessInfo {
        crate::sys::ProcessInfo {
            pid,
            name: name.into(),
            memory_mb,
            cpu_percent: cpu,
        }
    }

    #[test]
    fn diagnosis_names_the_cause_and_groups_processes() {
        let facts = crate::sys::SystemFacts {
            cpu_percent: 93.0,
            memory_used_mb: 15_500,
            memory_total_mb: 16_384,
            uptime_s: 10 * 86_400,
            disks: vec![("C:\\".into(), 3.2, 476.0), ("D:\\".into(), 300.0, 931.0)],
            os: String::new(),
            host: String::new(),
        };
        let list = vec![
            proc(1, "chrome.exe", 2048, 20.0),
            proc(2, "chrome.exe", 2048, 30.0),
            proc(3, "game.exe", 1024, 40.0),
            proc(0, "System Idle Process", 0, 90.0),
        ];
        let text = diagnose(&facts, &list, Some((15, false)));
        assert!(
            text.contains(
                "процессор загружен на 93%, больше всех — chrome.exe — ЦП 50%, 2 процессов"
            ),
            "{text}"
        );
        assert!(text.contains("память почти кончилась"), "{text}");
        assert!(text.contains("на диске C:\\ осталось 3.2 ГБ"), "{text}");
        assert!(!text.contains("D:\\"), "{text}");
        assert!(text.contains("без перезагрузки 10 дн."), "{text}");
        assert!(text.contains("заряд 15%"), "{text}");
        assert!(!text.contains("idle"), "{text}");

        let calm = crate::sys::SystemFacts {
            cpu_percent: 10.0,
            memory_used_mb: 4000,
            memory_total_mb: 16_384,
            uptime_s: 3600,
            disks: vec![("C:\\".into(), 200.0, 476.0)],
            ..facts
        };
        let text = diagnose(&calm, &list, None);
        assert!(text.starts_with("Явной причины не видно"), "{text}");
        assert!(
            text.contains("Больше всех памяти: chrome.exe — 4.0 ГБ"),
            "{text}"
        );
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
