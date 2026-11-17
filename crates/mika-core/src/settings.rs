//! Settings she may change by conversation (decision 0046): the whitelist,
//! the risk of each change and the port the application applies it through.
//!
//! Nothing secret is in the list: no keys, tokens, addresses, models,
//! folders or hotkeys. The application applies a change the same way its
//! settings page does; this module only decides what may change and how risky
//! the change is.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::{json, Value as Json};

use crate::config::{Config, Persona, TtsEngine};
use crate::i18n::Lang;
use crate::tools::{Assessment, RiskLevel};

/// A value of a setting.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Number(f64),
    Text(String),
}

impl Value {
    pub fn to_json(&self) -> Json {
        match self {
            Value::Bool(b) => json!(b),
            Value::Number(n) => json!(n),
            Value::Text(t) => json!(t),
        }
    }
}

/// What kind of value a setting takes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// On or off.
    Switch,
    /// A number inside its bounds, rounded to `step`.
    Number {
        min: f64,
        max: f64,
        step: f64,
        unit: &'static str,
    },
    /// One of these ids, each with its Russian name.
    Choice(&'static [(&'static str, &'static str)]),
    /// One of the values the application lists, such as installed voices.
    /// Empty is «автоматически».
    Listed,
}

/// How risky a change is, by its direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Risk {
    /// Both directions are the same.
    Same(RiskLevel),
    /// Turning on and turning off differ.
    Switch { on: RiskLevel, off: RiskLevel },
    /// A limit where 0 is none: lowering is low, raising or removing it medium.
    Limit,
}

/// One setting of the whitelist.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Setting {
    pub key: &'static str,
    /// What changes, in the accusative: «Включить микрофон», «Изменить голос».
    pub name: &'static str,
    /// One line about it for the model.
    pub about: &'static str,
    pub kind: Kind,
    pub risk: Risk,
    /// It is turned on only at this computer, never from Telegram: what she
    /// hears, sees and writes down about the user.
    pub at_pc_only: bool,
}

const PRIVATE: Risk = Risk::Switch {
    on: RiskLevel::High,
    off: RiskLevel::Low,
};
const COMFORT: Risk = Risk::Same(RiskLevel::Low);

const PERSONAS: &[(&str, &str)] = &[
    ("caring", "заботливая"),
    ("cheerful", "весёлая"),
    ("calm", "спокойная"),
    ("bold", "дерзкая"),
];
const LANGUAGES: &[(&str, &str)] = &[("ru", "русский"), ("en", "английский")];

/// The keys whose value lives outside `config.toml`: the application reads them.
pub const OUTSIDE_CONFIG: &[&str] = &["voice.volume", "autostart"];

