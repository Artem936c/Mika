//! Tools for scenarios by event: create, list, delete.
//!
//! A scenario runs by itself later, so creating one always shows the whole of
//! it on the confirm card, and the scope of an "always" rule is the scenario
//! itself with its name: any other one asks again.

use async_trait::async_trait;
use mika_core::commands::CommandStore;
use mika_core::confirm::clean;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use mika_core::triggers::{
    one_line, text_mark, NewTrigger, Trigger, TriggerAction, TriggerError, TriggerEvent,
    TriggerStore, MAX_TRIGGERS, MAX_TRIGGER_INSTRUCTION,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::util::{arg_u64, failed, schema, truncate};

/// Characters of an instruction that are written into the rule scope.
const SCOPE_INSTRUCTION: usize = 80;
/// Characters of a saved command's text that the confirm card shows.
const CARD_COMMAND: usize = 300;

fn bad(message: impl Into<String>) -> ToolError {
    ToolError::BadArguments(message.into())
}

/// A pause in words, to the second: the card states the limit as it is.
fn pause_words(seconds: u32) -> String {
    let (d, h, m, s) = (
        seconds / 86_400,
        seconds % 86_400 / 3600,
        seconds % 3600 / 60,
        seconds % 60,
    );
    let parts: Vec<String> = [(d, "д"), (h, "ч"), (m, "мин"), (s, "с")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n} {unit}"))
        .collect();
    if parts.is_empty() {
        "0 с".into()
    } else {
        parts.join(" ")
    }
}

/// What makes a scenario this scenario, apart from its name: the event, its
/// filter, the action with a mark of its whole text, and the limits. `|` in
/// a device name would read as a separator, so it is taken out. The text of
/// an instruction or a command is not written out whole: the scope is shown
/// on the permissions page.
fn definition(
    event: TriggerEvent,
    filter: &str,
    idle_min: u32,
    action: &TriggerAction,
    pin: &str,
    cooldown_s: u32,
    max_per_day: u32,
) -> String {
    let act = match action {
        TriggerAction::Command(name) => format!("command:{}#{pin}", clean(name)),
        TriggerAction::Instruction(text) => {
            let lower = text.to_lowercase();
            let head: String = lower.chars().take(SCOPE_INSTRUCTION).collect();
            format!("instruction:{head}#{}", text_mark(&lower))
        }
    };
    format!(
        "{}|{}|{idle_min}|{act}|{cooldown_s}|{max_per_day}",
        event.as_str(),
        filter.to_lowercase().replace('|', " "),
    )
}

/// The scenario as a rule scope. Everything the user confirmed is in it: the
/// name, the event, its filter, the action with its text, the limits and,
/// when another scenario of that name is replaced, which one. So "always"
/// allows only the very same step again.
fn scope(new: &NewTrigger, replaced: Option<&Trigger>) -> String {
    let mut scope = format!(
        "{}|{}",
        clean(&new.name),
        definition(
            new.event,
            &new.filter,
            new.idle_min,
            &new.action,
            &new.pin,
            new.cooldown_s,
            new.max_per_day
        )
    );
    if let Some(old) = replaced {
        let was = definition(
            old.event,
            &old.filter,
            old.idle_min,
            &old.action,
            &old.pin,
            old.cooldown_s,
            old.max_per_day,
        );
        scope.push_str(&format!("|replaces:{}", text_mark(&was)));
    }
    scope
}

/// A whole number argument; absent means 0, which stands for the default.
fn number(args: &Value, key: &str) -> u32 {
    arg_u64(args, key).map_or(0, |n| n.min(u64::from(u32::MAX)) as u32)
}

/// What a scenario does, in a few words.
fn action_short(action: &TriggerAction, max: usize) -> String {
    match action {
        TriggerAction::Command(name) => format!("команда «{name}»"),
        TriggerAction::Instruction(text) => format!("поручение: {}", truncate(text, max)),
    }
}

/// "Already there", for a scenario that is created again unchanged.
fn already(t: &Trigger) -> String {
    let off = if t.enabled {
        ""
    } else {
        " Он выключен: включить его можно на странице настроек «Расширения»."
    };
    format!(
        "Такой сценарий уже есть: «{}» (№{}).{off} Ничего не изменилось.",
        t.name, t.id
    )
}

pub struct TriggerCreateTool {
    pub store: TriggerStore,
    pub commands: CommandStore,
}

/// A scenario read from the arguments.
struct Parsed {
    new: NewTrigger,
    /// The text of the saved command the scenario runs, as it is now.
    command_text: Option<String>,
    /// The scenario of the same name there is now, if any.
    old: Option<Trigger>,
}

impl TriggerCreateTool {
    /// The scenario the arguments describe, checked and with defaults.
    fn parse(&self, args: &Value) -> Result<Parsed, ToolError> {
        let name = arg_str(args, "name")?;
        let raw = arg_str(args, "event")?;
        let event = TriggerEvent::parse(raw)
            .ok_or_else(|| bad(format!("неизвестное событие {}", truncate(raw.trim(), 40))))?;
        let text = |key| {
            arg_str_opt(args, key)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        let action = match (text("command"), text("instruction")) {
            (Some(command), None) => TriggerAction::Command(command.into()),
            (None, Some(instruction)) => TriggerAction::Instruction(instruction.into()),
            _ => return Err(bad("укажи что-то одно: command или instruction")),
        };
        // The command as it is now: its text goes on the card and is pinned,
        // so the scenario never runs a text the user did not confirm.
        let command_text = match &action {
            TriggerAction::Command(command) => Some(
                self.commands
                    .find(command)
                    .ok_or_else(|| {
                        bad(format!(
                            "нет команды «{}»: сначала сохрани её через command_save или дай поручение в instruction",
                            truncate(command, 60)
                        ))
                    })?
                    .prompt,
            ),
            TriggerAction::Instruction(_) => None,
        };
        let new = NewTrigger {
            name: name.into(),
            event,
            filter: arg_str_opt(args, "filter").unwrap_or("").into(),
            idle_min: number(args, "idle_min"),
            action,
            cooldown_s: number(args, "cooldown_s"),
            max_per_day: number(args, "max_per_day"),
            pin: command_text.as_deref().map(text_mark).unwrap_or_default(),
        }
        .checked()
        .map_err(|e| bad(e.to_string()))?;
        let old = self
            .store
            .find(&new.name)
            .map_err(|e| failed(e.to_string()))?;
        Ok(Parsed {
            new,
            command_text,
            old,
        })
    }
}

/// What the scenario will do, whole: the instruction, or the saved command
/// with its text. A command longer than the card shows is marked as cut,
/// with its length; its text is pinned whole.
fn action_words(action: &TriggerAction, command_text: Option<&str>) -> String {
    match action {
        TriggerAction::Instruction(text) => truncate(text, MAX_TRIGGER_INSTRUCTION),
        TriggerAction::Command(name) => {
            let text = one_line(command_text.unwrap_or(""));
            let length = text.chars().count();
            if length > CARD_COMMAND {
                format!(
                    "команда «{name}»: {} (показано {CARD_COMMAND} знаков из {length}; весь текст — на странице настроек «Расширения»)",
                    truncate(&text, CARD_COMMAND)
                )
            } else {
                format!("команда «{name}»: {text}")
            }
        }
    }
}

#[async_trait]
impl Tool for TriggerCreateTool {
    fn spec(&self) -> ToolSpec {
        let events: Vec<&str> = TriggerEvent::ALL.iter().map(|e| e.as_str()).collect();
        spec(
            "trigger_create",
            "Сценарий по событию: когда на компьютере что-то происходит, выполнить сохранённую команду пользователя или поручение. События: audio_connected и audio_disconnected — подключили или отключили звуковое устройство (наушники), filter — часть его названия; app_started и app_closed — запустили или закрыли программу, filter — имя процесса (chrome.exe; исходящее подключение к удалённому рабочему столу — mstsc.exe); rdp_connected и rdp_disconnected — к этому компьютеру подключились по RDP или отключились; session_locked и session_unlocked — компьютер заблокировали или разблокировали; user_away и user_back — пользователя нет idle_min минут или он вернулся. Что делать: command — имя сохранённой команды, или instruction — поручение словами. Действия сценария спрашивают разрешение, как обычно.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Короткое имя сценария, например «Наушники»"
                    },
                    "event": { "type": "string", "enum": events },
                    "filter": {
                        "type": "string",
                        "description": "Для audio_*: часть названия устройства, можно не указывать. Для app_*: имя процесса, например chrome.exe"
                    },
                    "idle_min": {
                        "type": "integer",
                        "description": "Для user_away и user_back: сколько минут без ввода, по умолчанию 10"
                    },
                    "command": {
                        "type": "string",
                        "description": "Имя сохранённой команды пользователя"
                    },
                    "instruction": {
                        "type": "string",
                        "description": "Или поручение словами: что сделать, до 300 символов"
                    },
                    "cooldown_s": {
                        "type": "integer",
                        "description": "Пауза между запусками в секундах, по умолчанию 60"
                    },
                    "max_per_day": {
                        "type": "integer",
                        "description": "Не больше стольких запусков в день, по умолчанию 20"
                    }
                },
                "required": ["name", "event"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let Parsed {
            new,
            command_text,
            old,
        } = self.parse(args)?;
        // The very same scenario again changes nothing: no question for it.
        if let Some(same) = old.as_ref().filter(|old| old.same_as(&new)) {
            return Err(failed(already(same)));
        }
        let count = self.store.list().map_err(|e| failed(e.to_string()))?.len();
        if old.is_none() && count >= MAX_TRIGGERS {
            return Err(failed(TriggerError::TooMany.to_string()));
        }
        // The limits stand first: they stay in sight however long the rest is.
        let mut target = format!(
            "Пауза {}, не больше {} раз в день. Когда: {}. Что делать: {}.",
            pause_words(new.cooldown_s),
            new.max_per_day,
            new.event.describe(&new.filter, new.idle_min),
            action_words(&new.action, command_text.as_deref()),
        );
        let summary = match &old {
            Some(old) => {
                target.push_str(&format!(
                    " Заменит сценарий «{}»: когда {} → {}.",
                    old.name,
                    old.event.describe(&old.filter, old.idle_min),
                    action_short(&old.action, 80)
                ));
                format!("Заменить сценарий «{}»", old.name)
            }
            None => format!("Создать сценарий «{}»", new.name),
        };
        Ok(Assessment::new(
            RiskLevel::Medium,
            summary,
            target,
            "сценарий запускается сам, когда тебя может не быть рядом; его действия спросят разрешение, как обычно",
        )
        .with_scope(scope(&new, old.as_ref())))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let Parsed { new, old, .. } = self.parse(&args)?;
        if let Some(same) = old.as_ref().filter(|old| old.same_as(&new)) {
            return Ok(ToolOutput::new(already(same)));
        }
        let t = self.store.save(new).map_err(|e| failed(e.to_string()))?;
        let does = match &t.action {
            TriggerAction::Command(name) => format!("команда «{name}»"),
            TriggerAction::Instruction(_) => "поручение".to_string(),
        };
        let done = if old.is_some() {
            "заменён"
        } else {
            "создан"
        };
        Ok(ToolOutput::new(format!(
            "Сценарий «{}» {done} (№{}): когда {} — {does}. Он работает, пока включён на странице настроек «Расширения».",
            t.name,
            t.id,
            t.event.describe(&t.filter, t.idle_min)
        )))
    }
}

pub struct TriggerListTool(pub TriggerStore);

#[async_trait]
impl Tool for TriggerListTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "trigger_list",
            "Показать сценарии по событию: что и когда запускается само.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть сценарии по событию",
            "сценарии",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let list = self.0.list().map_err(|e| failed(e.to_string()))?;
        if list.is_empty() {
            return Ok(ToolOutput::new("Сценариев по событию пока нет."));
        }
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let lines: Vec<String> = list.iter().map(|t| line(t, &today)).collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

/// One scenario for `trigger_list`.
fn line(t: &Trigger, today: &str) -> String {
    format!(
        "№{} «{}» [{}]: когда {} → {}; пауза {}, до {} в день, сегодня {}",
        t.id,
        t.name,
        if t.enabled { "вкл" } else { "выкл" },
        t.event.describe(&t.filter, t.idle_min),
        action_short(&t.action, 120),
        pause_words(t.cooldown_s),
        t.max_per_day,
        t.fired_today(today)
    )
}

pub struct TriggerDeleteTool(pub TriggerStore);

impl TriggerDeleteTool {
    /// The scenario named by `id` or, without it, by `name`.
    fn target(&self, args: &Value) -> Result<Trigger, ToolError> {
        let name = arg_str_opt(args, "name")
            .map(str::trim)
            .filter(|n| !n.is_empty());
        let found = match (arg_u64(args, "id"), name) {
            (Some(id), _) => self.0.get(id.min(i64::MAX as u64) as i64),
            (None, Some(name)) => self.0.find(name),
            (None, None) => return Err(bad("укажи name или id")),
        };
        found
            .map_err(|e| failed(e.to_string()))?
            .ok_or_else(|| failed("нет такого сценария"))
    }
}

#[async_trait]
impl Tool for TriggerDeleteTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "trigger_delete",
            "Удалить сценарий по событию: по имени или по номеру из trigger_list.",
            schema(
                &[
                    ("name", "string", "Имя сценария"),
                    ("id", "integer", "Или его номер из trigger_list"),
                ],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let t = self.target(args)?;
        let summary = if arg_u64(args, "id").is_some() {
            format!("Удалить сценарий №{}", t.id)
        } else {
            format!("Удалить сценарий «{}»", t.name)
        };
        Ok(Assessment::new(
            RiskLevel::Low,
            summary,
            format!(
                "«{}»: когда {}",
                t.name,
                t.event.describe(&t.filter, t.idle_min)
            ),
            "сценарий можно создать снова",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let t = self.target(&args)?;
        if self.0.delete(t.id).map_err(|e| failed(e.to_string()))? {
            Ok(ToolOutput::new(format!("Сценарий «{}» удалён.", t.name)))
        } else {
            Err(failed("нет такого сценария"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use mika_core::storage::Db;
    use std::sync::Arc;

    fn tools() -> (TriggerCreateTool, TriggerListTool, TriggerDeleteTool) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let store = TriggerStore::new(db.clone());
        let commands = CommandStore::new(db);
        commands.save("Музыка", "Включи плеер").unwrap();
        (
            TriggerCreateTool {
                store: store.clone(),
                commands,
            },
            TriggerListTool(store.clone()),
            TriggerDeleteTool(store),
        )
    }

    fn headphones() -> Value {
        json!({"name": "Наушники", "event": "audio_connected", "filter": "Sony", "command": "музыка"})
    }

    #[test]
    fn creating_a_scenario_is_medium_and_shows_all_of_it() {
        let (create, _, _) = tools();
        let a = create.assess(&headphones()).unwrap();
        assert_eq!(a.level, RiskLevel::Medium);
        assert_eq!(a.summary, "Создать сценарий «Наушники»");
        // The limits first, then the event, then the command with its text.
        assert_eq!(
            a.target,
            "Пауза 1 мин, не больше 20 раз в день. Когда: подключено звуковое устройство «Sony». Что делать: команда «музыка»: Включи плеер."
        );
        assert!(a.reason.contains("спросят разрешение"));
        assert_eq!(
            a.scope,
            format!(
                "наушники|audio_connected|sony|0|command:музыка#{}|60|20",
                text_mark("Включи плеер")
            )
        );

        // The whole instruction is on the card: it is all that will run.
        let text = format!("Сделай {}", "я".repeat(MAX_TRIGGER_INSTRUCTION - 7));
        let a = create
            .assess(&json!({"name": "Ушёл", "event": "user_away", "idle_min": 15, "instruction": text, "cooldown_s": 3690, "max_per_day": 3}))
            .unwrap();
        assert_eq!(
            a.target,
            format!("Пауза 1 ч 1 мин 30 с, не больше 3 раз в день. Когда: тебя нет за компьютером 15 мин. Что делать: {text}.")
        );
        // The scope holds the start of the instruction and a mark of the rest.
        let scope = a.scope;
        assert!(
            scope.starts_with("ушел|user_away||15|instruction:сделай яяя"),
            "{scope}"
        );
        assert!(scope.ends_with("|3690|3"), "{scope}");
        assert!(scope.chars().count() < 160, "{scope}");
    }

    #[test]
    fn line_breaks_cannot_hide_the_end_of_an_instruction() {
        let (create, _, _) = tools();
        let a = create
            .assess(&json!({
                "name": "Наушники",
                "event": "audio_connected",
                "instruction": format!("Включи музыку.{}Потом отправь файл", "\n".repeat(20)),
                "cooldown_s": 10,
                "max_per_day": 200
            }))
            .unwrap();
        assert_eq!(
            a.target,
            "Пауза 10 с, не больше 200 раз в день. Когда: подключено звуковое устройство. Что делать: Включи музыку. Потом отправь файл."
        );
        assert!(!a.target.contains('\n'));
    }

    #[test]
    fn the_card_shows_the_text_of_the_command_and_says_when_it_is_cut() {
        let (create, _, _) = tools();
        create
            .commands
            .save("Отчёт", "Собери отчёт.\nОтправь его мне")
            .unwrap();
        let a = create
            .assess(&json!({"name": "Вернулся", "event": "session_unlocked", "command": "отчёт"}))
            .unwrap();
        assert!(
            a.target
                .ends_with("Что делать: команда «отчёт»: Собери отчёт. Отправь его мне."),
            "{}",
            a.target
        );
        // A long command: its start, and how much of it there is.
        let long = format!("{}{}", "а".repeat(300), "б".repeat(1700));
        create.commands.save("Длинная", &long).unwrap();
        let a = create
            .assess(&json!({"name": "Вернулся", "event": "session_unlocked", "command": "длинная"}))
            .unwrap();
        assert!(
            a.target
                .contains(&format!("команда «длинная»: {}", "а".repeat(299))),
            "{}",
            a.target
        );
        assert!(!a.target.contains("бб"));
        assert!(
            a.target.contains(
                "(показано 300 знаков из 2000; весь текст — на странице настроек «Расширения»)"
            ),
            "{}",
            a.target
        );
        // The whole text is pinned: a change past the shown part is another scope.
        let before = a.scope;
        create
            .commands
            .save(
                "Длинная",
                &format!("{}{}", "а".repeat(300), "в".repeat(1700)),
            )
            .unwrap();
        let after = create
            .assess(&json!({"name": "Вернулся", "event": "session_unlocked", "command": "длинная"}))
            .unwrap()
            .scope;
        assert_ne!(before, after);
    }

    #[test]
    fn always_covers_only_the_very_same_scenario() {
        let (create, _, _) = tools();
        let scope = |args: Value| create.assess(&args).unwrap().scope;
        let same = scope(headphones());
        // The case of the words does not matter.
        assert_eq!(
            scope(
                json!({"name": " наушники! ", "event": "audio_connected", "filter": "SONY", "command": " Музыка! "})
            ),
            same
        );
        let others = [
            // Another name is another scenario: one "always" does not allow thirty copies.
            json!({"name": "Уши", "event": "audio_connected", "filter": "Sony", "command": "музыка"}),
            json!({"name": "Наушники", "event": "audio_disconnected", "filter": "Sony", "command": "музыка"}),
            json!({"name": "Наушники", "event": "audio_connected", "filter": "JBL", "command": "музыка"}),
            json!({"name": "Наушники", "event": "audio_connected", "filter": "Sony", "instruction": "музыка"}),
            json!({"name": "Наушники", "event": "audio_connected", "filter": "Sony", "command": "музыка", "cooldown_s": 10}),
            json!({"name": "Наушники", "event": "audio_connected", "filter": "Sony", "command": "музыка", "max_per_day": 200}),
        ];
        for other in others {
            assert_ne!(scope(other.clone()), same, "{other}");
        }
        // The same command name with another text is another scenario.
        create.commands.save("Музыка", "Удали загрузки").unwrap();
        assert_ne!(scope(headphones()), same);
        create.commands.save("Музыка", "Включи плеер").unwrap();
        assert_eq!(scope(headphones()), same);
        // Two long instructions that start alike differ by their mark.
        let start = "я".repeat(SCOPE_INSTRUCTION);
        let long = |end: &str| {
            scope(
                json!({"name": "а", "event": "session_locked", "instruction": format!("{start} {end}")}),
            )
        };
        assert_ne!(long("выключи звук"), long("удали загрузки"));
        assert_eq!(long("выключи звук"), long("Выключи ЗВУК"));
        // The mark is a part of a SHA-256, not a short checksum.
        let mark = long("выключи звук");
        let mark = mark.split('#').nth(1).unwrap().split('|').next().unwrap();
        assert_eq!(mark.len(), 16);
        // Idle minutes are a part of the scenario.
        let away = |min: u32| {
            scope(json!({"name": "а", "event": "user_away", "idle_min": min, "command": "музыка"}))
        };
        assert_ne!(away(5), away(30));
        // A separator in a device name stays inside its own field.
        assert_eq!(
            scope(
                json!({"name": "а", "event": "audio_connected", "filter": "a|0|command:x", "command": "музыка"})
            ),
            format!(
                "а|audio_connected|a 0 command:x|0|command:музыка#{}|60|20",
                text_mark("Включи плеер")
            )
        );
    }

    #[test]
    fn bad_scenarios_are_refused_before_the_question() {
        let (create, _, _) = tools();
        let refused = |args: Value| match create.assess(&args) {
            Err(ToolError::BadArguments(why)) => why,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            refused(json!({"name": "а", "event": "usb_plugged", "command": "музыка"})),
            "неизвестное событие usb_plugged"
        );
        let one = "укажи что-то одно: command или instruction";
        assert_eq!(
            refused(json!({"name": "а", "event": "session_locked"})),
            one
        );
        assert_eq!(
            refused(
                json!({"name": "а", "event": "session_locked", "command": "музыка", "instruction": "Включи"})
            ),
            one
        );
        assert_eq!(
            refused(
                json!({"name": "а", "event": "session_locked", "command": " ", "instruction": ""})
            ),
            one
        );
        assert!(
            refused(json!({"name": "а", "event": "session_locked", "command": "тишина"}))
                .starts_with("нет команды «тишина»: сначала сохрани её через command_save")
        );
        assert!(
            refused(json!({"name": "а", "event": "app_started", "command": "музыка"}))
                .contains("имя процесса")
        );
        assert!(
            refused(json!({"name": "а", "event": "app_closed", "filter": "C:\\Games\\game.exe", "command": "музыка"}))
                .contains("имя процесса")
        );
        assert!(
            refused(json!({"name": "!", "event": "session_locked", "command": "музыка"}))
                .contains("имя")
        );
        assert_eq!(
            refused(
                json!({"name": "а", "event": "user_back", "idle_min": 500, "command": "музыка"})
            ),
            "idle_min должно быть от 1 до 240"
        );
        // A long instruction is not cut: it is refused, with a way out.
        let long = "я".repeat(MAX_TRIGGER_INSTRUCTION + 1);
        let why = refused(json!({"name": "а", "event": "session_locked", "instruction": long}));
        assert!(
            why.contains("300") && why.contains("своей командой"),
            "{why}"
        );
        assert!(matches!(
            create.assess(&json!({"event": "session_locked", "command": "музыка"})),
            Err(ToolError::BadArguments(_))
        ));
    }

    #[tokio::test]
    async fn create_list_and_delete_by_name_and_by_id() {
        let (create, list, delete) = tools();
        assert_eq!(
            run(&list, json!({})).await.unwrap().text,
            "Сценариев по событию пока нет."
        );
        let made = run(&create, headphones()).await.unwrap();
        assert_eq!(
            made.text,
            "Сценарий «Наушники» создан (№1): когда подключено звуковое устройство «Sony» — команда «музыка». Он работает, пока включён на странице настроек «Расширения»."
        );
        assert!(made.data.is_none());
        let made = run(
            &create,
            json!({"name": "После игры", "event": "app_closed", "filter": "Game", "instruction": "Верни громкость на 40", "cooldown_s": 300}),
        )
        .await
        .unwrap();
        assert!(made.text.starts_with(
            "Сценарий «После игры» создан (№2): когда закрыта программа game.exe — поручение."
        ));
        create.store.set_enabled(2, false).unwrap();
        create
            .store
            .mark_fired(1, 100, &chrono::Local::now().format("%Y-%m-%d").to_string())
            .unwrap();
        assert_eq!(
            run(&list, json!({})).await.unwrap().text,
            "№1 «Наушники» [вкл]: когда подключено звуковое устройство «Sony» → команда «музыка»; пауза 1 мин, до 20 в день, сегодня 1\n\
             №2 «После игры» [выкл]: когда закрыта программа game.exe → поручение: Верни громкость на 40; пауза 5 мин, до 20 в день, сегодня 0"
        );

        let a = delete.assess(&json!({"name": "наушники"})).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert_eq!(a.summary, "Удалить сценарий «Наушники»");
        assert_eq!(a.scope, "*");
        assert_eq!(
            run(&delete, json!({"name": " наушники "}))
                .await
                .unwrap()
                .text,
            "Сценарий «Наушники» удалён."
        );
        let a = delete.assess(&json!({"id": 2})).unwrap();
        assert_eq!(a.summary, "Удалить сценарий №2");
        assert_eq!(a.target, "«После игры»: когда закрыта программа game.exe");
        assert_eq!(
            run(&delete, json!({"id": 2})).await.unwrap().text,
            "Сценарий «После игры» удалён."
        );
        assert_eq!(
            run(&delete, json!({"id": 2})).await,
            Err(ToolError::Failed("нет такого сценария".into()))
        );
        assert_eq!(
            run(&delete, json!({"name": "наушники"})).await,
            Err(ToolError::Failed("нет такого сценария".into()))
        );
        assert_eq!(
            run(&delete, json!({})).await,
            Err(ToolError::BadArguments("укажи name или id".into()))
        );
        assert_eq!(
            run(&list, json!({})).await.unwrap().text,
            "Сценариев по событию пока нет."
        );
    }

    #[tokio::test]
    async fn the_same_name_replaces_and_the_thirty_first_is_refused() {
        let (create, _, _) = tools();
        run(&create, headphones()).await.unwrap();
        let other = json!({"name": "наушники!", "event": "audio_disconnected", "instruction": "Поставь на паузу"});
        // The card says that a scenario is replaced, and which one.
        let a = create.assess(&other).unwrap();
        assert_eq!(a.summary, "Заменить сценарий «Наушники»");
        assert!(
            a.target.ends_with(
                "Что делать: Поставь на паузу. Заменит сценарий «Наушники»: когда подключено звуковое устройство «Sony» → команда «музыка»."
            ),
            "{}",
            a.target
        );
        let replacing = a.scope;
        let again = run(&create, other.clone()).await.unwrap();
        assert!(
            again.text.starts_with("Сценарий «наушники!» заменён (№2)"),
            "{}",
            again.text
        );
        assert_eq!(create.store.list().unwrap().len(), 1);
        // An "always" for that replacement does not cover replacing something
        // else under the same name, nor a plain creation.
        create.store.delete(2).unwrap();
        let plain = create.assess(&other).unwrap();
        assert_eq!(plain.summary, "Создать сценарий «наушники!»");
        assert_ne!(plain.scope, replacing);
        assert!(replacing.starts_with(&format!("{}|replaces:", plain.scope)));
        run(
            &create,
            json!({"name": "Наушники", "event": "session_locked", "instruction": "Выключи звук"}),
        )
        .await
        .unwrap();
        assert_ne!(create.assess(&other).unwrap().scope, replacing);

        for i in 1..MAX_TRIGGERS {
            run(
                &create,
                json!({"name": format!("Сценарий {i}"), "event": "session_locked", "command": "музыка"}),
            )
            .await
            .unwrap();
        }
        let extra = json!({"name": "Лишний", "event": "session_locked", "command": "музыка"});
        // Refused before the question, so no card is shown for it.
        assert_eq!(
            create.assess(&extra),
            Err(ToolError::Failed("сценариев уже 30, удали лишний".into()))
        );
        // A scenario of a name that is taken still replaces the old one.
        assert!(create.assess(&headphones()).is_ok());
    }

    #[tokio::test]
    async fn the_very_same_scenario_again_asks_nothing_and_changes_nothing() {
        let (create, _, _) = tools();
        let made = run(&create, headphones()).await.unwrap();
        assert!(made.text.contains("создан (№1)"), "{}", made.text);
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        create.store.mark_fired(1, 100, &day).unwrap();
        // Refused before the question: an "always" rule cannot be used to
        // clear the counters or to switch the scenario on again.
        assert_eq!(
            create.assess(&headphones()),
            Err(ToolError::Failed(
                "Такой сценарий уже есть: «Наушники» (№1). Ничего не изменилось.".into()
            ))
        );
        create.store.set_enabled(1, false).unwrap();
        assert_eq!(
            run(&create, headphones()).await,
            Err(ToolError::Failed(
                "Такой сценарий уже есть: «Наушники» (№1). Он выключен: включить его можно на странице настроек «Расширения». Ничего не изменилось.".into()
            ))
        );
        let t = create.store.get(1).unwrap().unwrap();
        assert!(!t.enabled);
        assert_eq!((t.fired_at, t.fired_today(&day)), (100, 1));
        // The command got another text: now it is another scenario, with a card.
        create.commands.save("Музыка", "Включи радио").unwrap();
        let a = create.assess(&headphones()).unwrap();
        assert_eq!(a.summary, "Заменить сценарий «Наушники»");
        assert!(a.target.contains("команда «музыка»: Включи радио."));
        let changed = run(&create, headphones()).await.unwrap();
        assert!(changed.text.contains("заменён (№2)"), "{}", changed.text);
        let t = create.store.get(2).unwrap().unwrap();
        assert_eq!(t.pin, text_mark("Включи радио"));
        // Replaced, it goes on counting from the runs already made.
        assert_eq!((t.fired_at, t.fired_today(&day)), (100, 1));
    }

    #[test]
    fn a_pause_is_put_in_words_to_the_second() {
        assert_eq!(pause_words(10), "10 с");
        assert_eq!(pause_words(60), "1 мин");
        assert_eq!(pause_words(90), "1 мин 30 с");
        assert_eq!(pause_words(3600), "1 ч");
        assert_eq!(pause_words(86_400), "1 д");
        assert_eq!(pause_words(0), "0 с");
    }

    #[test]
    fn the_schema_names_every_event() {
        let (create, list, delete) = tools();
        let s = create.spec();
        let events = s.parameters["properties"]["event"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(events.len(), 10);
        for e in TriggerEvent::ALL {
            assert!(events.contains(&json!(e.as_str())));
            assert!(s.description.contains(e.as_str()), "{}", e.as_str());
        }
        assert_eq!(s.parameters["required"], json!(["name", "event"]));
        assert_eq!(list.spec().name, "trigger_list");
        assert_eq!(delete.spec().name, "trigger_delete");
    }
}
