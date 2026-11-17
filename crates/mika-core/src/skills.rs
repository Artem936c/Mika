//! Skills (decision 0047): a name, a short description, named parameters
//! and the steps she repeats on request, each one a tool and its arguments.
//!
//! A skill is made from what she did, not typed by hand. It is replayed by
//! the agent loop step by step, each step through the gate exactly as a
//! call of the model: a skill never carries a permission of its own.

mod book;

use std::collections::BTreeSet;
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::confirm::clean;
use crate::storage::{Db, DbError};

pub use book::{is_read, Offer, Recorded, Recording, SkillBook, OFFER_MIN_ACTIONS};

pub const MAX_SKILL_NAME: usize = 60;
pub const MAX_SKILL_ABOUT: usize = 300;
pub const MAX_SKILL_STEPS: usize = 20;
pub const MAX_SKILL_PARAMS: usize = 8;
pub const MAX_PARAM_NAME: usize = 30;
pub const MAX_PARAM_ABOUT: usize = 200;
/// The steps of one skill as JSON, characters.
pub const MAX_STEPS_JSON: usize = 20_000;

/// Tools a skill never repeats. The skill tools themselves: a skill that
/// runs a skill could run itself. The plan of a task: it belongs to the
/// turn that made it. The user's commands: making and removing them is a
/// decision of its own, not a step of a routine.
pub const NOT_REPLAYED: &[&str] = &[
    "skill_save",
    "skill_list",
    "skill_delete",
    "skill_run",
    "task_plan",
    "task_step",
    "command_save",
    "command_list",
    "command_delete",
];

/// Tools a recording ignores: the same ones.
pub fn is_service(tool: &str) -> bool {
    NOT_REPLAYED.contains(&tool)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillStep {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillParam {
    pub name: String,
    /// What the value is, in a few words.
    #[serde(default)]
    pub about: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Skill {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub params: Vec<SkillParam>,
    pub steps: Vec<SkillStep>,
}

/// A skill before it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSkill {
    pub name: String,
    pub description: String,
    pub params: Vec<SkillParam>,
    pub steps: Vec<SkillStep>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SkillError {
    #[error("у навыка должно быть имя из букв или цифр, до 60 символов")]
    BadName,
    #[error("описание навыка — не больше 300 символов")]
    BadDescription,
    #[error("в навыке должно быть от 1 до 20 шагов, всего не больше 20 000 символов")]
    BadSteps,
    #[error("шаг {0}: {1}")]
    BadStep(usize, String),
    #[error("параметр «{0}»: {1}")]
    BadParam(String, String),
    #[error("имя «{0}» уже занято командой, выбери другое")]
    NameTaken(String),
    #[error("нет навыка «{0}»")]
    NotFound(String),
    #[error("не хватает значения параметра «{0}»")]
    MissingParam(String),
    #[error("у навыка нет параметра «{0}»")]
    UnknownParam(String),
    #[error("навыки недоступны: {0}")]
    Db(String),
}

impl From<DbError> for SkillError {
    fn from(e: DbError) -> Self {
        SkillError::Db(e.to_string())
    }
}

/// Whether a name can stand between `{{` and `}}`.
fn is_param_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_PARAM_NAME
        && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// The names of the placeholders in one text, in order, repeats included.
fn names_in(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            break;
        };
        let inner = after[..close].trim();
        if is_param_name(inner) {
            out.push(inner);
            rest = &after[close + 2..];
        } else {
            rest = after;
        }
    }
    out
}

/// Every placeholder in the strings of `value`.
pub fn placeholders(value: &Value) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    walk(value, &mut |s| {
        found.extend(names_in(s).into_iter().map(str::to_string));
    });
    found
}

fn walk(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::String(s) => f(s),
        Value::Array(items) => items.iter().for_each(|v| walk(v, f)),
        Value::Object(map) => map.values().for_each(|v| walk(v, f)),
        _ => {}
    }
}

