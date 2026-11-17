//! Skills (decision 0047): what she did, saved to be done again by name.
//!
//! `skill_run` does not run tools itself: it hands the steps to the agent
//! loop, which runs each one through the gate like a call of the model.

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::settings;
use mika_core::skills::{
    self as core, NewSkill, SkillBook, SkillError, SkillParam, SkillStep, SkillStore,
};
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, Replay, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use crate::util::{failed, schema, truncate};

fn bad(e: SkillError) -> ToolError {
    ToolError::BadArguments(e.to_string())
}

/// A step that turns on what she hears, sees or writes down is not saved:
/// that is turned on by words every time (decision 0046), never as a step
/// among others. Turning such a thing off is a fine step.
fn settings_problem(args: &Value) -> Option<String> {
    let raw = args.get("value")?;
    if !core::placeholders(raw).is_empty() {
        return Some("значение настройки в навыке не может быть параметром".into());
    }
    let setting = settings::find(args.get("key")?.as_str()?)?;
    let off = settings::Value::Bool(false);
    if setting.risk_of(&off, &settings::Value::Bool(true)) < RiskLevel::High {
        return None;
    }
    match setting.parse(raw, &off, &[]) {
        Ok(settings::Value::Bool(false)) => None,
        _ => Some(format!(
            "включать {} нужно каждый раз словами, в навык это не входит",
            setting.name
        )),
    }
}

/// The steps of a call: given in `steps`, or taken from the last recording
/// (`keep` picks the numbers of the recorded steps, in its order).
fn steps_of(args: &Value, book: &SkillBook) -> Result<Vec<SkillStep>, ToolError> {
    if let Some(given) = args
        .get("steps")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        return given
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let tool = s
                    .get("tool")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::BadArguments(format!("у шага {} нет tool", i + 1)))?;
                Ok(SkillStep {
                    tool: tool.to_string(),
                    args: s.get("args").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
    }
    let Some(recording) = book.last() else {
        return Err(failed(
            "нечего сохранять: я ещё ничего не сделала в этом разговоре. Сначала сделай шаги или передай их в steps",
        ));
    };
    let picked: Vec<usize> = match args.get("keep").and_then(Value::as_array) {
        Some(keep) if !keep.is_empty() => keep
            .iter()
            .map(|n| {
                n.as_u64()
                    .map(|n| n as usize)
                    .filter(|n| (1..=recording.steps.len()).contains(n))
                    .ok_or_else(|| {
                        ToolError::BadArguments(format!(
                            "в keep номера шагов от 1 до {}",
                            recording.steps.len()
                        ))
                    })
            })
            .collect::<Result<_, _>>()?,
        _ => (1..=recording.steps.len()).collect(),
    };
    Ok(picked
        .into_iter()
        .map(|n| {
            let s = &recording.steps[n - 1];
            SkillStep {
                tool: s.tool.clone(),
                args: s.args.clone(),
            }
        })
        .collect())
}

pub struct SkillSaveTool {
    pub store: SkillStore,
    pub book: SkillBook,
}