/// Every setting she may change by conversation.
pub const SETTINGS: &[Setting] = &[
    // What she hears, sees and keeps: on asks every time, off is safe.
    Setting {
        key: "privacy.microphone",
        name: "микрофон",
        about: "главный выключатель микрофона: без него она не слышит ни фраз, ни своего имени, ни диктовки",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: true,
    },
    Setting {
        key: "voice.wake",
        name: "активационную фразу",
        about: "слушать своё имя, пока микрофон разрешён",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: true,
    },
    Setting {
        key: "dictation",
        name: "диктовку",
        about: "диктовка в окно впереди по сочетанию клавиш; нужен микрофон",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: true,
    },
    Setting {
        key: "privacy.screen",
        name: "просмотр экрана",
        about: "главный выключатель экрана: снимки и чтение экрана",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: true,
    },
    Setting {
        key: "screen_ask",
        name: "вопрос про экран",
        about: "вопрос про окно впереди по сочетанию клавиш; нужен просмотр экрана",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: true,
    },
    Setting {
        key: "log.content",
        name: "запись текстов разговоров в журнал",
        about: "писать в журнал тексты разговоров и аргументы инструментов",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: true,
    },
    Setting {
        key: "notifications",
        name: "чтение уведомлений других программ",
        about: "читать уведомления мессенджеров и других программ",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: true,
    },
    Setting {
        key: "tracking.app_time",
        name: "учёт времени в программах",
        about: "запоминать, сколько времени открыта каждая программа",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: true,
    },
    Setting {
        key: "tracking.clipboard",
        name: "историю буфера обмена",
        about: "помнить последние скопированные тексты, пока она запущена",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: true,
    },
    Setting {
        key: "tools.web",
        name: "инструменты интернета",
        about: "поиск в интернете, страницы, погода, курсы, новости",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: false,
    },
    Setting {
        key: "telegram",
        name: "мост Telegram",
        about: "команды с телефона через бота Telegram",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: false,
    },
    Setting {
        key: "telegram.voice_in",
        name: "голосовые сообщения из Telegram",
        about: "распознавать голосовые сообщения из Telegram как команды",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: false,
    },
    Setting {
        key: "telegram.voice_reply",
        name: "голосовые ответы в Telegram",
        about: "на голосовое сообщение отвечать голосовым",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: false,
    },
    Setting {
        key: "events",
        name: "сценарии по событию",
        about: "сценарии, которые сами запускаются при событии",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: false,
    },
    Setting {
        key: "autostart",
        name: "запуск вместе с Windows",
        about: "запускаться при входе в Windows",
        kind: Kind::Switch,
        risk: PRIVATE,
        at_pc_only: false,
    },
    // Comfort: easy to change back.
    Setting {
        key: "voice.speak_replies",
        name: "озвучку ответов",
        about: "читать ответы вслух",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "voice.volume",
        name: "громкость голоса",
        about: "громкость её голоса в микшере Windows, 0–100; громкость всего компьютера меняет volume",
        kind: Kind::Number {
            min: 0.0,
            max: 100.0,
            step: 1.0,
            unit: "%",
        },
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "voice.rate",
        name: "скорость речи",
        about: "скорость голоса Windows: 1 обычная, меньше медленнее, больше быстрее",
        kind: Kind::Number {
            min: 0.5,
            max: 2.0,
            step: 0.1,
            unit: "",
        },
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "voice.voice",
        name: "голос",
        about: "каким голосом она говорит; варианты — установленные голоса",
        kind: Kind::Listed,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "voice.brief_long",
        name: "краткий пересказ длинных ответов вслух",
        about: "длинный ответ вслух не читать целиком, а пересказать коротко",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "voice.follow_up",
        name: "продолжение разговора голосом",
        about: "после ответа на голосовой вопрос слушать ещё несколько секунд",
        kind: Kind::Switch,
        risk: Risk::Switch {
            on: RiskLevel::Medium,
            off: RiskLevel::Low,
        },
        at_pc_only: false,
    },
    Setting {
        key: "voice.duck_media",
        name: "приглушение других программ",
        about: "делать другие программы тише, пока она слушает и говорит",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "voice.tone_by_mood",
        name: "тон голоса по настроению",
        about: "высота и темп голоса следуют её настроению",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "notifications.read_aloud",
        name: "чтение уведомлений вслух",
        about: "говорить уведомления вслух, когда ответы озвучиваются",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "assistant.persona",
        name: "характер",
        about: "её характер",
        kind: Kind::Choice(PERSONAS),
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "rhythm.lively",
        name: "свои реплики",
        about: "приветствия, «уже поздно», радость за сделанное: реплики без модели",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "rhythm.breaks",
        name: "напоминания о перерыве",
        about: "предлагать перерыв после долгой работы",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "rhythm.break_every_min",
        name: "интервал перерывов",
        about: "через сколько минут работы предлагать перерыв",
        kind: Kind::Number {
            min: 15.0,
            max: 240.0,
            step: 1.0,
            unit: " мин",
        },
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "skills.suggest",
        name: "предложения сохранить навык",
        about: "после трёх действий подряд предлагать сохранить их как навык",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "island.hide_in_games",
        name: "скрытие острова в играх",
        about: "прятать островок, пока впереди полноэкранная программа",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "avatar.in_island",
        name: "аватар в чате",
        about: "показывать аватар в левой колонке открытого чата",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "avatar.on_desktop",
        name: "аватар на рабочем столе",
        about: "показывать аватар в углу рабочего стола",
        kind: Kind::Switch,
        risk: COMFORT,
        at_pc_only: false,
    },
    Setting {
        key: "i18n.language",
        name: "язык интерфейса",
        about: "язык окон, меню и её ответов по умолчанию",
        kind: Kind::Choice(LANGUAGES),
        risk: Risk::Same(RiskLevel::Medium),
        at_pc_only: false,
    },
    Setting {
        key: "costs.day_limit",
        name: "дневной лимит расходов",
        about: "сколько можно потратить на модели за день, 0 — без лимита",
        kind: Kind::Number {
            min: 0.0,
            max: 1e9,
            step: 0.01,
            unit: "",
        },
        risk: Risk::Limit,
        at_pc_only: false,
    },
    Setting {
        key: "costs.month_limit",
        name: "месячный лимит расходов",
        about: "сколько можно потратить на модели за месяц, 0 — без лимита",
        kind: Kind::Number {
            min: 0.0,
            max: 1e9,
            step: 0.01,
            unit: "",
        },
        risk: Risk::Limit,
        at_pc_only: false,
    },
];

