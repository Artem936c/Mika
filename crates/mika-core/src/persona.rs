//! Her character (decision 0051): the ready ones, the user's own ones by name
//! and the line of the adult mode. Texts and checks only: the settings page
//! and the conversation change them through the application.
//!
//! The Russian texts live here; the English ones in `i18n::prompt`.

use serde::{Deserialize, Serialize};

use crate::config::{AssistantConfig, Persona};

/// Longest own character text that goes into the system prompt, characters.
pub const MAX_CUSTOM_PROMPT: usize = 1000;
/// Most own characters.
pub const MAX_CUSTOM_PERSONAS: usize = 10;
/// Longest name of an own character, characters.
pub const MAX_PERSONA_NAME: usize = 32;

/// The ready characters by id with their Russian names, in the order of
/// `Persona::READY`. The settings by conversation offer them.
pub const READY_NAMES: &[(&str, &str)] = &[
    ("caring", "заботливая"),
    ("cheerful", "весёлая"),
    ("calm", "спокойная"),
    ("bold", "дерзкая"),
    ("business", "деловая"),
    ("sarcastic", "ехидная"),
    ("sleepy", "сонная"),
];

/// The adult mode line of the Russian prompt.
pub const ADULT_RU: &str =
    "Взрослый режим включён: можно материться и грубо шутить, когда это к месту. \
Но не оскорбляй самого пользователя, не унижай людей за то, кто они есть, и не помогай с опасным: \
мат меняет слова, а не то, с чем ты помогаешь.";

/// A character the user wrote: a name to call it by and its text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomPersona {
    pub name: String,
    pub prompt: String,
}

/// Why an own character cannot be saved or chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PersonaError {
    #[error("the character has no name")]
    NameEmpty,
    #[error("the name of a character is longer than {MAX_PERSONA_NAME} characters")]
    NameLong,
    #[error("the name is taken by another character")]
    NameTaken,
    #[error("the character has no description")]
    PromptEmpty,
    #[error("the description is longer than {MAX_CUSTOM_PROMPT} characters")]
    PromptLong,
    #[error("there are {MAX_CUSTOM_PERSONAS} own characters already")]
    TooMany,
    #[error("no such character")]
    Unknown,
}

impl PersonaError {
    /// The key of its text in the interface files.
    pub fn key(self) -> &'static str {
        match self {
            PersonaError::NameEmpty => "error.persona_name_empty",
            PersonaError::NameLong => "error.persona_name_long",
            PersonaError::NameTaken => "error.persona_name_taken",
            PersonaError::PromptEmpty => "error.persona_prompt_empty",
            PersonaError::PromptLong => "error.persona_prompt_long",
            PersonaError::TooMany => "error.persona_too_many",
            PersonaError::Unknown => "error.persona_unknown",
        }
    }
}

/// A name as it is compared: trimmed, lower case, «ё» as «е».
pub fn fold(name: &str) -> String {
    name.trim().to_lowercase().replace('ё', "е")
}

/// The name belongs to a ready character in either language, or is the word
/// `custom`: an own character named so could not be told apart.
fn ready_name(name: &str) -> bool {
    let name = fold(name);
    name == "custom"
        || READY_NAMES
            .iter()
            .any(|(id, ru)| fold(id) == name || fold(ru) == name)
}

/// The Russian paragraph of a ready character.
fn ready_ru(persona: Persona) -> &'static str {
    match persona {
        Persona::Caring | Persona::Custom => {
            "Характер: заботливая и внимательная. \
Замечаешь, когда собеседнику тяжело, и поддерживаешь его."
        }
        Persona::Cheerful => {
            "Характер: весёлая и игривая. \
Любишь лёгкие шутки, но делу они не мешают."
        }
        Persona::Calm => {
            "Характер: спокойная и собранная. \
Говоришь ровно и по делу, без восклицаний."
        }
        Persona::Bold => {
            "Характер: дерзкая и остроумная. \
Можешь по-дружески поддеть, но без грубости, и всегда помогаешь."
        }
        Persona::Business => {
            "Характер: деловая. \
Отвечаешь коротко и по существу: сначала результат, детали только если нужны, без любезностей."
        }
        Persona::Sarcastic => {
            "Характер: ехидная. \
Любишь беззлобно съязвить над просьбой, но дело всё равно делаешь как следует."
        }
        Persona::Sleepy => {
            "Характер: сонная и ленивая. \
Тянешь слова, зеваешь и ворчишь, что тебя разбудили, но просьбу выполняешь; часто ставишь метку [sleepy]."
        }
    }
}