/// The text of a value as it goes inside a longer string.
fn as_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `args` with the placeholders filled. A string that is only a
/// placeholder takes the value itself, a number stays a number; inside a
/// longer string the value goes in as text. Placeholders without a value
/// stay as they are.
pub fn fill(args: &Value, values: &Map<String, Value>) -> Value {
    match args {
        Value::String(s) => {
            let names = names_in(s);
            if names.len() == 1 && s.trim() == format!("{{{{{}}}}}", names[0]) {
                if let Some(v) = values.get(names[0]) {
                    return v.clone();
                }
            }
            let mut out = s.clone();
            for (name, v) in values {
                for spelled in [format!("{{{{{name}}}}}"), format!("{{{{ {name} }}}}")] {
                    out = out.replace(&spelled, &as_text(v));
                }
            }
            Value::String(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| fill(v, values)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), fill(v, values)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Turns a concrete value of the steps into the placeholder of `param`:
/// a string or a number equal to it becomes `{{param}}`, and a text of
/// at least two characters is replaced inside longer strings too. Returns
/// how many places changed.
pub fn generalize(steps: &mut [SkillStep], param: &str, value: &Value) -> usize {
    let mark = format!("{{{{{param}}}}}");
    let text = as_text(value);
    if text.trim().is_empty() {
        return 0;
    }
    let mut changed = 0;
    for step in steps.iter_mut() {
        swap(&mut step.args, value, &text, &mark, &mut changed);
    }
    changed
}

fn swap(v: &mut Value, value: &Value, text: &str, mark: &str, changed: &mut usize) {
    match v {
        Value::String(s) if s == text => {
            *s = mark.to_string();
            *changed += 1;
        }
        Value::String(s) if text.chars().count() >= 2 && s.contains(text) => {
            *s = s.replace(text, mark);
            *changed += 1;
        }
        Value::Number(_) | Value::Bool(_) if &*v == value => {
            *v = Value::String(mark.to_string());
            *changed += 1;
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|x| swap(x, value, text, mark, changed)),
        Value::Object(map) => map
            .values_mut()
            .for_each(|x| swap(x, value, text, mark, changed)),
        _ => {}
    }
}

/// Whether a tool name has the shape of one: what the model may call.
fn is_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A skill as it may be stored: the checks that need no tools. The tools
/// of the steps are checked against the registry by `skill_save`.
pub fn check(skill: NewSkill) -> Result<NewSkill, SkillError> {
    let name = skill.name.trim().to_string();
    if clean(&name).is_empty() || name.chars().count() > MAX_SKILL_NAME {
        return Err(SkillError::BadName);
    }
    let description = skill.description.trim().to_string();
    if description.chars().count() > MAX_SKILL_ABOUT {
        return Err(SkillError::BadDescription);
    }
    if skill.steps.is_empty() || skill.steps.len() > MAX_SKILL_STEPS {
        return Err(SkillError::BadSteps);
    }
    let mut steps = Vec::with_capacity(skill.steps.len());
    for (i, step) in skill.steps.into_iter().enumerate() {
        let n = i + 1;
        let tool = step.tool.trim().to_string();
        if !is_tool_name(&tool) {
            return Err(SkillError::BadStep(n, "нет такого инструмента".into()));
        }
        if NOT_REPLAYED.contains(&tool.as_str()) {
            return Err(SkillError::BadStep(
                n,
                format!("`{tool}` в навыке не повторяется"),
            ));
        }
        let args = match step.args {
            Value::Null => Value::Object(Map::new()),
            v @ Value::Object(_) => v,
            _ => {
                return Err(SkillError::BadStep(
                    n,
                    "аргументы должны быть объектом".into(),
                ))
            }
        };
        steps.push(SkillStep { tool, args });
    }
    let size = serde_json::to_string(&steps).map_or(usize::MAX, |s| s.chars().count());
    if size > MAX_STEPS_JSON {
        return Err(SkillError::BadSteps);
    }
    if skill.params.len() > MAX_SKILL_PARAMS {
        return Err(SkillError::BadParam(
            skill.params[MAX_SKILL_PARAMS].name.clone(),
            "параметров не больше восьми".into(),
        ));
    }
    let mut declared = BTreeSet::new();
    let mut params = Vec::with_capacity(skill.params.len());
    for p in skill.params {
        let pname = p.name.trim().to_string();
        if !is_param_name(&pname) {
            return Err(SkillError::BadParam(
                pname,
                "имя из букв, цифр и _, до 30 символов".into(),
            ));
        }
        if !declared.insert(pname.clone()) {
            return Err(SkillError::BadParam(pname, "повторяется".into()));
        }
        let about = p.about.trim().to_string();
        if about.chars().count() > MAX_PARAM_ABOUT {
            return Err(SkillError::BadParam(
                pname,
                "описание до 200 символов".into(),
            ));
        }
        params.push(SkillParam { name: pname, about });
    }
    let used: BTreeSet<String> = steps.iter().flat_map(|s| placeholders(&s.args)).collect();
    if let Some(name) = used.difference(&declared).next() {
        return Err(SkillError::BadParam(
            name.clone(),
            "стоит в шагах, но не объявлен".into(),
        ));
    }
    if let Some(name) = declared.difference(&used).next() {
        return Err(SkillError::BadParam(
            name.clone(),
            "объявлен, но не стоит ни в одном шаге".into(),
        ));
    }
    Ok(NewSkill {
        name,
        description,
        params,
        steps,
    })
}

/// The steps of `skill` with the values of `given` in place of its
/// parameters, ready to run. Every parameter needs a value; a value for a
/// parameter the skill has not is an error, so a misspelt name is noticed.
pub fn resolve(skill: &Skill, given: &Map<String, Value>) -> Result<Vec<SkillStep>, SkillError> {
    if let Some(name) = given
        .keys()
        .find(|k| !skill.params.iter().any(|p| &p.name == *k))
    {
        return Err(SkillError::UnknownParam(name.clone()));
    }
    if let Some(p) = skill.params.iter().find(|p| {
        given
            .get(&p.name)
            .is_none_or(|v| as_text(v).trim().is_empty())
    }) {
        return Err(SkillError::MissingParam(p.name.clone()));
    }
    skill
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            // A row restored from a copy was checked there; this keeps the
            // promise that no skill ever runs a skill, whatever its row says.
            if NOT_REPLAYED.contains(&s.tool.as_str()) {
                return Err(SkillError::BadStep(
                    i + 1,
                    format!("`{}` в навыке не повторяется", s.tool),
                ));
            }
            Ok(SkillStep {
                tool: s.tool.clone(),
                args: fill(&s.args, given),
            })
        })
        .collect()
}

/// The steps as one line for a card: `1. open_app; 2. run_shell`.
pub fn steps_line(steps: &[SkillStep]) -> String {
    steps
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s.tool))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The message the assistant gets instead of the skill name.
pub fn expand(skill: &Skill) -> String {
    let mut text = format!(
        "{}\n\nЭто мой сохранённый навык. Вызови skill_run с name «{}».",
        skill.name, skill.name
    );
    if !skill.params.is_empty() {
        let list: Vec<String> = skill
            .params
            .iter()
            .map(|p| {
                if p.about.is_empty() {
                    p.name.clone()
                } else {
                    format!("{} ({})", p.name, p.about)
                }
            })
            .collect();
        text.push_str(&format!(
            " Ему нужны значения: {}. Если их нет в моём сообщении, спроси меня.",
            list.join(", ")
        ));
    }
    text
}