/// The setting of `key`, if she may change it.
pub fn find(key: &str) -> Option<&'static Setting> {
    let key = key.trim().to_lowercase();
    SETTINGS.iter().find(|s| s.key == key)
}

/// Every key, for an error that says what is allowed.
pub fn keys() -> String {
    SETTINGS
        .iter()
        .map(|s| s.key)
        .collect::<Vec<_>>()
        .join(", ")
}

fn persona_id(persona: Persona) -> &'static str {
    match persona {
        Persona::Caring => "caring",
        Persona::Cheerful => "cheerful",
        Persona::Calm => "calm",
        Persona::Bold => "bold",
        Persona::Custom => "custom",
    }
}

/// The value of `key` in `config`; `None` for a key the application reads
/// itself (`OUTSIDE_CONFIG`) and for a key outside the whitelist.
pub fn read(config: &Config, key: &str) -> Option<Value> {
    let on = |v: bool| Some(Value::Bool(v));
    let number = |v: f64, step: f64| Some(Value::Number(rounded(v, step)));
    let text = |v: &str| Some(Value::Text(v.to_string()));
    let (voice, rhythm, costs) = (&config.voice, &config.rhythm, &config.costs);
    match key {
        "privacy.microphone" => on(config.privacy.microphone_enabled),
        "voice.wake" => on(voice.wake_enabled),
        "dictation" => on(config.dictation.enabled),
        "privacy.screen" => on(config.privacy.screen_enabled),
        "screen_ask" => on(config.screen_ask.enabled),
        "log.content" => on(config.log.content),
        "notifications" => on(config.notifications.enabled),
        "tracking.app_time" => on(config.tracking.app_time),
        "tracking.clipboard" => on(config.tracking.clipboard),
        "tools.web" => on(config.tools.web_enabled),
        "telegram" => on(config.telegram.enabled),
        "telegram.voice_in" => on(config.telegram.voice_in),
        "telegram.voice_reply" => on(config.telegram.voice_reply),
        "events" => on(config.events.enabled),
        "voice.speak_replies" => on(voice.speak_replies),
        "voice.rate" => number(voice.rate as f64, 0.1),
        // The voice that speaks: the provider's when the provider speaks.
        "voice.voice" if voice.tts == TtsEngine::Provider => text(&voice.tts_voice),
        "voice.voice" => text(&voice.voice),
        "voice.brief_long" => on(voice.brief_long),
        "voice.follow_up" => on(voice.follow_up),
        "voice.duck_media" => on(voice.duck_media),
        "voice.tone_by_mood" => on(voice.tone_by_mood),
        "notifications.read_aloud" => on(config.notifications.read_aloud),
        "assistant.persona" => text(persona_id(config.assistant.persona)),
        "rhythm.lively" => on(rhythm.lively),
        "rhythm.breaks" => on(rhythm.breaks),
        "rhythm.break_every_min" => number(rhythm.break_every_min as f64, 1.0),
        "island.hide_in_games" => on(config.island.hide_in_games),
        "skills.suggest" => on(config.skills.suggest),
        "avatar.in_island" => on(config.avatar.in_island),
        "avatar.on_desktop" => on(config.avatar.on_desktop),
        "i18n.language" => text(
            Lang::from_code(&config.i18n.language)
                .unwrap_or(Lang::Ru)
                .code(),
        ),
        "costs.day_limit" => number(costs.day_limit, 0.01),
        "costs.month_limit" => number(costs.month_limit, 0.01),
        _ => None,
    }
}

/// Digits after the point of a step: 1 is 0, 0.1 is 1, 0.01 is 2.
fn decimals(step: f64) -> i32 {
    if step >= 1.0 {
        0
    } else {
        (-step.log10()).round() as i32
    }
}

/// `value` rounded to the digits of `step`.
fn rounded(value: f64, step: f64) -> f64 {
    let scale = 10f64.powi(decimals(step));
    (value * scale).round() / scale
}

