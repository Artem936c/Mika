//! Interface language and texts. One source with the Svelte UI:
//! `frontend/src/locales/ru.json` and `en.json`.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::config::Config;

pub mod prompt;

const RU: &str = include_str!("../../../frontend/src/locales/ru.json");
const EN: &str = include_str!("../../../frontend/src/locales/en.json");

/// A language of the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    #[default]
    Ru,
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Ru, Lang::En];

    /// The code kept in the settings: `ru` or `en`.
    pub fn code(self) -> &'static str {
        match self {
            Lang::Ru => "ru",
            Lang::En => "en",
        }
    }

    /// The language of a code, case and outer spaces aside. Only `ru` and `en` exist.
    pub fn from_code(code: &str) -> Option<Lang> {
        match code.trim().to_ascii_lowercase().as_str() {
            "ru" => Some(Lang::Ru),
            "en" => Some(Lang::En),
            _ => None,
        }
    }

    /// The language of a code; an unknown code reads as Russian.
    pub fn parse_or_default(code: &str) -> Lang {
        Lang::from_code(code).unwrap_or_default()
    }

    /// The language for a Windows language id such as 0x0419: Russian for a
    /// Russian system and for an unknown one, English for any other.
    pub fn from_windows_langid(id: u16) -> Lang {
        // The low ten bits are the primary language; 0x19 is Russian.
        match id & 0x3ff {
            0 | 0x19 => Lang::Ru,
            _ => Lang::En,
        }
    }

    /// The name of the assistant before the user changes it.
    pub fn default_name(self) -> &'static str {
        match self {
            Lang::Ru => "Мика",
            Lang::En => "Mika",
        }
    }

    fn index(self) -> usize {
        match self {
            Lang::Ru => 0,
            Lang::En => 1,
        }
    }
}

/// The language of this process: 0 is Russian, 1 is English.
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The interface language of the process.
pub fn lang() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::Ru,
    }
}

/// Switches the interface language of the process. Tests never call it: the
/// tests of a crate share one process, and others assert Russian texts.
pub fn set_lang(lang: Lang) {
    CURRENT.store(lang.index() as u8, Ordering::Relaxed);
}

/// The speech language of the settings (`voice.stt_language`): the language the user speaks.
static SPEECH: parking_lot::RwLock<String> = parking_lot::const_rwlock(String::new());

/// Tells the process which language the user speaks. Tests never call it, like `set_lang`.
pub fn set_speech_language(code: &str) {
    *SPEECH.write() = code.to_string();
}

/// The language of the automatic system voice, as the start of a voice
/// language tag such as `ru-RU`.
pub fn voice_language() -> String {
    voice_language_in(lang(), &SPEECH.read())
}

/// The automatic voice is the voice of the language she replies in. With the
/// Russian interface that is Russian. With the English one she replies in the
/// language of the user, so it is the speech language of the settings, and
/// English while that is empty. The interface language alone does not decide:
/// a Russian speaker with English labels still hears a Russian voice.
pub fn voice_language_in(lang: Lang, speech: &str) -> String {
    match lang {
        Lang::Ru => Lang::Ru.code().to_string(),
        Lang::En => {
            // `en-US` and `en_us` are English: the first part of the tag counts.
            let primary = speech
                .trim()
                .split(['-', '_'])
                .next()
                .unwrap_or_default()
                .to_lowercase();
            if primary.is_empty() {
                Lang::En.code().to_string()
            } else {
                primary
            }
        }
    }
}

fn parse(text: &str) -> HashMap<String, String> {
    serde_json::from_str(text).unwrap_or_else(|e| {
        log::error!("locale file is broken: {e}");
        HashMap::new()
    })
}

fn table(lang: Lang) -> &'static HashMap<String, String> {
    static TABLES: OnceLock<[HashMap<String, String>; 2]> = OnceLock::new();
    &TABLES.get_or_init(|| [parse(RU), parse(EN)])[lang.index()]
}

/// The lookup order: the table of the language, then the fallback table.
fn pick<'a>(
    table: &'a HashMap<String, String>,
    fallback: &'a HashMap<String, String>,
    key: &str,
) -> Option<&'a str> {
    table
        .get(key)
        .or_else(|| fallback.get(key))
        .map(String::as_str)
}

