//! Her character (decision 0051): the page «Персонаж», the quick switch in
//! the chat and the settings by conversation change it here. The checks live
//! in `mika_core::persona`; this keeps the result, puts it into the system
//! prompt and tells every window.

use std::sync::Arc;

use mika_core::config::{AssistantConfig, CustomPersona, Persona};
use mika_core::persona::{PersonaError, MAX_CUSTOM_PERSONAS, MAX_CUSTOM_PROMPT, MAX_PERSONA_NAME};
use tauri::AppHandle;

use crate::strings::tr;

type CtxState<'a> = tauri::State<'a, Arc<crate::app::Ctx>>;

/// What the page and the chat menu draw.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PersonaView {
    /// The character in force: the id of a ready one or the name of an own one.
    choice: String,
    /// Ids of the ready characters, in their order.
    ready: Vec<&'static str>,
    custom: Vec<CustomPersona>,
    adult: bool,
    max_custom: usize,
    max_name: usize,
    max_prompt: usize,
}

impl PersonaView {
    fn of(a: &AssistantConfig) -> Self {
        Self {
            choice: a.persona_choice(),
            ready: Persona::READY.iter().map(|p| p.id()).collect(),
            custom: a.custom_personas.clone(),
            adult: a.adult,
            max_custom: MAX_CUSTOM_PERSONAS,
            max_name: MAX_PERSONA_NAME,
            max_prompt: MAX_CUSTOM_PROMPT,
        }
    }
}

/// Changes the character part of the settings with `edit`, keeps it, puts it
/// into the system prompt and tells every window. Nothing changes when
/// `edit` refuses.
fn change(
    app: &AppHandle,
    ctx: &crate::app::Ctx,
    edit: impl FnOnce(&mut AssistantConfig) -> Result<(), PersonaError>,
) -> Result<PersonaView, String> {
    let view = {
        let mut config = ctx.config.lock();
        let mut assistant = config.assistant.clone();
        edit(&mut assistant).map_err(|e| tr(e.key()))?;
        config.assistant = assistant;
        PersonaView::of(&config.assistant)
    };
    ctx.save_config()?;
    ctx.refresh_prompt();
    crate::bridge::settings_changed(app);
    Ok(view)
}

/// What the journal says of the character in force: the id of a ready one;
/// the name of an own one is the user's text and stays out.
fn shown_choice(view: &PersonaView) -> &str {
    if view.ready.contains(&view.choice.as_str()) {
        &view.choice
    } else {
        "own"
    }
}

#[tauri::command]
pub fn get_personas(ctx: CtxState<'_>) -> PersonaView {
    PersonaView::of(&ctx.config.lock().assistant)
}

/// Puts on a ready character by its id or an own one by its name.
#[tauri::command]
pub fn set_persona(
    app: AppHandle,
    ctx: CtxState<'_>,
    choice: String,
) -> Result<PersonaView, String> {
    let view = change(&app, &ctx, |a| a.choose_persona(&choice))?;
    log::info!(target: "app", "character: {}", shown_choice(&view));
    Ok(view)
}

/// Adds an own character, or changes the one called `old`.
#[tauri::command]
pub fn save_custom_persona(
    app: AppHandle,
    ctx: CtxState<'_>,
    old: Option<String>,
    name: String,
    prompt: String,
) -> Result<PersonaView, String> {
    let view = change(&app, &ctx, |a| {
        a.save_custom_persona(old.as_deref(), &name, &prompt)
    })?;
    log::info!(
        target: "app",
        "own character saved: new={} count={}",
        old.is_none(),
        view.custom.len()
    );
    Ok(view)
}

/// Removes an own character; the caring one takes the place of one in force.
#[tauri::command]
pub fn delete_custom_persona(
    app: AppHandle,
    ctx: CtxState<'_>,
    name: String,
) -> Result<PersonaView, String> {
    let view = change(&app, &ctx, |a| a.delete_custom_persona(&name))?;
    log::info!(
        target: "app",
        "own character deleted: count={} character={}",
        view.custom.len(),
        shown_choice(&view)
    );
    Ok(view)
}

/// Turns adult mode on or off. The page asks «Тебе есть 18?» before it turns
/// it on; the settings by conversation ask through the policy and never from
/// Telegram.
#[tauri::command]
pub fn set_adult(app: AppHandle, ctx: CtxState<'_>, on: bool) -> Result<PersonaView, String> {
    let view = change(&app, &ctx, |a| {
        a.adult = on;
        Ok(())
    })?;
    log::info!(target: "app", "adult mode: {on}");
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::i18n::{tr_in, Lang};

    #[test]
    fn the_view_lists_the_ready_ones_and_the_own_ones() {
        let mut a = AssistantConfig::default();
        a.save_custom_persona(None, "Котёнок", "Мурлычешь.")
            .unwrap();
        let view = PersonaView::of(&a);
        assert_eq!(view.choice, "caring");
        assert_eq!(
            view.ready,
            vec![
                "caring",
                "cheerful",
                "calm",
                "bold",
                "business",
                "sarcastic",
                "sleepy"
            ]
        );
        assert_eq!(view.custom.len(), 1);
        assert!(!view.adult);
        assert_eq!((view.max_custom, view.max_prompt), (10, 1000));
        assert_eq!(shown_choice(&view), "caring");
        a.choose_persona("Котёнок").unwrap();
        let view = PersonaView::of(&a);
        assert_eq!(view.choice, "Котёнок");
        assert_eq!(
            shown_choice(&view),
            "own",
            "the name stays out of the journal"
        );
    }

    #[test]
    fn every_refusal_has_a_text_in_both_languages() {
        for e in [
            PersonaError::NameEmpty,
            PersonaError::NameLong,
            PersonaError::NameTaken,
            PersonaError::PromptEmpty,
            PersonaError::PromptLong,
            PersonaError::TooMany,
            PersonaError::Unknown,
        ] {
            for lang in [Lang::Ru, Lang::En] {
                assert_ne!(tr_in(lang, e.key()), e.key(), "{e:?} {lang:?}");
            }
        }
    }
}