const ON_WORDS: &[&str] = &[
    "on",
    "true",
    "1",
    "yes",
    "да",
    "вкл",
    "включи",
    "включить",
    "включено",
    "включена",
];
const OFF_WORDS: &[&str] = &[
    "off",
    "false",
    "0",
    "no",
    "нет",
    "выкл",
    "выключи",
    "выключить",
    "выключено",
    "выключена",
];
const TOGGLE_WORDS: &[&str] = &["toggle", "переключи", "переключить"];
const AUTO_WORDS: &[&str] = &["", "auto", "automatic", "авто", "автоматически"];
/// Most options a list or an error names.
const MAX_OPTIONS: usize = 30;

fn joined<'a>(items: impl Iterator<Item = &'a str>) -> String {
    items.take(MAX_OPTIONS).collect::<Vec<_>>().join(", ")
}

impl Setting {
    /// The value the user asked for. `raw` may be relative for a number:
    /// «+10», «-0.2» from `current`, cut to the bounds. `options` are the
    /// values of a `Listed` setting.
    pub fn parse(&self, raw: &Json, current: &Value, options: &[String]) -> Result<Value, String> {
        let wrong = || format!("для «{}» подходит: {}", self.key, self.allowed(options));
        match self.kind {
            Kind::Switch => {
                let word = match raw {
                    Json::Bool(b) => return Ok(Value::Bool(*b)),
                    Json::Number(n) => n.to_string(),
                    Json::String(s) => s.trim().to_lowercase(),
                    _ => return Err(wrong()),
                };
                if ON_WORDS.contains(&word.as_str()) {
                    Ok(Value::Bool(true))
                } else if OFF_WORDS.contains(&word.as_str()) {
                    Ok(Value::Bool(false))
                } else if TOGGLE_WORDS.contains(&word.as_str()) {
                    Ok(Value::Bool(*current != Value::Bool(true)))
                } else {
                    Err(wrong())
                }
            }
            Kind::Number { min, max, step, .. } => {
                let (number, shift) = match raw {
                    Json::Number(n) => (n.as_f64(), false),
                    Json::String(s) => {
                        let s = s.trim().replace(',', ".");
                        let shift = s.starts_with('+') || s.starts_with('-');
                        (s.parse::<f64>().ok(), shift)
                    }
                    _ => (None, false),
                };
                let number = number.filter(|n| n.is_finite()).ok_or_else(wrong)?;
                let value = if shift {
                    let now = match current {
                        Value::Number(n) => *n,
                        _ => min,
                    };
                    (now + number).clamp(min, max)
                } else if (min..=max).contains(&number) {
                    number
                } else {
                    return Err(wrong());
                };
                Ok(Value::Number(rounded(value, step).clamp(min, max)))
            }
            Kind::Choice(list) => raw
                .as_str()
                .map(|s| s.trim().to_lowercase())
                .and_then(|word| {
                    list.iter()
                        .find(|(id, name)| *id == word || name.to_lowercase() == word)
                })
                .map(|(id, _)| Value::Text(id.to_string()))
                .ok_or_else(wrong),
            Kind::Listed => {
                let word = raw
                    .as_str()
                    .map(|s| s.trim().to_lowercase())
                    .ok_or_else(wrong)?;
                if AUTO_WORDS.contains(&word.as_str()) {
                    return Ok(Value::Text(String::new()));
                }
                if let Some(exact) = options.iter().find(|o| o.to_lowercase() == word) {
                    return Ok(Value::Text(exact.clone()));
                }
                let near: Vec<&String> = options
                    .iter()
                    .filter(|o| o.to_lowercase().contains(&word))
                    .collect();
                match near.as_slice() {
                    [one] => Ok(Value::Text((*one).clone())),
                    [] => Err(wrong()),
                    many => Err(format!(
                        "для «{}» подходит несколько: {}; назови точнее",
                        self.key,
                        joined(many.iter().map(|o| o.as_str()))
                    )),
                }
            }
        }
    }

    /// The risk of going from `old` to `new`.
    pub fn risk_of(&self, old: &Value, new: &Value) -> RiskLevel {
        match self.risk {
            Risk::Same(level) => level,
            Risk::Switch { on, .. } if *new == Value::Bool(true) => on,
            Risk::Switch { off, .. } => off,
            Risk::Limit if raises_limit(old, new) => RiskLevel::Medium,
            Risk::Limit => RiskLevel::Low,
        }
    }

    /// The change turns the setting on.
    pub fn turns_on(&self, old: &Value, new: &Value) -> bool {
        *old == Value::Bool(false) && *new == Value::Bool(true)
    }

