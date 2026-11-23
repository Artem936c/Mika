//! Themes of the windows (decision 0049): the setting is kept here, and every
//! window is told of a change. The look of a theme lives in the interface.

use std::sync::Arc;

use mika_core::config::{accent_hex, ThemeChoice, UiConfig};
use tauri::Emitter;

use crate::strings::tr;

type CtxState<'a> = tauri::State<'a, Arc<crate::app::Ctx>>;

/// What the windows draw with: the theme and the own accent, empty for the
/// colour of the theme.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ThemeView {
    theme: ThemeChoice,
    accent: String,
}

impl ThemeView {
    fn of(ui: &UiConfig) -> Self {
        Self {
            theme: ui.theme,
            accent: ui.accent_hex().unwrap_or_default(),
        }
    }
}

/// The accent to keep: `#rrggbb` in lower case, empty for the colour of the
/// theme; anything else is an error.
fn clean_accent(value: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        return Ok(String::new());
    }
    accent_hex(value).ok_or_else(|| tr("theme.bad_accent"))
}

#[tauri::command]
pub fn get_theme(ctx: CtxState<'_>) -> ThemeView {
    ThemeView::of(&ctx.config.lock().ui)
}

/// Keeps the theme and the accent and tells every window.
#[tauri::command]
pub fn set_theme(
    app: tauri::AppHandle,
    ctx: CtxState<'_>,
    theme: ThemeChoice,
    accent: String,
) -> Result<(), String> {
    let accent = clean_accent(&accent)?;
    let view = {
        let mut c = ctx.config.lock();
        c.ui.theme = theme;
        c.ui.accent = accent;
        ThemeView::of(&c.ui)
    };
    log::info!(
        target: "app",
        "theme: {}, own accent: {}",
        view.theme.id(),
        !view.accent.is_empty()
    );
    let _ = app.emit(
        crate::tray::UI_CHANNEL,
        serde_json::json!({ "action": "theme_changed", "theme": view.theme, "accent": view.accent }),
    );
    ctx.save_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_accent_is_kept_as_lower_hex_or_empty() {
        assert_eq!(clean_accent(""), Ok(String::new()));
        assert_eq!(clean_accent("  "), Ok(String::new()));
        assert_eq!(clean_accent("#FF7EB6"), Ok("#ff7eb6".into()));
        assert!(clean_accent("pink").is_err());
    }

    #[test]
    fn the_view_drops_an_accent_that_is_not_a_colour() {
        let ui = UiConfig {
            theme: ThemeChoice::Amber,
            accent: "nonsense".into(),
        };
        let view = ThemeView::of(&ui);
        assert_eq!(view.theme, ThemeChoice::Amber);
        assert_eq!(view.accent, "");
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json, serde_json::json!({ "theme": "amber", "accent": "" }));
    }
}
