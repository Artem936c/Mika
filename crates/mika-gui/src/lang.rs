//! Interface language: the first start, the settings command and the prompts of the app that follow it.

use std::path::Path;
use std::sync::Arc;

use mika_core::config::Config;
use mika_core::i18n::{self, Lang};
use mika_core::paths::config_path;
use tauri::{Emitter, Manager};

/// How a long reply is retold for the voice while the interface is English.
const RETELL_PROMPT_EN: &str = "Retell the assistant's reply for a voice to speak: one or two short conversational sentences, at most 200 characters, in the first person, as a woman, in the same language as the reply. Keep the point and the outcome. No lists, links, file paths, code or emoji. Add nothing new. Answer with the retelling only.";
/// The same for a reply with voice marks (decision 0026): the tone is kept with the same marks.
const RETELL_PROMPT_EXPRESSIVE_EN: &str = "Retell the assistant's reply for a voice to speak: one or two short conversational sentences, at most 200 characters, in the first person, as a woman, in the same language as the reply. Keep the point and the outcome. No lists, links, file paths, code or emoji. Add nothing new. The reply is marked up for the voice: remarks in square brackets, such as [softly] or [sigh], and paired tags <whisper>…</whisper>, <emphasis>…</emphasis>, <soft>…</soft>, <fast>…</fast>, <slow>…</slow>, <stress>…</stress>, <forceful>…</forceful>. Keep the tone of the reply with the same marks, sparingly: one or two marks per retelling; they do not count towards the 200 characters. There are no other tags. Do not write remarks in round brackets: the voice reads them aloud. Answer with the retelling only.";

/// The language for a first start: Russian for a Russian Windows, English for any other.
pub fn detect() -> Lang {
    Lang::from_windows_langid(mika_tools::win::locale::ui_language())
}

/// Settings of a first start in `to`: the language, and the name, the wake
/// name and the speech language that go with it.
fn first_start(config: &mut Config, to: Lang) {
    i18n::apply_defaults(config, Lang::Ru, to);
    config.i18n.language = to.code().into();
}

/// Whether this start is the first one of a profile: neither the settings nor
/// the database exist yet. Settings that were deleted or lost beside a
/// database with history belong to an existing user, whose language, name and
/// speech language are not guessed anew from Windows.
pub fn is_first_start(config: &Path, database: &Path) -> bool {
    !config.exists() && !database.exists()
}

/// Sets the language of the process from the settings. `fresh` = the first
/// start of a profile (`is_first_start`).
pub fn start(config: &mut Config, fresh: bool) {
    if fresh {
        first_start(config, detect());
        // Saved at once, so the next start is not a first one.
        if let Err(e) = config.save(&config_path()) {
            log::warn!("cannot save settings: {e}");
        }
    }
    i18n::set_lang(config.i18n.lang());
    // The automatic voice is picked by it, right after this.
    i18n::set_speech_language(&config.voice.stt_language);
    log::info!("interface language: {}", i18n::lang().code());
}

/// Picks the Windows voice of the settings again. The automatic one is the
/// voice of the language she replies in, which the speech language of the
/// settings decides while the interface is English (decision 0035).
pub fn pick_voice(ctx: &crate::app::Ctx) {
    let (speech, voice, rate) = {
        let c = ctx.config.lock();
        (
            c.voice.stt_language.clone(),
            c.voice.voice.clone(),
            c.voice.rate,
        )
    };
    i18n::set_speech_language(&speech);
    ctx.voice.configure(&voice, rate);
}

#[tauri::command]
pub fn get_language() -> &'static str {
    i18n::lang().code()
}

/// Switches the interface language without a restart. Until the first-run
/// screen is passed, the name, the wake name and the speech language follow it
/// while they still hold their defaults.
///
/// Async: it changes tray menu texts, and window and menu work must not run
/// inside a synchronous command on Windows.
#[tauri::command]
pub async fn set_language(
    app: tauri::AppHandle,
    ctx: tauri::State<'_, Arc<crate::app::Ctx>>,
    value: String,
) -> Result<(), String> {
    let new = Lang::from_code(&value).ok_or_else(|| crate::strings::tr("lang.error"))?;
    let old = i18n::lang();
    if new == old {
        return Ok(());
    }
    let (rewake, name) = {
        let mut cfg = ctx.config.lock();
        cfg.i18n.language = new.code().into();
        let rewake = !cfg.assistant.onboarded && i18n::apply_defaults(&mut cfg, old, new);
        (rewake, cfg.assistant.name.clone())
    };
    i18n::set_lang(new);
    log::info!("interface language: {}", new.code());
    // The system prompt takes the base of the new language.
    ctx.refresh_prompt();
    // The automatic Windows voice is picked again: it follows the language she
    // replies in. After the first run the speech language stays as it was, so a
    // Russian speaker who wants English labels keeps the Russian voice.
    pick_voice(&ctx);
    if rewake {
        ctx.inner().restart_wake();
    }
    crate::tray::retranslate(&app, &name, app.state::<mika_core::StateHandle>().get());
    let _ = app.emit(
        crate::tray::UI_CHANNEL,
        serde_json::json!({ "action": "language_changed", "lang": new.code() }),
    );
    // The name may have changed with the language.
    crate::bridge::settings_changed(&app);
    // Last: the switch itself is already applied for this run.
    ctx.save_config()
}

/// The retelling prompt of the interface language; `None` keeps the Russian one of the caller.
pub fn retell_prompt(expressive: bool) -> Option<&'static str> {
    retell_prompt_in(i18n::lang(), expressive)
}