    /// The value in words: «вкл», «80%», «без лимита», «автоматически».
    pub fn shown(&self, value: &Value) -> String {
        match (value, self.kind) {
            (Value::Bool(true), _) => "вкл".into(),
            (Value::Bool(false), _) => "выкл".into(),
            (Value::Number(n), _) if *n == 0.0 && self.risk == Risk::Limit => "без лимита".into(),
            (Value::Number(n), Kind::Number { unit, .. }) => format!("{n}{unit}"),
            (Value::Number(n), _) => n.to_string(),
            (Value::Text(t), Kind::Listed) if t.is_empty() => "автоматически".into(),
            (Value::Text(t), Kind::Choice(list)) => match list.iter().find(|(id, _)| id == t) {
                Some((_, name)) => name.to_string(),
                None if t == "custom" => "свой".into(),
                None => t.clone(),
            },
            (Value::Text(t), _) => t.clone(),
        }
    }

    /// What may be set, in words.
    pub fn allowed(&self, options: &[String]) -> String {
        match self.kind {
            Kind::Switch => "вкл или выкл".into(),
            Kind::Number { min, max, unit, .. } => {
                let mut text =
                    format!("от {min} до {max}{unit}; можно сдвиг от текущего: «+N» или «-N»");
                if self.risk == Risk::Limit {
                    text.push_str("; 0 — без лимита");
                }
                text
            }
            Kind::Choice(list) => list
                .iter()
                .map(|(id, name)| format!("{id} ({name})"))
                .collect::<Vec<_>>()
                .join(", "),
            Kind::Listed if options.is_empty() => {
                "выбрать не из чего: приложение не дало списка вариантов".into()
            }
            Kind::Listed => format!("{} или «авто»", joined(options.iter().map(|o| o.as_str()))),
        }
    }

    /// The card of the change: «Включить микрофон», «было: выкл → станет: вкл».
    pub fn assess(&self, old: &Value, new: &Value) -> Assessment {
        let level = self.risk_of(old, new);
        let on = *new == Value::Bool(true);
        let summary = match self.kind {
            Kind::Switch if on => format!("Включить {}", self.name),
            Kind::Switch => format!("Выключить {}", self.name),
            _ => format!("Изменить {}", self.name),
        };
        let target = format!("было: {} → станет: {}", self.shown(old), self.shown(new));
        let reason = match level {
            RiskLevel::High => {
                "открывает то, что она слышит, видит, хранит или отправляет; спрашивается каждый раз"
            }
            RiskLevel::Medium => "заметно меняет её работу, но легко вернуть",
            RiskLevel::Low if matches!(self.risk, Risk::Switch { .. }) => {
                "выключить безопасно, легко вернуть"
            }
            RiskLevel::Low => "легко вернуть как было",
        };
        let scope = match self.risk {
            Risk::Same(_) => self.key.to_string(),
            Risk::Switch { .. } if on => format!("{}:on", self.key),
            Risk::Switch { .. } => format!("{}:off", self.key),
            Risk::Limit if raises_limit(old, new) => format!("{}:up", self.key),
            Risk::Limit => format!("{}:down", self.key),
        };
        Assessment::new(level, summary, target, reason).with_scope(scope)
    }
}

/// A limit grows. 0 is none, so removing a limit raises it, and setting one
/// where there was none lowers it.
fn raises_limit(old: &Value, new: &Value) -> bool {
    let (Value::Number(old), Value::Number(new)) = (old, new) else {
        return true;
    };
    match (*old == 0.0, *new == 0.0) {
        (true, _) => false,
        (false, true) => true,
        (false, false) => new > old,
    }
}

// Where the application applies a change. It is implemented by the app,
/// which calls the same code its settings page calls.
#[async_trait]
pub trait SettingsPort: Send + Sync {
    /// The value in force now.
    fn read(&self, key: &str) -> Result<Value, String>;
    /// The values of a `Listed` setting; empty when there are none to offer.
    fn options(&self, key: &str) -> Vec<String>;
    /// The turn goes on in Telegram.
    fn in_telegram(&self) -> bool;
    /// Applies the change like the settings page: the same checks, the same
    /// side effects, saved to `config.toml`, the windows told.
    async fn apply(&self, key: &str, value: &Value) -> Result<(), String>;
}

/// The port, set by the application once it runs. Empty in the terminal client.
#[derive(Clone, Default)]
pub struct SettingsSlot(Arc<RwLock<Option<Arc<dyn SettingsPort>>>>);

impl SettingsSlot {
    pub fn set(&self, port: Arc<dyn SettingsPort>) {
        *self.0.write() = Some(port);
    }