impl SkillSaveTool {
    /// The skill a call would save, checked as far as it can be before it runs.
    fn prepare(&self, args: &Value) -> Result<NewSkill, ToolError> {
        let name = arg_str(args, "name")?.trim().to_string();
        let mut steps = steps_of(args, &self.book)?;
        let mut params = Vec::new();
        for p in args
            .get("params")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let pname = p
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::BadArguments("у параметра нет name".into()))?
                .trim()
                .to_string();
            let about = p
                .get("description")
                .or_else(|| p.get("about"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(value) = p.get("value").filter(|v| !v.is_null()) {
                if core::generalize(&mut steps, &pname, value) == 0 {
                    return Err(ToolError::BadArguments(format!(
                        "значение параметра «{pname}» не встречается в шагах"
                    )));
                }
            }
            params.push(SkillParam { name: pname, about });
        }
        let skill = core::check(NewSkill {
            name,
            description: arg_str_opt(args, "description")
                .unwrap_or_default()
                .to_string(),
            params,
            steps,
        })
        .map_err(bad)?;
        if self.store.taken_by_command(&skill.name) {
            return Err(bad(SkillError::NameTaken(skill.name)));
        }
        if let Some(registry) = self.book.registry() {
            for (i, step) in skill.steps.iter().enumerate() {
                let n = i + 1;
                let Some(tool) = registry.get(&step.tool) else {
                    return Err(bad(SkillError::BadStep(
                        n,
                        format!("инструмента `{}` нет", step.tool),
                    )));
                };
                if step.tool == "settings_set" {
                    if let Some(why) = settings_problem(&step.args) {
                        return Err(bad(SkillError::BadStep(n, why)));
                    }
                }
                // With parameters the arguments are known only at the run.
                if !core::placeholders(&step.args).is_empty() {
                    continue;
                }
                // Wrong arguments never work. What does not work now (no
                // such window open, a part of the app not ready) may at
                // the run: that step then stops the skill and says why.
                if let Err(e @ ToolError::BadArguments(_)) = tool.assess(&step.args) {
                    return Err(bad(SkillError::BadStep(n, e.to_string())));
                }
            }
        }
        Ok(skill)
    }
}