/// Text for `key` in the interface language.
pub fn tr(key: &str) -> String {
    tr_in(lang(), key)
}

/// Text for `key` in `lang`. A key that language lacks comes in Russian; an
/// unknown key comes back as is, so a typo is visible on screen.
pub fn tr_in(lang: Lang, key: &str) -> String {
    pick(table(lang), table(Lang::Ru), key)
        .unwrap_or(key)
        .to_string()
}

/// Text for `key` in the interface language with `{name}` placeholders filled.
pub fn tr_args(key: &str, args: &[(&str, &str)]) -> String {
    fill(&tr(key), args)
}

/// Text for `key` in `lang` with `{name}` placeholders filled.
pub fn tr_args_in(lang: Lang, key: &str, args: &[(&str, &str)]) -> String {
    fill(&tr_in(lang, key), args)
}

/// The placeholder name when `after`, the text behind a `{`, starts with `name}`.
fn placeholder(after: &str) -> Option<&str> {
    let len = after
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(after.len());
    (len > 0 && after[len..].starts_with('}')).then(|| &after[..len])
}

/// Puts the values in place of `{name}` placeholders, in one pass like the
/// `t()` of the interface: an inserted value is never scanned again, and a
/// placeholder without a value stays as written.
pub fn fill(text: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = placeholder(after).and_then(|name| {
            args.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, value)| (name.len(), *value))
        });
        match value {
            Some((len, value)) => {
                out.push_str(value);
                rest = &after[len + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Names of the `{name}` placeholders of a text.
pub fn placeholders(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        if let Some(name) = placeholder(rest) {
            names.insert(name.to_string());
        }
    }
    names
}

/// The `[i18n]` section of `config.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct I18nConfig {
    /// Interface language: `ru` or `en`. Anything else reads as `ru`.
    pub language: String,
}

impl Default for I18nConfig {
    fn default() -> Self {
        Self {
            language: Lang::Ru.code().into(),
        }
    }
}

impl I18nConfig {
    pub fn lang(&self) -> Lang {
        Lang::parse_or_default(&self.language)
    }
}

/// Settings whose default depends on the language follow it while they still hold
/// the default of `from`. Returns true when the wake phrase or the speech language changed.
pub fn apply_defaults(config: &mut Config, from: Lang, to: Lang) -> bool {
    if from == to {
        return false;
    }
    let mut changed = false;
    if config.assistant.name == from.default_name() {
        config.assistant.name = to.default_name().into();
    }
    if config.assistant.wake_phrase == from.default_name() {
        config.assistant.wake_phrase = to.default_name().into();
        changed = true;
    }
    if config
        .voice
        .stt_language
        .trim()
        .eq_ignore_ascii_case(from.code())
    {
        config.voice.stt_language = to.code().into();
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cyrillic(text: &str) -> bool {
        text.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
    }

    fn keys(lang: Lang) -> BTreeSet<&'static str> {
        table(lang).keys().map(String::as_str).collect()
    }

    #[test]
    fn locales_have_the_same_keys() {
        let (ru, en) = (keys(Lang::Ru), keys(Lang::En));
        assert!(ru.len() > 500, "ru.json did not load");
        let missing: Vec<_> = ru.difference(&en).collect();
        let extra: Vec<_> = en.difference(&ru).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "missing in en.json: {missing:?}\nnot in ru.json: {extra:?}"
        );
    }

    #[test]
    fn locales_have_the_same_placeholders() {
        for (key, ru) in table(Lang::Ru) {
            let Some(en) = table(Lang::En).get(key) else {
                continue;
            };
            assert_eq!(placeholders(ru), placeholders(en), "{key}");
        }
    }

    #[test]
    fn variant_keys_match() {
        let variants = |lang: Lang| -> BTreeSet<&'static str> {
            table(lang)
                .iter()
                .filter(|(_, text)| text.contains('|'))
                .map(|(key, _)| key.as_str())
                .collect()
        };
        let ru = variants(Lang::Ru);
        assert_eq!(ru, variants(Lang::En));
        assert!(ru.contains("rhythm.break"));
        for key in ru {
            let count = |lang: Lang| table(lang)[key].split('|').count();
            assert_eq!(count(Lang::Ru), count(Lang::En), "{key}");
            for lang in Lang::ALL {
                let text = &table(lang)[key];
                let all = placeholders(text);
                for variant in text.split('|') {
                    assert_eq!(
                        placeholders(variant),
                        all,
                        "{key} ({}): {variant}",
                        lang.code()
                    );
                }
            }
        }
    }

    #[test]
    fn english_locale_has_no_cyrillic() {
        let offenders: Vec<_> = table(Lang::En)
            .iter()
            .filter(|(_, text)| cyrillic(text))
            .map(|(key, _)| key)
            .collect();
        assert!(offenders.is_empty(), "{offenders:?}");
    }

    #[test]
    fn no_message_is_empty() {
        for lang in Lang::ALL {
            for (key, text) in table(lang) {
                assert!(!text.trim().is_empty(), "{key} ({})", lang.code());
            }
        }
    }

    #[test]
    fn text_falls_back_to_russian_then_to_the_key() {
        let table: HashMap<String, String> = [("both".to_string(), "english".to_string())].into();
        let fallback: HashMap<String, String> = [
            ("both".to_string(), "русский".to_string()),
            ("only_ru".to_string(), "только русский".to_string()),
        ]
        .into();
        assert_eq!(pick(&table, &fallback, "both"), Some("english"));
        assert_eq!(pick(&table, &fallback, "only_ru"), Some("только русский"));
        assert_eq!(pick(&table, &fallback, "nowhere"), None);

        assert_eq!(tr_in(Lang::Ru, "tray.quit"), "Выход");
        assert_eq!(tr_in(Lang::En, "tray.quit"), "Quit");
        assert_eq!(tr_in(Lang::En, "no.such.key"), "no.such.key");
        assert_eq!(tr_in(Lang::Ru, "no.such.key"), "no.such.key");
    }

    #[test]
    fn fill_is_one_pass_and_keeps_unknown_placeholders() {
        assert_eq!(
            fill("{a} {b} {c}", &[("a", "{b}"), ("b", "x")]),
            "{b} x {c}"
        );
        assert_eq!(fill("нет подстановок", &[("a", "x")]), "нет подстановок");
        assert_eq!(fill("{a}{a}", &[("a", "я")]), "яя");
        // Not placeholders: an empty name, a name with a space, no closing brace.
        assert_eq!(fill("{} { a } {a {{a}", &[("a", "x")]), "{} { a } {a {x");
        assert_eq!(
            tr_args_in(Lang::En, "chat.setup", &[("name", "Mika")]),
            "For Mika to answer, set a provider and a key."
        );
        assert_eq!(
            tr_args_in(Lang::Ru, "chat.setup", &[("name", "Айри")]),
            "Чтобы Айри отвечала, укажи провайдера и ключ."
        );
        assert_eq!(
            placeholders("{min} и {what_1}, {} { x } {min}"),
            ["min", "what_1"].map(String::from).into()
        );
    }

    #[test]
    fn lang_codes() {
        assert_eq!(Lang::from_code(" EN "), Some(Lang::En));
        assert_eq!(Lang::from_code("ru"), Some(Lang::Ru));
        assert_eq!(Lang::from_code("de"), None);
        assert_eq!(Lang::from_code(""), None);
        assert_eq!(Lang::parse_or_default("de"), Lang::Ru);
        assert_eq!(Lang::parse_or_default("En"), Lang::En);
        for lang in Lang::ALL {
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
        }
        assert_eq!(Lang::default(), Lang::Ru);
        assert_eq!(Lang::Ru.default_name(), "Мика");
        assert_eq!(Lang::En.default_name(), "Mika");
    }

    #[test]
    fn windows_language_ids() {
        assert_eq!(Lang::from_windows_langid(0x0419), Lang::Ru);
        assert_eq!(Lang::from_windows_langid(0x0409), Lang::En);
        assert_eq!(Lang::from_windows_langid(0x0809), Lang::En);
        assert_eq!(Lang::from_windows_langid(0x0422), Lang::En);
        assert_eq!(Lang::from_windows_langid(0), Lang::Ru);
    }

    #[test]
    fn the_automatic_voice_is_of_the_language_she_replies_in() {
        // Russian interface: she speaks Russian whatever the recognition is told.
        for speech in ["ru", "en", "", "de"] {
            assert_eq!(voice_language_in(Lang::Ru, speech), "ru", "{speech}");
        }
        // English interface: the language the user speaks.
        assert_eq!(voice_language_in(Lang::En, "en"), "en");
        // English labels for a Russian speaker do not bring an English voice.
        assert_eq!(voice_language_in(Lang::En, "ru"), "ru");
        assert_eq!(voice_language_in(Lang::En, " RU-ru "), "ru");
        assert_eq!(voice_language_in(Lang::En, "en_US"), "en");
        assert_eq!(voice_language_in(Lang::En, "de"), "de");
        // The provider guesses the speech language: English by default.
        assert_eq!(voice_language_in(Lang::En, ""), "en");
        assert_eq!(voice_language_in(Lang::En, "  "), "en");

        // A first start on an English Windows moves the speech language with
        // the interface; a later switch of the interface does not.
        let mut first = Config::default();
        apply_defaults(&mut first, Lang::Ru, Lang::En);
        assert_eq!(voice_language_in(Lang::En, &first.voice.stt_language), "en");
        let settled = Config::default();
        assert_eq!(
            voice_language_in(Lang::En, &settled.voice.stt_language),
            "ru"
        );
    }

    #[test]
    fn language_setting_loads_and_survives_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(Config::default().i18n.language, "ru");
        assert_eq!(Config::default().i18n.lang(), Lang::Ru);

        // A file written before the setting existed.
        std::fs::write(&path, "[assistant]\nname = \"Айри\"\n").unwrap();
        assert_eq!(Config::load(&path).unwrap().i18n.lang(), Lang::Ru);

        // An unknown language does not make the whole file broken.
        std::fs::write(&path, "[i18n]\nlanguage = \"de\"\n").unwrap();
        let odd = Config::load(&path).unwrap();
        assert_eq!(odd.i18n.language, "de");
        assert_eq!(odd.i18n.lang(), Lang::Ru);

        let mut config = Config::default();
        config.i18n.language = "en".into();
        config.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[i18n]\nlanguage = \"en\""), "{text}");
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded, config);
        assert_eq!(loaded.i18n.lang(), Lang::En);
    }

    #[test]
    fn defaults_follow_the_language_until_the_user_changes_them() {
        let mut config = Config::default();
        assert!(apply_defaults(&mut config, Lang::Ru, Lang::En));
        assert_eq!(config.assistant.name, "Mika");
        assert_eq!(config.assistant.wake_phrase, "Mika");
        assert_eq!(config.voice.stt_language, "en");
        // Nothing but these three changed.
        let mut expected = Config::default();
        expected.assistant.name = "Mika".into();
        expected.assistant.wake_phrase = "Mika".into();
        expected.voice.stt_language = "en".into();
        assert_eq!(config, expected);

        assert!(apply_defaults(&mut config, Lang::En, Lang::Ru));
        assert_eq!(config, Config::default());

        // What the user set is left alone.
        let mut own = Config::default();
        own.assistant.name = "Айри".into();
        own.assistant.wake_phrase = "Айри".into();
        own.voice.stt_language = "de".into();
        let before = own.clone();
        assert!(!apply_defaults(&mut own, Lang::Ru, Lang::En));
        assert_eq!(own, before);

        // Only the name follows: nothing for the wake phrase to restart.
        let mut named = Config::default();
        named.assistant.wake_phrase = "Айри".into();
        named.voice.stt_language = "de".into();
        assert!(!apply_defaults(&mut named, Lang::Ru, Lang::En));
        assert_eq!(named.assistant.name, "Mika");
        assert_eq!(named.assistant.wake_phrase, "Айри");

        // The speech language is compared without case and outer spaces.
        let mut spaced = Config::default();
        spaced.voice.stt_language = " RU ".into();
        assert!(apply_defaults(&mut spaced, Lang::Ru, Lang::En));
        assert_eq!(spaced.voice.stt_language, "en");

        let mut same = Config::default();
        assert!(!apply_defaults(&mut same, Lang::Ru, Lang::Ru));
        assert_eq!(same, Config::default());
        // English defaults do not move when the switch starts from Russian.
        let mut english = expected.clone();
        assert!(!apply_defaults(&mut english, Lang::Ru, Lang::En));
        assert_eq!(english, expected);
    }
}