    pub fn get(&self) -> Option<Arc<dyn SettingsPort>> {
        self.0.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(key: &str) -> &'static Setting {
        find(key).unwrap_or_else(|| panic!("{key} is not in the list"))
    }

    const ON: Value = Value::Bool(true);
    const OFF: Value = Value::Bool(false);

    #[test]
    fn keys_are_unique_and_known() {
        for (i, a) in SETTINGS.iter().enumerate() {
            assert!(
                SETTINGS[i + 1..].iter().all(|b| b.key != a.key),
                "{} twice",
                a.key
            );
            assert!(!a.name.is_empty() && !a.about.is_empty(), "{}", a.key);
        }
        assert!(find("voice.rate").is_some());
        assert!(find(" Voice.Rate ").is_some(), "keys read loosely");
        assert!(find("llm.model").is_none());
        assert!(keys().contains("voice.volume") && keys().contains("privacy.microphone"));
    }

    #[test]
    fn turning_privacy_on_is_high_and_off_is_low() {
        for key in [
            "privacy.microphone",
            "voice.wake",
            "dictation",
            "privacy.screen",
            "screen_ask",
            "log.content",
            "tools.web",
            "telegram",
            "telegram.voice_in",
            "telegram.voice_reply",
            "events",
            "autostart",
            "notifications",
            "tracking.app_time",
            "tracking.clipboard",
        ] {
            let s = s(key);
            assert_eq!(s.risk_of(&OFF, &ON), RiskLevel::High, "{key} on");
            assert_eq!(s.risk_of(&ON, &OFF), RiskLevel::Low, "{key} off");
            assert!(s.turns_on(&OFF, &ON) && !s.turns_on(&ON, &OFF));
        }
    }

    #[test]
    fn comfort_is_low_or_medium_and_a_limit_rises_at_medium() {
        let low = |key: &str, a: Value, b: Value| s(key).risk_of(&a, &b);
        assert_eq!(low("voice.speak_replies", OFF, ON), RiskLevel::Low);
        assert_eq!(
            low("voice.volume", Value::Number(80.0), Value::Number(40.0)),
            RiskLevel::Low
        );
        assert_eq!(
            low(
                "voice.voice",
                Value::Text("".into()),
                Value::Text("Irina".into())
            ),
            RiskLevel::Low
        );
        assert_eq!(low("voice.follow_up", OFF, ON), RiskLevel::Medium);
        // Offers to save a skill only suggest: low both ways (decision 0047).
        assert_eq!(low("skills.suggest", OFF, ON), RiskLevel::Low);
        assert_eq!(low("skills.suggest", ON, OFF), RiskLevel::Low);
        assert_eq!(low("voice.follow_up", ON, OFF), RiskLevel::Low);
        assert_eq!(
            low(
                "i18n.language",
                Value::Text("ru".into()),
                Value::Text("en".into())
            ),
            RiskLevel::Medium
        );
        let limit = s("costs.day_limit");
        let n = Value::Number;
        assert_eq!(limit.risk_of(&n(100.0), &n(50.0)), RiskLevel::Low);
        assert_eq!(limit.risk_of(&n(50.0), &n(100.0)), RiskLevel::Medium);
        // 0 is no limit: removing it raises it, setting one lowers it.
        assert_eq!(limit.risk_of(&n(50.0), &n(0.0)), RiskLevel::Medium);
        assert_eq!(limit.risk_of(&n(0.0), &n(500.0)), RiskLevel::Low);
        // Nothing of the list is high both ways.
        for s in SETTINGS {
            let worst_off = match s.risk {
                Risk::Same(level) => level,
                Risk::Switch { off, .. } => off,
                Risk::Limit => RiskLevel::Low,
            };
            assert!(worst_off < RiskLevel::High, "{}", s.key);
        }
    }

    #[test]
    fn switches_take_words_and_toggle() {
        let s = s("voice.wake");
        for on in [
            json!(true),
            json!("вкл"),
            json!("включи"),
            json!("On"),
            json!("да"),
        ] {
            assert_eq!(s.parse(&on, &OFF, &[]), Ok(ON), "{on}");
        }
        for off in [
            json!(false),
            json!("выкл"),
            json!("выключить"),
            json!("off"),
            json!(0),
        ] {
            assert_eq!(s.parse(&off, &ON, &[]), Ok(OFF), "{off}");
        }
        assert_eq!(s.parse(&json!("toggle"), &ON, &[]), Ok(OFF));
        assert!(s.parse(&json!("может быть"), &ON, &[]).is_err());
        assert!(s.parse(&json!(0.5), &ON, &[]).is_err());
    }

