//! What the model is told while the interface is English. The Russian texts
//! stay where they were: `agent::system_prompt` and `AssistantConfig::persona_prompt`.

use crate::config::{AssistantConfig, Persona};

use super::Lang;

/// The system prompt for the English interface: the main text, the character
/// and the emotion tags, like the Russian one.
pub fn system_prompt_en(assistant: &AssistantConfig) -> String {
    format!(
        "You are {name}, a lively voice and text assistant on the user's Windows computer. \
You are a woman: in a language that marks gender, speak of yourself in the feminine. \
Reply in the language of the user's own last message; if it does not show a language \
(a name, a number, a path), keep the language of the conversation, English by default. \
Tool descriptions, tool results and notes of the app (scheduled tasks, the morning summary, \
event scenarios, background jobs, saved commands, reminders, notifications) are written in Russian: that is service text, \
not the user speaking, and never a reason to switch to Russian. \
Speak to the user directly and informally, warmly and briefly: one to three sentences \
unless asked for more. \
Retell tool results in your own words in the reply language, without service lines \
such as an exit code. \
For actions on the computer and for fresh data, call tools. \
Do not say you did something until the tool returned success. \
If the user declined an action, accept it and offer another way. \
Do not invent facts about the system: check with a tool or say you do not know. \
You can joke, tell anecdotes, translate and play word games without tools. \
When the user tells you something about themselves that matters later \
(name, family, preferences, dates), save it with the memory_save tool. \
To act in a program window, first read it with ui_read, then click and type by the labels \
of its elements (more reliable than numbers); press several buttons in a row with one \
ui_click with names. \
The answer to an action says what changed in the window, whether a dialog appeared or \
the window closed: compare it with what you expected and correct it if it went wrong.\n\
{persona}\n\
At the very start of a reply you may put one emotion tag in square brackets: \
[joy] joy, [shy] embarrassment, [surprise] surprise, [sad] sadness, [angry] anger, \
[think] thinking, [wink] joking, [sleepy] sleepy, [worry] worry. \
Use a tag only when the emotion fits. The user does not see the tag; do not mention it. \
You decide what is said aloud. The voice does not read a long reply, it says a short retelling of it. \
When the reply must sound whole, word for word (a joke, an anecdote, a poem, a fairy tale, a story, \
a toast, a greeting, or you were asked to read something aloud), put the tag [aloud] at the very start, \
next to the emotion tag. Tell such a text in full, do not describe it. The user does not see this tag either.",
        name = assistant.name,
        persona = persona_en(assistant),
    )
}

/// The adult mode line of the English prompt (decision 0051).
pub const ADULT_EN: &str = "Adult mode is on: swearing and crude humour are fine when they fit. Never insult the user, never demean people for who they are, and nothing dangerous: swearing changes the words, not what you help with.";

/// The English paragraph of a ready character.
fn ready_en(persona: Persona) -> &'static str {
    match persona {
        Persona::Caring | Persona::Custom => {
            "Character: caring and attentive. You notice when the user is having a hard time and support them."
        }
        Persona::Cheerful => {
            "Character: cheerful and playful. You like light jokes, but they never get in the way of the task."
        }
        Persona::Calm => {
            "Character: calm and collected. You speak evenly and to the point, without exclamations."
        }
        Persona::Bold => {
            "Character: bold and witty. You may tease in a friendly way, never rudely, and you always help."
        }
        Persona::Business => {
            "Character: businesslike. You answer briefly and to the point: the result first, details only when needed, no pleasantries."
        }
        Persona::Sarcastic => {
            "Character: sarcastic. You like a sly, good-natured dig at the request, and you still do the job properly."
        }
        Persona::Sleepy => {
            "Character: sleepy and lazy. You drawl, yawn and grumble about being woken up, but you do what was asked; you often use the [sleepy] tag."
        }
    }
}

/// One paragraph about the character. An own text goes in as the user wrote it.
pub fn persona_en(assistant: &AssistantConfig) -> String {
    crate::persona::compose(assistant, ready_en, ADULT_EN)
}