/// The steps with their arguments, one per line, for the model.
fn steps_text(steps: &[SkillStep]) -> String {
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "{}. {} {}",
                i + 1,
                s.tool,
                truncate(&s.args.to_string(), 300)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn params_text(params: &[SkillParam]) -> String {
    params
        .iter()
        .map(|p| {
            if p.about.is_empty() {
                p.name.clone()
            } else {
                format!("{} ({})", p.name, p.about)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[async_trait]
impl Tool for SkillSaveTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "skill_save",
            "Сохранить навык: порядок действий, который потом выполняется по имени через skill_run. Без steps берёт твои последние действия (шаги прошлого ответа), keep — номера шагов, которые оставить. Значения, которые в другой раз будут другими, сделай параметрами: params с name, description и value — это значение из шагов станет {{name}}. Пример: name «рабочий режим», description «Блокнот, почта и тише звук».",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Короткое имя навыка, как его будет писать пользователь"},
                    "description": {"type": "string", "description": "Что делает навык, одной фразой"},
                    "keep": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "description": "Номера записанных шагов, которые оставить, по порядку; без него — все"
                    },
                    "steps": {
                        "type": "array",
                        "description": "Шаги явно, если навык не из последних действий",
                        "items": {
                            "type": "object",
                            "properties": {
                                "tool": {"type": "string"},
                                "args": {"type": "object"}
                            },
                            "required": ["tool"]
                        }
                    },
                    "params": {
                        "type": "array",
                        "description": "Параметры навыка",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string", "description": "Имя из букв, цифр и _"},
                                "description": {"type": "string", "description": "Что это за значение"},
                                "value": {"description": "Значение из шагов, которое станет этим параметром"}
                            },
                            "required": ["name"]
                        }
                    }
                },
                "required": ["name"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let skill = self.prepare(args)?;
        let mut target = core::steps_line(&skill.steps);
        if !skill.params.is_empty() {
            target.push_str(&format!("; параметры: {}", params_text(&skill.params)));
        }
        Ok(Assessment::new(
            RiskLevel::Medium,
            format!("Сохранить навык «{}»", skill.name),
            target,
            "навык будет выполняться по имени; каждый его шаг всё равно спросит разрешение, если оно нужно",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let skill = self.store.save(self.prepare(&args)?).map_err(bad)?;
        log::info!(
            target: "tool",
            "skill saved: name={} steps={} params={}",
            mika_core::journal::content(&skill.name),
            skill.steps.len(),
            skill.params.len()
        );
        let mut text = format!(
            "Навык «{}» сохранён. Шаги:\n{}",
            skill.name,
            steps_text(&skill.steps)
        );
        if !skill.params.is_empty() {
            text.push_str(&format!("\nПараметры: {}.", params_text(&skill.params)));
        }
        text.push_str(&format!(
            "\nЧтобы выполнить его, достаточно написать «{}».",
            skill.name
        ));
        Ok(ToolOutput::new(text))
    }
}

pub struct SkillListTool(pub SkillStore);

#[async_trait]
impl Tool for SkillListTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "skill_list",
            "Показать сохранённые навыки: имя, описание, параметры и шаги.",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть свои навыки",
            "навыки",
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
            return Ok(ToolOutput::new("Навыков пока нет."));
        }
        let lines: Vec<String> = list
            .iter()
            .map(|s| {
                let mut line = format!("«{}»", s.name);
                if !s.description.is_empty() {
                    line.push_str(&format!(": {}", s.description));
                }
                if !s.params.is_empty() {
                    line.push_str(&format!("; параметры: {}", params_text(&s.params)));
                }
                line.push_str(&format!("; шаги: {}", core::steps_line(&s.steps)));
                line
            })
            .collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

pub struct SkillDeleteTool(pub SkillStore);

#[async_trait]
impl Tool for SkillDeleteTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "skill_delete",
            "Удалить сохранённый навык по имени.",
            schema(&[("name", "string", "Имя навыка")], &["name"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let name = arg_str(args, "name")?.trim();
        Ok(Assessment::new(
            RiskLevel::Medium,
            format!("Удалить навык «{name}»"),
            name,
            "его шаги пропадут, вернуть навык можно, только сохранив заново",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let name = arg_str(&args, "name")?.trim();
        if self.0.delete(name).map_err(|e| failed(e.to_string()))? {
            log::info!(
                target: "tool",
                "skill deleted: name={}",
                mika_core::journal::content(name)
            );
            Ok(ToolOutput::new(format!("Навык «{name}» удалён.")))
        } else {
            Err(failed(format!("нет навыка «{name}»")))
        }
    }
}

pub struct SkillRunTool(pub SkillStore);

impl SkillRunTool {
    /// The skill and its steps with the values filled.
    fn resolve(&self, args: &Value) -> Result<(String, Vec<SkillStep>), ToolError> {
        let name = arg_str(args, "name")?.trim();
        let skill = self
            .0
            .find(name)
            .ok_or_else(|| failed(SkillError::NotFound(name.to_string()).to_string()))?;
        let given: Map<String, Value> = match args.get("params") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => {
                return Err(ToolError::BadArguments(
                    "params — объект «имя параметра: значение»".into(),
                ))
            }
        };
        let steps = core::resolve(&skill, &given).map_err(bad)?;
        Ok((skill.name, steps))
    }
}

#[async_trait]
impl Tool for SkillRunTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "skill_run",
            "Выполнить сохранённый навык по имени. Шаги идут по одному, каждый со своей проверкой; если шаг не вышел, остальные не запускаются, и ты узнаешь, какой шаг и почему. Значения параметров навыка — в params, например {\"app\": \"калькулятор\"}.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Имя навыка"},
                    "params": {"type": "object", "description": "Значения параметров навыка: имя — значение"}
                },
                "required": ["name"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let (name, steps) = self.resolve(args)?;
        Ok(Assessment::new(
            RiskLevel::Low,
            format!("Выполнить навык «{name}»"),
            core::steps_line(&steps),
            "каждый шаг навыка проходит свою проверку и спросит разрешение, если оно нужно",
        )
        .with_scope(mika_core::confirm::clean(&name)))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let (name, steps) = self.resolve(&args)?;
        log::info!(
            target: "tool",
            "skill run started: name={} steps={}",
            mika_core::journal::content(&name),
            steps.len()
        );
        Ok(
            ToolOutput::new(format!("Выполняю навык «{name}», шагов: {}.", steps.len()))
                .with_replay(Replay { skill: name, steps }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use mika_core::storage::Db;
    use mika_core::tools::ToolRegistry;
    use std::sync::Arc;

    struct Kit {
        store: SkillStore,
        book: SkillBook,
        save: SkillSaveTool,
        // Keeps the tools the book points at alive.
        _registry: Arc<ToolRegistry>,
    }

    fn kit() -> Kit {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let (env, _) = crate::testing::env_with(db.clone(), "C:\\Users\\me");
        let mut reg = ToolRegistry::new();
        crate::register_all(&mut reg, &env, &[]);
        let registry = Arc::new(reg);
        let book = env.shared.skills.clone();
        book.set_registry(&registry);
        let store = SkillStore::new(db);
        Kit {
            save: SkillSaveTool {
                store: store.clone(),
                book: book.clone(),
            },
            store,
            book,
            _registry: registry,
        }
    }

    fn record(book: &SkillBook, calls: &[(&str, Value, bool)]) {
        book.begin_turn();
        for (tool, args, read) in calls {
            book.record(tool, args, *read);
        }
        book.end_turn(true);
    }

    #[tokio::test]
    async fn the_last_steps_are_saved_with_a_value_made_a_parameter() {
        let k = kit();
        record(
            &k.book,
            &[
                ("get_time", json!({}), true),
                (
                    "open_url",
                    json!({"url": "https://mail.example.com"}),
                    false,
                ),
                ("volume", json!({"action": "set", "level": 30}), false),
            ],
        );
        let args = json!({
            "name": "Почта",
            "description": "почта и тише",
            "keep": [2, 3],
            "params": [{"name": "level", "description": "громкость", "value": 30}]
        });
        let card = k.save.assess(&args).unwrap();
        assert_eq!(card.level, RiskLevel::Medium);
        assert_eq!(card.summary, "Сохранить навык «Почта»");
        assert_eq!(
            card.target,
            "1. open_url; 2. volume; параметры: level (громкость)"
        );
        let out = run(&k.save, args).await.unwrap();
        assert!(out.text.contains("«Почта» сохранён"), "{}", out.text);
        let s = k.store.find("почта").unwrap();
        assert_eq!(s.steps.len(), 2);
        assert_eq!(
            s.steps[1].args,
            json!({"action": "set", "level": "{{level}}"})
        );
        assert_eq!(s.params[0].name, "level");
    }

    #[tokio::test]
    async fn without_keep_every_recorded_step_is_kept_and_without_a_recording_nothing() {
        let k = kit();
        assert!(k.save.assess(&json!({"name": "x"})).is_err());
        record(
            &k.book,
            &[
                ("get_time", json!({}), true),
                ("open_url", json!({"url": "https://a.ru"}), false),
            ],
        );
        run(&k.save, json!({"name": "Всё"})).await.unwrap();
        assert_eq!(k.store.find("всё").unwrap().steps.len(), 2);
        assert!(k.save.assess(&json!({"name": "x", "keep": [3]})).is_err());
    }

    #[tokio::test]
    async fn unsafe_unknown_and_broken_steps_are_refused_at_save() {
        let k = kit();
        let refused = |steps: Value| {
            k.save
                .assess(&json!({"name": "x", "steps": steps}))
                .expect_err("refused")
                .to_string()
        };
        assert!(
            refused(json!([{"tool": "skill_run", "args": {"name": "x"}}]))
                .contains("не повторяется")
        );
        assert!(refused(json!([{"tool": "task_plan", "args": {}}])).contains("не повторяется"));
        assert!(refused(json!([{"tool": "rm_rf", "args": {}}])).contains("нет"));
        assert!(
            refused(json!([{"tool": "open_url", "args": {"url": "ftp://x"}}])).contains("шаг 1")
        );
        // Turning on what she hears is said every time, never saved.
        assert!(refused(
            json!([{"tool": "settings_set", "args": {"key": "privacy.microphone", "value": "вкл"}}])
        )
        .contains("словами"));
        assert!(refused(json!([{"tool": "settings_set", "args": {"key": "privacy.microphone", "value": "переключи"}}]))
            .contains("словами"));
        let e = k
            .save
            .assess(&json!({
                "name": "x",
                "steps": [{"tool": "settings_set", "args": {"key": "privacy.microphone", "value": "{{v}}"}}],
                "params": [{"name": "v"}]
            }))
            .unwrap_err();
        assert!(e.to_string().contains("параметром"), "{e}");
        // Turning it off, and what is not private, are fine steps.
        for args in [
            json!({"key": "privacy.microphone", "value": "выкл"}),
            json!({"key": "voice.volume", "value": 40}),
        ] {
            k.save
                .assess(&json!({"name": "x", "steps": [{"tool": "settings_set", "args": args}]}))
                .unwrap();
        }
        // A value that is nowhere in the steps cannot become a parameter.
        assert!(k
            .save
            .assess(&json!({
                "name": "x",
                "steps": [{"tool": "get_time", "args": {}}],
                "params": [{"name": "app", "value": "calc"}]
            }))
            .is_err());
    }

    #[tokio::test]
    async fn a_name_of_a_command_is_refused_before_the_card() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        mika_core::commands::CommandStore::new(db.clone())
            .save("Утро", "Расскажи погоду")
            .unwrap();
        let save = SkillSaveTool {
            store: SkillStore::new(db),
            book: SkillBook::default(),
        };
        let e = save
            .assess(&json!({"name": "утро", "steps": [{"tool": "get_time"}]}))
            .unwrap_err();
        assert!(e.to_string().contains("занято командой"), "{e}");
    }

    #[tokio::test]
    async fn run_fills_the_parameters_and_hands_the_steps_to_the_loop() {
        let k = kit();
        run(
            &k.save,
            json!({
                "name": "Сайт",
                "steps": [{"tool": "open_url", "args": {"url": "https://{{host}}/"}}],
                "params": [{"name": "host", "description": "адрес"}]
            }),
        )
        .await
        .unwrap();
        let tool = SkillRunTool(k.store.clone());
        let args = json!({"name": "сайт", "params": {"host": "ya.ru"}});
        let card = tool.assess(&args).unwrap();
        assert_eq!(card.level, RiskLevel::Low);
        assert_eq!(card.target, "1. open_url");
        let out = run(&tool, args).await.unwrap();
        let replay = out.replay.unwrap();
        assert_eq!(replay.skill, "Сайт");
        assert_eq!(replay.steps[0].args, json!({"url": "https://ya.ru/"}));
        // A missing value is named; so is a skill that is not there.
        let e = tool.assess(&json!({"name": "сайт"})).unwrap_err();
        assert!(e.to_string().contains("«host»"), "{e}");
        assert!(tool.assess(&json!({"name": "нет такого"})).is_err());
    }

    #[tokio::test]
    async fn list_and_delete() {
        let k = kit();
        let list = SkillListTool(k.store.clone());
        assert_eq!(
            run(&list, json!({})).await.unwrap().text,
            "Навыков пока нет."
        );
        run(
            &k.save,
            json!({"name": "Время", "description": "сказать время", "steps": [{"tool": "get_time"}]}),
        )
        .await
        .unwrap();
        assert_eq!(
            run(&list, json!({})).await.unwrap().text,
            "«Время»: сказать время; шаги: 1. get_time"
        );
        let delete = SkillDeleteTool(k.store.clone());
        assert_eq!(
            delete.assess(&json!({"name": "время"})).unwrap().level,
            RiskLevel::Medium
        );
        run(&delete, json!({"name": "время"})).await.unwrap();
        assert!(run(&delete, json!({"name": "время"})).await.is_err());
    }
}