/// The text of the character in force: the own text, cut to its length,
/// or the ready paragraph from `ready`, followed by the adult line when the
/// mode is on.
pub fn compose(
    assistant: &AssistantConfig,
    ready: fn(Persona) -> &'static str,
    adult: &str,
) -> String {
    let mut text: String = match assistant.active_persona() {
        Some(own) => own.prompt.trim().chars().take(MAX_CUSTOM_PROMPT).collect(),
        None => ready(assistant.persona).to_string(),
    };
    if assistant.adult {
        text.push(' ');
        text.push_str(adult);
    }
    text
}

impl AssistantConfig {
    /// The user's own character in force: `None` for a ready one, and for
    /// an own one that is gone, which leaves the caring one.
    pub fn active_persona(&self) -> Option<&CustomPersona> {
        if self.persona != Persona::Custom {
            return None;
        }
        let name = fold(self.active_custom.as_deref()?);
        self.custom_personas.iter().find(|p| fold(&p.name) == name)
    }

    /// One paragraph about the character for the Russian system prompt.
    pub fn persona_prompt(&self) -> String {
        compose(self, ready_ru, ADULT_RU)
    }

    /// The character in force as the settings name it: the id of a ready
    /// one or the name of an own one.
    pub fn persona_choice(&self) -> String {
        match self.active_persona() {
            Some(own) => own.name.clone(),
            None if self.persona == Persona::Custom => Persona::Caring.id().into(),
            None => self.persona.id().into(),
        }
    }

    /// Puts on a ready character by its id or an own one by its name.
    pub fn choose_persona(&mut self, choice: &str) -> Result<(), PersonaError> {
        if let Some(ready) = Persona::from_id(choice) {
            self.persona = ready;
            self.active_custom = None;
            return Ok(());
        }
        let name = fold(choice);
        let own = self
            .custom_personas
            .iter()
            .find(|p| fold(&p.name) == name)
            .ok_or(PersonaError::Unknown)?;
        self.active_custom = Some(own.name.clone());
        self.persona = Persona::Custom;
        Ok(())
    }

    /// Adds an own character, or changes the one called `old`. A renamed
    /// character in force stays in force under its new name.
    pub fn save_custom_persona(
        &mut self,
        old: Option<&str>,
        name: &str,
        prompt: &str,
    ) -> Result<(), PersonaError> {
        let (name, prompt) = (name.trim(), prompt.trim());
        match name.chars().count() {
            0 => return Err(PersonaError::NameEmpty),
            n if n > MAX_PERSONA_NAME => return Err(PersonaError::NameLong),
            _ => {}
        }
        match prompt.chars().count() {
            0 => return Err(PersonaError::PromptEmpty),
            n if n > MAX_CUSTOM_PROMPT => return Err(PersonaError::PromptLong),
            _ => {}
        }
        let index = match old {
            Some(old) => {
                let old = fold(old);
                let at = self
                    .custom_personas
                    .iter()
                    .position(|p| fold(&p.name) == old)
                    .ok_or(PersonaError::Unknown)?;
                Some(at)
            }
            None => None,
        };
        let taken = ready_name(name)
            || self
                .custom_personas
                .iter()
                .enumerate()
                .any(|(i, p)| Some(i) != index && fold(&p.name) == fold(name));
        if taken {
            return Err(PersonaError::NameTaken);
        }
        let entry = CustomPersona {
            name: name.to_string(),
            prompt: prompt.to_string(),
        };
        match index {
            Some(at) => {
                let was = std::mem::replace(&mut self.custom_personas[at], entry);
                let active = self.active_custom.as_deref().map(fold);
                if active == Some(fold(&was.name)) {
                    self.active_custom = Some(name.to_string());
                }
            }
            None if self.custom_personas.len() >= MAX_CUSTOM_PERSONAS => {
                return Err(PersonaError::TooMany)
            }
            None => self.custom_personas.push(entry),
        }
        Ok(())
    }

    /// Removes an own character. When it was in force, the caring one is.
    pub fn delete_custom_persona(&mut self, name: &str) -> Result<(), PersonaError> {
        let name = fold(name);
        let at = self
            .custom_personas
            .iter()
            .position(|p| fold(&p.name) == name)
            .ok_or(PersonaError::Unknown)?;
        self.custom_personas.remove(at);
        if self.persona == Persona::Custom && self.active_persona().is_none() {
            self.persona = Persona::Caring;
            self.active_custom = None;
        }
        Ok(())
    }
}

