//! Tray icon and the global shortcut that open the chat.

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::strings::tr;
use crate::window;

pub const UI_CHANNEL: &str = "mika://ui";
/// Tried in order; Ctrl+Alt+Space is often taken (the Claude app uses it).
const SHORTCUTS: &[(&str, bool)] = &[("Ctrl+Alt+M", false), ("Ctrl+Shift+M", true)];

static ACTIVE_SHORTCUT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// The shortcut that was registered, or a note that none was free.
pub fn shortcut_label() -> String {
    ACTIVE_SHORTCUT
        .get()
        .copied()
        .map(str::to_string)
        .unwrap_or_else(|| crate::strings::tr("tray.shortcut_none"))
}

/// Shows the island, gives it focus and tells the page to open the chat.
pub fn open_chat(app: &AppHandle) {
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
            "quit" => app.exit(0),
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
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

pub fn global_shortcut_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_global_shortcut::{Builder, ShortcutState};
    Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
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