/// Longest arguments of one step in the note under a save request, characters.
const NOTE_ARGS: usize = 300;

/// The message the model gets when the user asks to save the last recording
/// as a skill with the chip under the reply: `shown` and the numbered steps.
pub fn save_request(shown: &str, recording: &Recording) -> String {
    let lines: Vec<String> = recording
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let args = s.args.to_string();
            let args: String = if args.chars().count() > NOTE_ARGS {
                args.chars().take(NOTE_ARGS).chain("…".chars()).collect()
            } else {
                args
            };
            let read = if s.read { " (чтение)" } else { "" };
            format!("{}. {} {args}{read}", i + 1, s.tool)
        })
        .collect();
    format!(
        "{shown}\n\n(служебная заметка) Сохрани как навык действия, которые ты только что сделала. \
Записанные шаги:\n{}\nВызови skill_save: придумай короткое имя и описание, в keep оставь номера нужных шагов \
(шаги чтения, которые только что-то искали, можно убрать), а значения, которые в другой раз будут другими, \
сделай параметрами (params с value).",
        lines.join("\n")
    )
}

#[derive(Clone)]
pub struct SkillStore {
    db: Arc<Db>,
}

impl SkillStore {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    /// Checks and saves a skill; the same name (case and punctuation
    /// ignored) replaces the old one. A name of a command is refused.
    pub fn save(&self, skill: NewSkill) -> Result<Skill, SkillError> {
        let skill = check(skill)?;
        let key = clean(&skill.name);
        let params_json =
            serde_json::to_string(&skill.params).map_err(|e| SkillError::Db(e.to_string()))?;
        let steps_json =
            serde_json::to_string(&skill.steps).map_err(|e| SkillError::Db(e.to_string()))?;
        let id = self.db.with(|c| {
            let taken: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM commands WHERE key = ?1)",
                [&key],
                |r| r.get(0),
            )?;
            if taken {
                return Ok(None);
            }
            c.execute(
                "INSERT INTO skills (name, key, description, params, steps)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(key) DO UPDATE SET name = ?1, description = ?3, params = ?4, steps = ?5",
                params![skill.name, key, skill.description, params_json, steps_json],
            )?;
            c.query_row("SELECT id FROM skills WHERE key = ?1", [&key], |r| r.get(0))
                .map(Some)
        })?;
        let Some(id) = id else {
            return Err(SkillError::NameTaken(skill.name));
        };
        Ok(Skill {
            id,
            name: skill.name,
            description: skill.description,
            params: skill.params,
            steps: skill.steps,
        })
    }

    /// Whether a command already has this name: then no skill takes it.
    pub fn taken_by_command(&self, name: &str) -> bool {
        let key = clean(name);
        self.db
            .with(|c| {
                c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM commands WHERE key = ?1)",
                    [key],
                    |r| r.get(0),
                )
            })
            .unwrap_or(false)
    }

    pub fn list(&self) -> Result<Vec<Skill>, SkillError> {
        let rows = self.db.with(|c| {
            let mut st =
                c.prepare("SELECT id, name, description, params, steps FROM skills ORDER BY name")?;
            let rows = st.query_map([], row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })?;
        Ok(rows.into_iter().flatten().collect())
    }

    pub fn delete(&self, name: &str) -> Result<bool, SkillError> {
        let key = clean(name);
        Ok(self
            .db
            .with(|c| c.execute("DELETE FROM skills WHERE key = ?1", [key]))?
            > 0)
    }

    /// The skill whose name equals the whole message.
    pub fn find(&self, message: &str) -> Option<Skill> {
        let key = clean(message);
        if key.is_empty() {
            return None;
        }
        self.db
            .with(|c| {
                c.query_row(
                    "SELECT id, name, description, params, steps FROM skills WHERE key = ?1",
                    [key],
                    row,
                )
                .optional()
            })
            .unwrap_or_else(|e| {
                log::error!(target: "tool", "cannot read skills: {e}");
                None
            })
            .flatten()
    }
}