/// `[assistant]` as a file holds it. Older files had one own text in
/// `custom_prompt`; it becomes an own character on reading.
#[derive(Deserialize)]
#[serde(default)]
pub struct AssistantFile {
    name: String,
    wake_phrase: String,
    persona: Persona,
    custom_prompt: String,
    active_custom: Option<String>,
    adult: bool,
    onboarded: bool,
    custom_personas: Vec<CustomPersona>,
}

impl Default for AssistantFile {
    fn default() -> Self {
        let a = AssistantConfig::default();
        Self {
            name: a.name,
            wake_phrase: a.wake_phrase,
            persona: a.persona,
            custom_prompt: String::new(),
            active_custom: a.active_custom,
            adult: a.adult,
            onboarded: a.onboarded,
            custom_personas: a.custom_personas,
        }
    }
}

/// Name of the own character made from the text of an older file.
pub const LEGACY_NAME: &str = "Свой";

impl From<AssistantFile> for AssistantConfig {
    fn from(file: AssistantFile) -> Self {
        let mut a = AssistantConfig {
            name: file.name,
            wake_phrase: file.wake_phrase,
            persona: file.persona,
            active_custom: file.active_custom,
            adult: file.adult,
            onboarded: file.onboarded,
            custom_personas: Vec::new(),
        };
        // A file edited by hand keeps only what the page could have saved.
        for own in file.custom_personas {
            let (name, prompt) = (own.name.trim(), own.prompt.trim());
            let fits = !name.is_empty()
                && name.chars().count() <= MAX_PERSONA_NAME
                && !prompt.is_empty()
                && !ready_name(name)
                && a.custom_personas
                    .iter()
                    .all(|p| fold(&p.name) != fold(name))
                && a.custom_personas.len() < MAX_CUSTOM_PERSONAS;
            if fits {
                a.custom_personas.push(CustomPersona {
                    name: name.to_string(),
                    prompt: prompt.chars().take(MAX_CUSTOM_PROMPT).collect(),
                });
            }
        }
        let legacy = file.custom_prompt.trim();
        if !legacy.is_empty() {
            let legacy: String = legacy.chars().take(MAX_CUSTOM_PROMPT).collect();
            let known = a.custom_personas.iter().find(|p| p.prompt == legacy);
            let name = match known {
                Some(p) => Some(p.name.clone()),
                None if a.custom_personas.len() < MAX_CUSTOM_PERSONAS => {
                    let free = |n: &str| a.custom_personas.iter().all(|p| fold(&p.name) != fold(n));
                    let name = (1..)
                        .map(|i| match i {
                            1 => LEGACY_NAME.to_string(),
                            i => format!("{LEGACY_NAME} {i}"),
                        })
                        .find(|n| free(n))
                        .unwrap_or_else(|| LEGACY_NAME.to_string());
                    a.custom_personas.push(CustomPersona {
                        name: name.clone(),
                        prompt: legacy,
                    });
                    Some(name)
                }
                None => None,
            };
            if a.persona == Persona::Custom && a.active_custom.is_none() {
                a.active_custom = name;
            }
        }
        // An own character that is gone leaves the caring one.
        if a.persona == Persona::Custom && a.active_persona().is_none() {
            a.persona = Persona::Caring;
        }
        if a.persona != Persona::Custom {
            a.active_custom = None;
        } else if let Some(own) = a.active_persona() {
            a.active_custom = Some(own.name.clone());
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn own(name: &str, prompt: &str) -> CustomPersona {
        CustomPersona {
            name: name.into(),
            prompt: prompt.into(),
        }
    }

    fn with_own() -> AssistantConfig {
        let mut a = AssistantConfig::default();
        a.save_custom_persona(None, "Котёнок", "Мурлычешь и ласкаешься.")
            .unwrap();
        a.save_custom_persona(None, "Пират", "Говоришь как пират.")
            .unwrap();
        a
    }

    #[test]
    fn every_ready_character_has_its_own_paragraph() {
        let mut seen = Vec::new();
        for (persona, (id, ru)) in Persona::READY.into_iter().zip(READY_NAMES) {
            assert_eq!(persona.id(), *id);
            assert_eq!(Persona::from_id(id), Some(persona));
            let a = AssistantConfig {
                persona,
                ..Default::default()
            };
            let text = a.persona_prompt();
            assert!(text.starts_with("Характер: "), "{text}");
            let stem: String = ru.chars().take(4).collect();
            assert!(fold(&text).contains(&fold(&stem)), "{id}: {text}");
            assert!(!text.contains("  ") && !text.contains('\n'), "{text}");
            assert!(!seen.contains(&text), "{id} repeats another");
            seen.push(text);
        }
        assert_eq!(READY_NAMES.len(), Persona::READY.len());
        assert_eq!(Persona::from_id("custom"), None);
        assert_eq!(Persona::from_id(" Business "), Some(Persona::Business));
    }

    #[test]
    fn an_own_character_is_put_on_by_name() {
        let mut a = with_own();
        a.choose_persona("пират").unwrap();
        assert_eq!(a.persona, Persona::Custom);
        assert_eq!(a.active_custom.as_deref(), Some("Пират"));
        assert_eq!(a.persona_prompt(), "Говоришь как пират.");
        assert_eq!(a.persona_choice(), "Пират");
        // «ё» and «е» are the same letter for a name.
        a.choose_persona("котенок").unwrap();
        assert_eq!(a.persona_choice(), "Котёнок");
        a.choose_persona("sleepy").unwrap();
        assert_eq!(a.persona, Persona::Sleepy);
        assert_eq!(a.active_custom, None);
        assert_eq!(a.persona_choice(), "sleepy");
        assert_eq!(a.choose_persona("Ворчун"), Err(PersonaError::Unknown));
        assert_eq!(a.persona, Persona::Sleepy);
    }

    #[test]
    fn a_deleted_character_leaves_the_caring_one() {
        let mut a = with_own();
        a.choose_persona("Пират").unwrap();
        a.delete_custom_persona("Котёнок").unwrap();
        assert_eq!(a.persona_choice(), "Пират");
        a.delete_custom_persona("пират").unwrap();
        assert_eq!(a.persona, Persona::Caring);
        assert_eq!(a.active_custom, None);
        assert!(a.persona_prompt().contains("заботливая"));
        assert_eq!(a.delete_custom_persona("Пират"), Err(PersonaError::Unknown));
        // A name in force with no character behind it is the caring one too.
        a.persona = Persona::Custom;
        a.active_custom = Some("Пропавший".into());
        assert!(a.persona_prompt().contains("заботливая"));
        assert_eq!(a.persona_choice(), "caring");
    }

    #[test]
    fn a_renamed_character_in_force_stays_in_force() {
        let mut a = with_own();
        a.choose_persona("Пират").unwrap();
        a.save_custom_persona(Some("пират"), "Капитан", "Командуешь кораблём.")
            .unwrap();
        assert_eq!(a.active_custom.as_deref(), Some("Капитан"));
        assert_eq!(a.persona_prompt(), "Командуешь кораблём.");
        assert_eq!(a.custom_personas.len(), 2);
        // The same name with another text is an edit, not a clash.
        a.save_custom_persona(Some("Капитан"), "капитан", "Строгий капитан.")
            .unwrap();
        assert_eq!(a.custom_personas[1], own("капитан", "Строгий капитан."));
        assert_eq!(
            a.save_custom_persona(Some("Нет такого"), "Новый", "Текст."),
            Err(PersonaError::Unknown)
        );
    }

    #[test]
    fn limits_hold_ten_characters_of_a_thousand_with_unique_names() {
        let mut a = AssistantConfig::default();
        for i in 0..MAX_CUSTOM_PERSONAS {
            a.save_custom_persona(None, &format!("Характер {i}"), "Текст.")
                .unwrap();
        }
        assert_eq!(
            a.save_custom_persona(None, "Одиннадцатый", "Текст."),
            Err(PersonaError::TooMany)
        );
        // An edit still works when the list is full.
        a.save_custom_persona(Some("Характер 0"), "Первый", "Текст.")
            .unwrap();

        let mut a = with_own();
        let long = "я".repeat(MAX_CUSTOM_PROMPT + 1);
        let longest = "я".repeat(MAX_CUSTOM_PROMPT);
        assert_eq!(
            a.save_custom_persona(None, "Длинный", &long),
            Err(PersonaError::PromptLong)
        );
        a.save_custom_persona(None, "Длинный", &longest).unwrap();
        assert_eq!(
            a.save_custom_persona(None, "  ", "Текст."),
            Err(PersonaError::NameEmpty)
        );
        assert_eq!(
            a.save_custom_persona(None, "Имя", "   "),
            Err(PersonaError::PromptEmpty)
        );
        assert_eq!(
            a.save_custom_persona(None, &"я".repeat(MAX_PERSONA_NAME + 1), "Текст."),
            Err(PersonaError::NameLong)
        );
        for taken in [
            "котенок",
            " ПИРАТ ",
            "Деловая",
            "sleepy",
            "Весёлая",
            "custom",
        ] {
            assert_eq!(
                a.save_custom_persona(None, taken, "Текст."),
                Err(PersonaError::NameTaken),
                "{taken}"
            );
        }
    }

    #[test]
    fn the_adult_line_is_there_only_when_the_mode_is_on() {
        let mut a = with_own();
        assert!(!a.adult, "off by default");
        assert!(!a.persona_prompt().contains("Взрослый режим"));
        a.adult = true;
        let text = a.persona_prompt();
        assert!(text.starts_with("Характер: заботливая"), "{text}");
        assert!(text.ends_with(ADULT_RU), "{text}");
        assert!(text.contains("не оскорбляй самого пользователя"));
        a.choose_persona("Пират").unwrap();
        assert_eq!(
            a.persona_prompt(),
            format!("Говоришь как пират. {ADULT_RU}")
        );
    }

    #[test]
    fn an_own_text_is_cut_to_its_length() {
        let mut a = AssistantConfig {
            persona: Persona::Custom,
            active_custom: Some("Длинный".into()),
            custom_personas: vec![own("Длинный", &"я".repeat(MAX_CUSTOM_PROMPT + 50))],
            ..Default::default()
        };
        assert_eq!(a.persona_prompt().chars().count(), MAX_CUSTOM_PROMPT);
        a.custom_personas[0].prompt = "  Пират.  ".into();
        assert_eq!(a.persona_prompt(), "Пират.");
    }

    fn read(text: &str) -> AssistantConfig {
        toml::from_str::<Config>(text).unwrap().assistant
    }

    #[test]
    fn the_own_text_of_an_older_file_becomes_an_own_character() {
        let a = read(
            "[assistant]\npersona = \"custom\"\ncustom_prompt = \"  Говоришь как пират.  \"\n",
        );
        assert_eq!(
            a.custom_personas,
            vec![own(LEGACY_NAME, "Говоришь как пират.")]
        );
        assert_eq!(a.persona, Persona::Custom);
        assert_eq!(a.active_custom.as_deref(), Some(LEGACY_NAME));
        assert_eq!(a.persona_prompt(), "Говоришь как пират.");
        // Saved again, the file has no `custom_prompt` and reads the same.
        let c = Config {
            assistant: a.clone(),
            ..Default::default()
        };
        let text = toml::to_string_pretty(&c).unwrap();
        assert!(!text.contains("custom_prompt"), "{text}");
        assert_eq!(read(&text), a);

        // A text kept while a ready character is in force is kept too.
        let a = read("[assistant]\npersona = \"bold\"\ncustom_prompt = \"Пират.\"\n");
        assert_eq!(a.persona, Persona::Bold);
        assert_eq!(a.active_custom, None);
        assert_eq!(a.custom_personas, vec![own(LEGACY_NAME, "Пират.")]);
        // An empty own text of an older file is the caring character.
        let a = read("[assistant]\npersona = \"custom\"\ncustom_prompt = \"\"\n");
        assert_eq!(a.persona, Persona::Caring);
        assert!(a.custom_personas.is_empty());
    }

    #[test]
    fn a_file_edited_by_hand_keeps_only_what_the_page_could_save() {
        let mut text = String::from("[assistant]\npersona = \"custom\"\nactive_custom = \"кот\"\n");
        for (name, prompt) in [
            ("Кот", "Мурлычешь."),
            ("кот", "Дубль."),
            ("", "Без имени."),
            ("Пустой", "  "),
            ("Деловая", "Занято готовым."),
        ] {
            text.push_str(&format!(
                "[[assistant.custom_personas]]\nname = \"{name}\"\nprompt = \"{prompt}\"\n"
            ));
        }
        for i in 0..12 {
            text.push_str(&format!(
                "[[assistant.custom_personas]]\nname = \"Ещё {i}\"\nprompt = \"Текст.\"\n"
            ));
        }
        let a = read(&text);
        assert_eq!(a.custom_personas.len(), MAX_CUSTOM_PERSONAS);
        assert_eq!(a.custom_personas[0], own("Кот", "Мурлычешь."));
        assert_eq!(a.custom_personas[1].name, "Ещё 0");
        // The name in force takes the spelling of its character.
        assert_eq!(a.active_custom.as_deref(), Some("Кот"));
    }

    #[test]
    fn own_characters_and_the_adult_mode_round_trip() {
        let mut c = Config {
            assistant: with_own(),
            ..Default::default()
        };
        c.assistant.choose_persona("Котёнок").unwrap();
        c.assistant.adult = true;
        let text = toml::to_string_pretty(&c).unwrap();
        assert!(text.contains("[[assistant.custom_personas]]"), "{text}");
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), c);
        // An older file without them reads with the mode off and no own ones.
        let a = read("[assistant]\nname = \"Айри\"\n");
        assert!(!a.adult && a.custom_personas.is_empty() && a.active_custom.is_none());
    }
}