    #[test]
    fn numbers_stay_in_bounds_and_take_a_shift() {
        let rate = s("voice.rate");
        let n = Value::Number;
        assert_eq!(rate.parse(&json!(1.25), &n(1.0), &[]), Ok(n(1.3)));
        assert_eq!(rate.parse(&json!("1,5"), &n(1.0), &[]), Ok(n(1.5)));
        assert_eq!(rate.parse(&json!("+0.2"), &n(1.0), &[]), Ok(n(1.2)));
        assert!(rate.parse(&json!(3), &n(1.0), &[]).is_err());
        assert!(rate.parse(&json!("быстро"), &n(1.0), &[]).is_err());
        let volume = s("voice.volume");
        assert_eq!(volume.parse(&json!("-15"), &n(80.0), &[]), Ok(n(65.0)));
        // A shift past the bound stops at the bound.
        assert_eq!(volume.parse(&json!("-30"), &n(20.0), &[]), Ok(n(0.0)));
        assert_eq!(volume.parse(&json!("+50"), &n(80.0), &[]), Ok(n(100.0)));
        assert!(volume.parse(&json!(-5), &n(80.0), &[]).is_err());
        assert!(volume
            .parse(&json!(f64::NAN.to_string()), &n(80.0), &[])
            .is_err());
        let every = s("rhythm.break_every_min");
        assert!(every.parse(&json!(5), &n(50.0), &[]).is_err());
        assert_eq!(every.parse(&json!(45), &n(50.0), &[]), Ok(n(45.0)));
    }

    #[test]
    fn choices_take_ids_or_names() {
        let persona = s("assistant.persona");
        let t = |v: &str| Value::Text(v.into());
        assert_eq!(
            persona.parse(&json!("calm"), &t("caring"), &[]),
            Ok(t("calm"))
        );
        assert_eq!(
            persona.parse(&json!("Весёлая"), &t("caring"), &[]),
            Ok(t("cheerful"))
        );
        assert!(persona.parse(&json!("custom"), &t("caring"), &[]).is_err());
        let lang = s("i18n.language");
        assert_eq!(lang.parse(&json!("EN"), &t("ru"), &[]), Ok(t("en")));
        assert!(lang.parse(&json!("de"), &t("ru"), &[]).is_err());
    }

    #[test]
    fn a_listed_value_is_one_of_the_options() {
        let voice = s("voice.voice");
        let t = |v: &str| Value::Text(v.into());
        let options = vec![
            "Microsoft Irina Desktop".to_string(),
            "Microsoft Pavel".to_string(),
            "Microsoft Zira".to_string(),
        ];
        assert_eq!(
            voice.parse(&json!("microsoft pavel"), &t(""), &options),
            Ok(t("Microsoft Pavel"))
        );
        assert!(
            voice.parse(&json!("Ирина"), &t(""), &options).is_err(),
            "no guessing across alphabets"
        );
        assert_eq!(
            voice.parse(&json!("irina"), &t(""), &options),
            Ok(t("Microsoft Irina Desktop"))
        );
        // Two voices match: she must ask which one.
        assert!(voice.parse(&json!("microsoft"), &t(""), &options).is_err());
        assert_eq!(
            voice.parse(&json!("авто"), &t("Microsoft Pavel"), &options),
            Ok(t(""))
        );
        assert!(voice.parse(&json!("Kore"), &t(""), &[]).is_err());
    }

    #[test]
    fn the_card_says_what_changes() {
        let mic = s("privacy.microphone").assess(&OFF, &ON);
        assert_eq!(mic.summary, "Включить микрофон");
        assert_eq!(mic.target, "было: выкл → станет: вкл");
        assert_eq!(mic.level, RiskLevel::High);
        assert_eq!(mic.scope, "privacy.microphone:on");
        let off = s("privacy.microphone").assess(&ON, &OFF);
        assert_eq!(off.summary, "Выключить микрофон");
        assert_eq!(off.scope, "privacy.microphone:off");
        assert!(crate::policy::forever_block_for(&mic).is_some());
        assert!(crate::policy::forever_block_for(&off).is_none());

        let rate = s("voice.rate").assess(&Value::Number(1.0), &Value::Number(1.3));
        assert_eq!(rate.summary, "Изменить скорость речи");
        assert_eq!(rate.target, "было: 1 → станет: 1.3");
        let limit = s("costs.day_limit").assess(&Value::Number(0.0), &Value::Number(300.0));
        assert_eq!(limit.target, "было: без лимита → станет: 300");
        assert_eq!(limit.scope, "costs.day_limit:down");
        let voice =
            s("voice.voice").assess(&Value::Text(String::new()), &Value::Text("Pavel".into()));
        assert_eq!(voice.target, "было: автоматически → станет: Pavel");
        let persona = s("assistant.persona")
            .assess(&Value::Text("caring".into()), &Value::Text("bold".into()));
        assert_eq!(persona.target, "было: заботливая → станет: дерзкая");
        assert_eq!(s("voice.volume").shown(&Value::Number(80.0)), "80%");
        assert_eq!(
            s("rhythm.break_every_min").shown(&Value::Number(50.0)),
            "50 мин"
        );
    }