/// A stored row; `None` for one whose steps do not parse.
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Skill>> {
    let id: i64 = r.get(0)?;
    let params: String = r.get(3)?;
    let steps: String = r.get(4)?;
    let (Ok(params), Ok(steps)) = (
        serde_json::from_str::<Vec<SkillParam>>(&params),
        serde_json::from_str::<Vec<SkillStep>>(&steps),
    ) else {
        log::warn!(target: "tool", "a stored skill does not parse and is skipped: id={id}");
        return Ok(None);
    };
    Ok(Some(Skill {
        id,
        name: r.get(1)?,
        description: r.get(2)?,
        params,
        steps,
    }))
}

/// Whether a row of a copy could have been written by `SkillStore::save`:
/// the same checks, the key made from the name.
pub fn row_is_valid(name: &str, key: &str, description: &str, params: &str, steps: &str) -> bool {
    let (Ok(params), Ok(steps)) = (
        serde_json::from_str::<Vec<SkillParam>>(params),
        serde_json::from_str::<Vec<SkillStep>>(steps),
    ) else {
        return false;
    };
    let new = NewSkill {
        name: name.to_string(),
        description: description.to_string(),
        params: params.clone(),
        steps: steps.clone(),
    };
    match check(new) {
        Ok(checked) => {
            key == clean(name)
                && checked.name == name
                && checked.description == description
                && checked.params == params
                && checked.steps == steps
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> SkillStore {
        SkillStore::new(Arc::new(Db::open_in_memory().unwrap()))
    }

    fn step(tool: &str, args: Value) -> SkillStep {
        SkillStep {
            tool: tool.into(),
            args,
        }
    }

    fn new(name: &str, steps: Vec<SkillStep>, params: &[&str]) -> NewSkill {
        NewSkill {
            name: name.into(),
            description: "утро".into(),
            params: params
                .iter()
                .map(|p| SkillParam {
                    name: p.to_string(),
                    about: String::new(),
                })
                .collect(),
            steps,
        }
    }

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn placeholders_are_found_in_every_string_of_the_arguments() {
        let args =
            json!({"a": "{{app}}", "b": ["x {{ city }} y", 3], "c": {"d": "{{bad name}}{{n2}}"}});
        let found: Vec<String> = placeholders(&args).into_iter().collect();
        assert_eq!(found, ["app", "city", "n2"]);
    }

    #[test]
    fn fill_puts_values_in_and_keeps_a_whole_number_a_number() {
        let args = json!({"level": "{{level}}", "text": "Привет, {{who}}! {{ who }}", "n": 5});
        let filled = fill(&args, &map(json!({"level": 30, "who": "Аня"})));
        assert_eq!(
            filled,
            json!({"level": 30, "text": "Привет, Аня! Аня", "n": 5})
        );
    }

    #[test]
    fn generalize_turns_a_concrete_value_into_a_placeholder() {
        let mut steps = vec![
            step("open_app", json!({"name": "notepad"})),
            step("run_shell", json!({"command": "start notepad.exe"})),
            step("volume", json!({"level": 30})),
        ];
        assert_eq!(generalize(&mut steps, "app", &json!("notepad")), 2);
        assert_eq!(generalize(&mut steps, "level", &json!(30)), 1);
        assert_eq!(generalize(&mut steps, "none", &json!("calc")), 0);
        assert_eq!(steps[0].args, json!({"name": "{{app}}"}));
        assert_eq!(steps[1].args, json!({"command": "start {{app}}.exe"}));
        assert_eq!(steps[2].args, json!({"level": "{{level}}"}));
    }

    #[test]
    fn resolve_fills_params_and_names_a_missing_or_unknown_one() {
        let s = store()
            .save(new(
                "Открой",
                vec![step("open_app", json!({"name": "{{app}}"}))],
                &["app"],
            ))
            .unwrap();
        assert_eq!(
            resolve(&s, &map(json!({"app": "calc"}))).unwrap(),
            vec![step("open_app", json!({"name": "calc"}))]
        );
        assert_eq!(
            resolve(&s, &Map::new()),
            Err(SkillError::MissingParam("app".into()))
        );
        assert_eq!(
            resolve(&s, &map(json!({"app": "  "}))),
            Err(SkillError::MissingParam("app".into()))
        );
        assert_eq!(
            resolve(&s, &map(json!({"app": "calc", "ap": "x"}))),
            Err(SkillError::UnknownParam("ap".into()))
        );
    }

    #[test]
    fn a_skill_never_holds_a_skill_a_plan_or_a_command() {
        for tool in [
            "skill_run",
            "skill_save",
            "task_plan",
            "task_step",
            "command_save",
        ] {
            assert!(
                matches!(
                    check(new("x", vec![step(tool, json!({}))], &[])),
                    Err(SkillError::BadStep(1, _))
                ),
                "{tool}"
            );
        }
        // A row from elsewhere that holds one does not run either.
        let s = Skill {
            id: 1,
            name: "x".into(),
            description: String::new(),
            params: vec![],
            steps: vec![
                step("get_time", json!({})),
                step("skill_run", json!({"name": "x"})),
            ],
        };
        assert!(matches!(
            resolve(&s, &Map::new()),
            Err(SkillError::BadStep(2, _))
        ));
    }

    #[test]
    fn params_must_match_the_placeholders_of_the_steps() {
        let steps = || vec![step("open_app", json!({"name": "{{app}}"}))];
        assert!(matches!(
            check(new("x", steps(), &[])),
            Err(SkillError::BadParam(p, _)) if p == "app"
        ));
        assert!(matches!(
            check(new("x", steps(), &["app", "city"])),
            Err(SkillError::BadParam(p, _)) if p == "city"
        ));
        assert!(matches!(
            check(new("x", steps(), &["app", "app"])),
            Err(SkillError::BadParam(p, _)) if p == "app"
        ));
        assert!(matches!(
            check(new("x", steps(), &["bad name"])),
            Err(SkillError::BadParam(_, _))
        ));
        assert!(check(new("x", steps(), &["app"])).is_ok());
    }

    #[test]
    fn bad_names_steps_and_arguments_are_refused() {
        let one = || vec![step("get_time", json!({}))];
        assert_eq!(check(new("!!!", one(), &[])), Err(SkillError::BadName));
        assert_eq!(
            check(new(&"я".repeat(MAX_SKILL_NAME + 1), one(), &[])),
            Err(SkillError::BadName)
        );
        assert_eq!(check(new("x", vec![], &[])), Err(SkillError::BadSteps));
        assert_eq!(
            check(new(
                "x",
                vec![step("get_time", json!({})); MAX_SKILL_STEPS + 1],
                &[]
            )),
            Err(SkillError::BadSteps)
        );
        assert!(matches!(
            check(new("x", vec![step("get_time", json!([1]))], &[])),
            Err(SkillError::BadStep(1, _))
        ));
        assert!(matches!(
            check(new("x", vec![step("open app", json!({}))], &[])),
            Err(SkillError::BadStep(1, _))
        ));
        // No arguments are an empty object.
        let ok = check(new("x", vec![step("get_time", Value::Null)], &[])).unwrap();
        assert_eq!(ok.steps[0].args, json!({}));
    }

    #[test]
    fn saved_skills_are_found_by_name_replaced_listed_and_deleted() {
        let s = store();
        s.save(new("Утро", vec![step("get_time", json!({}))], &[]))
            .unwrap();
        s.save(new(
            "утро!",
            vec![
                step("get_time", json!({})),
                step("get_weather", json!({"city": "Омск"})),
            ],
            &[],
        ))
        .unwrap();
        let all = s.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "утро!");
        assert_eq!(all[0].steps.len(), 2);
        assert_eq!(
            s.find("  УТРО ").unwrap().steps[1].args,
            json!({"city": "Омск"})
        );
        assert!(s.find("доброе утро").is_none());
        assert!(s.delete("утро").unwrap());
        assert!(!s.delete("утро").unwrap());
        assert!(s.list().unwrap().is_empty());
    }

    #[test]
    fn a_skill_cannot_take_the_name_of_a_command() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        crate::commands::CommandStore::new(db.clone())
            .save("Рабочий режим", "Открой Блокнот")
            .unwrap();
        let s = SkillStore::new(db);
        assert_eq!(
            s.save(new(
                "рабочий  режим",
                vec![step("get_time", json!({}))],
                &[]
            )),
            Err(SkillError::NameTaken("рабочий  режим".into()))
        );
    }

    #[test]
    fn expand_names_the_skill_and_asks_for_its_values() {
        let s = Skill {
            id: 1,
            name: "Открой".into(),
            description: String::new(),
            params: vec![SkillParam {
                name: "app".into(),
                about: "какую программу".into(),
            }],
            steps: vec![step("open_app", json!({"name": "{{app}}"}))],
        };
        let text = expand(&s);
        assert!(text.starts_with("Открой\n\n"));
        assert!(text.contains("skill_run") && text.contains("app (какую программу)"));
    }

    #[test]
    fn a_row_of_a_copy_is_valid_only_as_the_store_would_write_it() {
        let steps = r#"[{"tool":"get_time","args":{}}]"#;
        assert!(row_is_valid("Утро", "утро", "", "[]", steps));
        assert!(!row_is_valid("Утро", "вечер", "", "[]", steps));
        assert!(!row_is_valid("Утро", "утро", "", "[]", "not json"));
        assert!(!row_is_valid(
            "Утро",
            "утро",
            "",
            "[]",
            r#"[{"tool":"skill_run","args":{"name":"Утро"}}]"#
        ));
        assert!(!row_is_valid(
            "Утро",
            "утро",
            "",
            "[]",
            r#"[{"tool":"open_app","args":{"name":"{{app}}"}}]"#
        ));
    }

    #[test]
    fn a_save_request_numbers_the_steps_and_marks_the_reads() {
        let rec = Recording {
            id: 4,
            steps: vec![
                Recorded {
                    tool: "list_windows".into(),
                    args: json!({}),
                    read: true,
                },
                Recorded {
                    tool: "open_app".into(),
                    args: json!({"name": "notepad"}),
                    read: false,
                },
            ],
        };
        let text = save_request("Сохрани это как навык", &rec);
        assert!(text.starts_with("Сохрани это как навык\n\n"));
        assert!(text.contains("1. list_windows {} (чтение)"));
        assert!(text.contains("2. open_app {\"name\":\"notepad\"}\n"));
        assert!(text.contains("skill_save"));
    }
}