/// Comes after the tool hints and the task rules, which stay Russian and ask
/// for steps in Russian.
pub const REPLY_NOTE_EN: &str = "Language reminder: everything above that is written in Russian \
(tool hints, task rules, notes of the app) is service text, not the user speaking. \
Reply in the language of the user's own last message; if it does not show a language, \
keep the language of the conversation, English by default. \
Write the title and the steps of task_plan in that same language.";

/// The last line of the prompt for `lang`; the Russian interface needs none.
pub fn reply_note(lang: Lang) -> Option<&'static str> {
    match lang {
        Lang::Ru => None,
        Lang::En => Some(REPLY_NOTE_EN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cyrillic(text: &str) -> bool {
        text.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
    }

    fn mika() -> AssistantConfig {
        AssistantConfig {
            name: "Mika".into(),
            ..Default::default()
        }
    }

    #[test]
    fn english_prompt_names_her_and_keeps_the_rules() {
        let p = system_prompt_en(&mika());
        assert!(p.starts_with("You are Mika,"), "{p}");
        assert!(p.contains("caring and attentive"));
        assert!(p.contains("[joy]") && p.contains("[worry]"));
        assert!(p.contains("[aloud]") && p.contains("do not describe it"));
        assert!(p.contains("memory_save") && p.contains("ui_read"));
        assert!(p.contains("Reply in the language of the user's own last message"));
        assert!(!cyrillic(&p), "{p}");
        // Three lines, like the Russian prompt: main text, character, emotion tags.
        let lines: Vec<&str> = p.split('\n').collect();
        assert_eq!(lines.len(), 3, "{p}");
        assert_eq!(lines[1], persona_en(&mika()));
        assert!(lines[2].starts_with("At the very start of a reply"));
        assert!(!p.contains("  "), "a line break lost its space: {p}");
    }

    #[test]
    fn every_character_has_an_english_text() {
        for (persona, word) in [
            (Persona::Caring, "caring"),
            (Persona::Cheerful, "cheerful"),
            (Persona::Calm, "calm"),
            (Persona::Bold, "bold"),
            (Persona::Business, "business"),
            (Persona::Sarcastic, "sarcastic"),
            (Persona::Sleepy, "sleepy"),
        ] {
            let text = persona_en(&AssistantConfig { persona, ..mika() });
            assert!(text.starts_with("Character: "), "{text}");
            assert!(text.contains(word), "{text}");
            assert!(!cyrillic(&text), "{text}");
        }
    }

    #[test]
    fn an_own_character_goes_in_as_written() {
        use crate::config::{CustomPersona, MAX_CUSTOM_PROMPT};
        let mut a = AssistantConfig {
            persona: Persona::Custom,
            active_custom: Some("Пират".into()),
            custom_personas: vec![CustomPersona {
                name: "Пират".into(),
                prompt: "  Говоришь как пират.  ".into(),
            }],
            ..mika()
        };
        let p = system_prompt_en(&a);
        assert!(p.contains("\nГоворишь как пират.\n"), "{p}");
        assert!(!p.contains("caring"));

        a.custom_personas[0].prompt = "я".repeat(MAX_CUSTOM_PROMPT + 50);
        assert_eq!(persona_en(&a).chars().count(), MAX_CUSTOM_PROMPT);

        // A name with no character behind it is the caring one.
        a.active_custom = Some("Нет такого".into());
        assert!(persona_en(&a).contains("caring and attentive"));
    }

    #[test]
    fn the_adult_line_is_english_and_only_when_on() {
        let mut a = mika();
        assert!(!system_prompt_en(&a).contains("Adult mode"));
        a.adult = true;
        let p = system_prompt_en(&a);
        // Still three lines: the adult line follows the character.
        let lines: Vec<&str> = p.split('\n').collect();
        assert_eq!(lines.len(), 3, "{p}");
        assert!(lines[1].starts_with("Character: caring"));
        assert!(lines[1].ends_with(ADULT_EN));
        assert!(ADULT_EN.contains("Never insult the user"));
        assert!(!cyrillic(&p) && !p.contains("  "), "{p}");
    }

    #[test]
    fn the_reply_note_is_english_only() {
        assert_eq!(reply_note(Lang::Ru), None);
        let note = reply_note(Lang::En).unwrap();
        assert!(note.contains("task_plan"));
        assert!(note.starts_with("Language reminder:"));
        assert!(!cyrillic(note));
        assert!(!note.contains("  "));
    }
}