pub fn retell_prompt_in(lang: Lang, expressive: bool) -> Option<&'static str> {
    match lang {
        Lang::Ru => None,
        Lang::En if expressive => Some(RETELL_PROMPT_EXPRESSIVE_EN),
        Lang::En => Some(RETELL_PROMPT_EN),
    }
}

/// What to do with the selected text, in the interface language; `None` keeps
/// the Russian table of the caller. The flag says whether the answer replaces the selection.
pub fn selection_instruction(action: &str) -> Option<(&'static str, bool)> {
    selection_instruction_in(i18n::lang(), action)
}

pub fn selection_instruction_in(lang: Lang, action: &str) -> Option<(&'static str, bool)> {
    if lang != Lang::En {
        return None;
    }
    Some(match action {
        "translate" => (
            "Translate the text below: English into Russian, any other language into English. Answer with the translation only, without comments or quotation marks.",
            true,
        ),
        "fix" => (
            "Fix the spelling, punctuation and grammar of the text below; do not change its meaning, style or language. Answer with the corrected text only, without comments or quotation marks.",
            true,
        ),
        "rephrase" => (
            "Rewrite the text below so that it is clearer and more polite; keep its meaning and its language. Answer with the new text only, without comments or quotation marks.",
            true,
        ),
        "shorter" => (
            "Make the text below half as long and keep the main points and its language. Answer with the new text only, without comments or quotation marks.",
            true,
        ),
        "explain" => ("Explain in simple words what the text below means.", false),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cyrillic(text: &str) -> bool {
        text.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c))
    }

    #[test]
    fn a_first_start_takes_the_language_and_its_defaults() {
        let mut english = Config::default();
        first_start(&mut english, Lang::En);
        assert_eq!(english.i18n.language, "en");
        assert_eq!(english.assistant.name, "Mika");
        assert_eq!(english.assistant.wake_phrase, "Mika");
        assert_eq!(english.voice.stt_language, "en");
        // Nothing that listens or looks is turned on by the language.
        assert!(!english.privacy.microphone_enabled && !english.privacy.screen_enabled);
        assert!(!english.voice.wake_enabled && !english.assistant.onboarded);

        let mut russian = Config::default();
        first_start(&mut russian, Lang::Ru);
        assert_eq!(russian, Config::default());
    }

    #[test]
    fn a_profile_with_history_is_not_a_first_start() {
        let dir = tempfile::tempdir().unwrap();
        let (config, database) = (dir.path().join("config.toml"), dir.path().join("mika.db"));
        assert!(is_first_start(&config, &database));
        // The settings are gone, the history is there: an existing user.
        std::fs::write(&database, b"history").unwrap();
        assert!(!is_first_start(&config, &database));
        std::fs::write(&config, b"").unwrap();
        assert!(!is_first_start(&config, &database));
        std::fs::remove_file(&database).unwrap();
        assert!(!is_first_start(&config, &database));
    }

    #[test]
    fn the_automatic_voice_survives_a_switch_of_the_labels() {
        // A settled Russian speaker: the switch to English leaves the speech
        // language alone, and with it the voice.
        let settled = Config::default();
        assert_eq!(
            i18n::voice_language_in(Lang::En, &settled.voice.stt_language),
            "ru"
        );
        // A first start on an English Windows: the voice is English.
        let mut english = Config::default();
        first_start(&mut english, Lang::En);
        assert_eq!(
            i18n::voice_language_in(Lang::En, &english.voice.stt_language),
            "en"
        );
        // And Russian again if the first-run screen is switched to Russian.
        i18n::apply_defaults(&mut english, Lang::En, Lang::Ru);
        assert_eq!(
            i18n::voice_language_in(Lang::Ru, &english.voice.stt_language),
            "ru"
        );
    }

    #[test]
    fn the_retelling_prompt_is_english_only_for_the_english_interface() {
        assert_eq!(retell_prompt_in(Lang::Ru, false), None);
        assert_eq!(retell_prompt_in(Lang::Ru, true), None);
        let plain = retell_prompt_in(Lang::En, false).unwrap();
        let expressive = retell_prompt_in(Lang::En, true).unwrap();
        assert!(!cyrillic(plain) && !cyrillic(expressive));
        assert!(plain.contains("in the same language as the reply"));
        // The same rules, and the seven effects named like in the Russian prompt.
        let end = "Answer with the retelling only.";
        let rules = plain.strip_suffix(end).unwrap();
        assert!(expressive.starts_with(rules));
        assert!(expressive.ends_with(end));
        for tag in &mika_core::marks::EFFECT_TAGS[..7] {
            assert!(expressive.contains(&format!("<{tag}>…</{tag}>")), "{tag}");
        }
        assert!(!plain.contains("<whisper>"));
    }

    #[test]
    fn selection_actions_have_english_requests_with_the_same_effect() {
        for action in ["translate", "fix", "rephrase", "shorter", "explain"] {
            let (text, replaces) = selection_instruction_in(Lang::En, action).unwrap();
            assert!(!cyrillic(text), "{action}");
            // The tests run in Russian, so this is the Russian table.
            let (russian, same) = crate::selection::instruction(action).unwrap();
            assert!(cyrillic(russian), "{action}");
            assert_eq!(replaces, same, "{action}");
            assert_eq!(selection_instruction_in(Lang::Ru, action), None);
        }
        assert_eq!(selection_instruction_in(Lang::En, "delete"), None);
        assert_eq!(selection_instruction_in(Lang::Ru, "delete"), None);
    }
}