    #[test]
    fn allowed_values_are_told() {
        assert_eq!(s("voice.wake").allowed(&[]), "вкл или выкл");
        assert!(s("voice.rate").allowed(&[]).contains("от 0.5 до 2"));
        assert!(s("costs.day_limit").allowed(&[]).contains("0 — без лимита"));
        assert!(s("assistant.persona")
            .allowed(&[])
            .contains("bold (дерзкая)"));
        let voices = s("voice.voice").allowed(&["Pavel".into(), "Zira".into()]);
        assert!(voices.contains("Pavel") && voices.contains("авто"));
    }

    #[test]
    fn values_are_read_from_the_config_and_only_from_it() {
        let mut c = Config::default();
        c.privacy.microphone_enabled = true;
        c.voice.rate = 1.4;
        c.voice.voice = "Pavel".into();
        c.assistant.persona = Persona::Bold;
        c.costs.month_limit = 1000.0;
        c.rhythm.break_every_min = 40;
        assert_eq!(read(&c, "privacy.microphone"), Some(ON));
        assert_eq!(read(&c, "voice.rate"), Some(Value::Number(1.4)));
        assert_eq!(read(&c, "voice.voice"), Some(Value::Text("Pavel".into())));
        assert_eq!(
            read(&c, "assistant.persona"),
            Some(Value::Text("bold".into()))
        );
        assert_eq!(read(&c, "costs.month_limit"), Some(Value::Number(1000.0)));
        assert_eq!(
            read(&c, "rhythm.break_every_min"),
            Some(Value::Number(40.0))
        );
        for key in OUTSIDE_CONFIG {
            assert!(find(key).is_some() && read(&c, key).is_none(), "{key}");
        }
        // Every other key of the list is read from the config.
        for s in SETTINGS.iter().filter(|s| !OUTSIDE_CONFIG.contains(&s.key)) {
            assert!(read(&c, s.key).is_some(), "{}", s.key);
        }
        assert_eq!(read(&c, "llm.model"), None);
    }

    #[test]
    fn a_provider_voice_is_read_when_the_provider_speaks() {
        let mut c = Config::default();
        c.voice.tts = crate::config::TtsEngine::Provider;
        c.voice.tts_voice = "Kore".into();
        assert_eq!(read(&c, "voice.voice"), Some(Value::Text("Kore".into())));
    }

    #[test]
    fn nothing_secret_is_in_the_list() {
        const SECRET: [&str; 12] = [
            "key",
            "token",
            "url",
            "secret",
            "password",
            "model",
            "folder",
            "path",
            "hotkey",
            "user_id",
            "custom_prompt",
            "backup",
        ];
        for s in SETTINGS {
            for word in SECRET {
                assert!(!s.key.contains(word), "{} looks secret", s.key);
            }
        }
    }

    #[tokio::test]
    async fn the_slot_hands_out_the_port_set_in_it() {
        struct Port;
        #[async_trait]
        impl SettingsPort for Port {
            fn read(&self, _key: &str) -> Result<Value, String> {
                Ok(Value::Bool(true))
            }
            fn options(&self, _key: &str) -> Vec<String> {
                Vec::new()
            }
            fn in_telegram(&self) -> bool {
                false
            }
            async fn apply(&self, _key: &str, _value: &Value) -> Result<(), String> {
                Ok(())
            }
        }
        let slot = SettingsSlot::default();
        assert!(slot.get().is_none());
        slot.clone().set(Arc::new(Port));
        let port = slot.get().expect("set through a clone");
        assert_eq!(port.read("voice.wake"), Ok(Value::Bool(true)));
        assert_eq!(port.apply("voice.wake", &OFF).await, Ok(()));
    }
}
