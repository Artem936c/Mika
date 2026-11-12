//! Tray icon and the global shortcut that open the chat.

use mika_core::AssistantState;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::strings::tr;
use crate::window;

pub const UI_CHANNEL: &str = "mika://ui";
/// Tried in order; Ctrl+Alt+Space is often taken (the Claude app uses it).
const SHORTCUTS: &[(&str, bool)] = &[("Ctrl+Alt+M", false), ("Ctrl+Shift+M", true)];

static ACTIVE_SHORTCUT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
/// Start or finish a voice phrase. Tried in order.
const LISTEN_SHORTCUTS: &[(&str, bool)] = &[("Ctrl+Alt+K", false), ("Ctrl+Shift+K", true)];
static LISTEN_SHORTCUT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// The voice shortcut that was registered, or a note that none was free.
pub fn listen_shortcut_label() -> String {
    LISTEN_SHORTCUT
        .get()
        .copied()
        .map(str::to_string)
        .unwrap_or_else(|| crate::strings::tr("tray.shortcut_none"))
}

/// The shortcut that was registered, or a note that none was free.
pub fn shortcut_label() -> String {
    ACTIVE_SHORTCUT
        .get()
        .copied()
        .map(str::to_string)
        .unwrap_or_else(|| crate::strings::tr("tray.shortcut_none"))
}

/// Shows the island, gives it focus and tells the page to open the chat.
/// It comes to the front even over a full-screen program.
pub fn open_chat(app: &AppHandle) {
    window::summon(app);
    if let Some(w) = app.get_webview_window("island") {
        if !w.is_visible().unwrap_or(true) {
            let _ = w.show();
        }
        let _ = w.set_focus();
    }
    let _ = app.emit(UI_CHANNEL, serde_json::json!({ "action": "open_chat" }));
}

/// `name` is the assistant name shown as the icon tooltip.
pub fn install(app: &AppHandle, name: &str) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", tr("tray.open"), true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", tr("tray.hide"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", tr("tray.quit"), true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &toggle, &quit])?;
    let toggle_item = toggle.clone();

    let mut builder = TrayIconBuilder::with_id("mika")
        .tooltip(name)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => open_chat(app),
            "toggle" => {
                // The desktop avatar follows the island.
                let avatar = app.get_webview_window(window::AVATAR_LABEL);
                if let Some(w) = app.get_webview_window("island") {
                    if w.is_visible().unwrap_or(true) {
                        let _ = w.hide();
                        if let Some(a) = &avatar {
                            let _ = a.hide();
                        }
                        let _ = toggle_item.set_text(tr("tray.show"));
                    } else {
                        let _ = window::show_without_activation(&w);
                        if let Some(a) = &avatar {
                            let _ = window::show_without_activation(a);
                        }
                        let _ = toggle_item.set_text(tr("tray.hide"));
                    }
                }
            }
            "quit" => {
                // The place of the ordinary window is kept in memory while it moves.
                if let Some(ctx) = app.try_state::<std::sync::Arc<crate::app::Ctx>>() {
                    let _ = ctx.save_config();
                }
                app.exit(0)
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                open_chat(tray.app_handle());
            }
        });
    match tauri::image::Image::from_bytes(state_icon(AssistantState::Idle)) {
        Ok(icon) => builder = builder.icon(icon),
        Err(e) => {
            log::warn!("tray icon is broken: {e}");
            if let Some(icon) = app.default_window_icon() {
                builder = builder.icon(icon.clone());
            }
        }
    }
    builder.build(app)?;
    Ok(())
}

/// Her face with a coloured dot for what she is doing; no dot while she waits.
fn state_icon(state: AssistantState) -> &'static [u8] {
    match state {
        AssistantState::Idle => include_bytes!("../icons/tray/idle.png"),
        AssistantState::Listening => include_bytes!("../icons/tray/listening.png"),
        AssistantState::Thinking => include_bytes!("../icons/tray/thinking.png"),
        AssistantState::Speaking => include_bytes!("../icons/tray/speaking.png"),
        AssistantState::AwaitingConfirm => include_bytes!("../icons/tray/awaiting_confirm.png"),
    }
}

/// Tooltip of the tray icon: the name, and the state unless she is idle.
pub fn tooltip(name: &str, state: AssistantState) -> String {
    match state {
        AssistantState::Idle => name.to_string(),
        s => format!(
            "{name} · {}",
            tr(&format!("state.{}", s.as_str())).to_lowercase()
        ),
    }
}

/// Shows the assistant state on the tray icon.
pub fn show_state(app: &AppHandle, name: &str, state: AssistantState) {
    let Some(tray) = app.tray_by_id("mika") else {
        return;
    };
    match tauri::image::Image::from_bytes(state_icon(state)) {
        Ok(icon) => {
            let _ = tray.set_icon(Some(icon));
        }
        Err(e) => log::warn!("tray icon is broken: {e}"),
    }
    let _ = tray.set_tooltip(Some(tooltip(name, state)));
}

pub fn global_shortcut_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_global_shortcut::{Builder, Code, ShortcutState};
    Builder::new()
        .with_handler(|app, shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            if shortcut.key == Code::KeyK {
                window::summon(app);
                let ctx = app
                    .state::<std::sync::Arc<crate::app::Ctx>>()
                    .inner()
                    .clone();
                tauri::async_runtime::spawn(async move { ctx.listen_and_submit().await });
            } else {
                open_chat(app);
            }
        })
        .build()
}

pub fn register_shortcut(app: &AppHandle) {
    use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
    for (label, shift) in SHORTCUTS {
        let mods = if *shift {
            Modifiers::CONTROL | Modifiers::SHIFT
        } else {
            Modifiers::CONTROL | Modifiers::ALT
        };
        match app
            .global_shortcut()
            .register(Shortcut::new(Some(mods), Code::KeyM))
        {
            Ok(()) => {
                let _ = ACTIVE_SHORTCUT.set(label);
                log::info!("shortcut {label} registered");
                return;
            }
            Err(e) => log::warn!("shortcut {label} is taken: {e}"),
        }
    }
}

pub fn register_listen_shortcut(app: &AppHandle) {
    use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
    for (label, shift) in LISTEN_SHORTCUTS {
        let mods = if *shift {
            Modifiers::CONTROL | Modifiers::SHIFT
        } else {
            Modifiers::CONTROL | Modifiers::ALT
        };
        match app
            .global_shortcut()
            .register(Shortcut::new(Some(mods), Code::KeyK))
        {
            Ok(()) => {
                let _ = LISTEN_SHORTCUT.set(label);
                log::info!("voice shortcut {label} registered");
                return;
            }
            Err(e) => log::warn!("voice shortcut {label} is taken: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_icon_is_a_png() {
        for s in [
            AssistantState::Idle,
            AssistantState::Listening,
            AssistantState::Thinking,
            AssistantState::Speaking,
            AssistantState::AwaitingConfirm,
        ] {
            assert!(state_icon(s).starts_with(b"\x89PNG"), "{s:?}");
            assert!(
                tauri::image::Image::from_bytes(state_icon(s)).is_ok(),
                "{s:?}"
            );
        }
    }

    #[test]
    fn tooltip_names_the_state_except_idle() {
        assert_eq!(tooltip("Мика", AssistantState::Idle), "Мика");
        assert_eq!(tooltip("Мика", AssistantState::Thinking), "Мика · думаю");
        assert_eq!(
            tooltip("Мика", AssistantState::AwaitingConfirm),
            "Мика · жду подтверждения"
        );
    }
}
