//! Tray icon and the global shortcut that open the chat.

use mika_core::AssistantState;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

use crate::strings::tr;
use crate::window;

pub const UI_CHANNEL: &str = "mika://ui";

/// The chat hotkey that was registered, or a note that none was free.
pub fn shortcut_label() -> String {
    crate::hotkeys::label("chat")
}

/// The voice hotkey that was registered, or a note that none was free.
pub fn listen_shortcut_label() -> String {
    crate::hotkeys::label("listen")
}

/// The selection hotkey that was registered, or a note that none was free.
pub fn selection_shortcut_label() -> String {
    crate::hotkeys::label("selection")
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
    // Greyed out until a screen recording runs: `set_recording` turns it on.
    let rec_stop = MenuItem::with_id(app, "rec_stop", tr("tray.rec_stop"), false, None::<&str>)?;
    let _ = REC_STOP.set(rec_stop.clone());
    let menu = Menu::with_items(app, &[&open, &toggle, &rec_stop, &quit])?;
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
                    // A hidden window draws no avatar model.
                    let shown = w.is_visible().unwrap_or(true);
                    for label in ["island", window::AVATAR_LABEL] {
                        crate::avatar_model::window_shown(app, label, shown);
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
            "rec_stop" => stop_recording(app),
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
    app.manage(TrayTexts {
        open,
        toggle,
        quit,
        rec_stop,
    });
    Ok(())
}

/// The menu items, kept so their texts can follow the interface language.
struct TrayTexts {
    open: MenuItem<tauri::Wry>,
    toggle: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
    rec_stop: MenuItem<tauri::Wry>,
}

/// Writes the menu and the tooltip again in the current language.
pub fn retranslate(app: &AppHandle, name: &str, state: AssistantState) {
    if let Some(texts) = app.try_state::<TrayTexts>() {
        let _ = texts.open.set_text(tr("tray.open"));
        let _ = texts.quit.set_text(tr("tray.quit"));
        let _ = texts.rec_stop.set_text(tr("tray.rec_stop"));
        // A missing island window counts as shown: the menu starts with that text.
        let shown = app
            .get_webview_window("island")
            .is_none_or(|w| w.is_visible().unwrap_or(true));
        let _ = texts
            .toggle
            .set_text(tr(if shown { "tray.hide" } else { "tray.show" }));
    }
    show_state(app, name, state);
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

/// Tooltip of the tray icon; while the screen is being recorded it says so after the state.
pub fn tooltip_with(name: &str, state: AssistantState, recording: bool) -> String {
    let base = tooltip(name, state);
    if recording {
        format!("{base} · {}", tr("tray.recording"))
    } else {
        base
    }
}

/// A screen recording runs: the icon carries a red dot and the tooltip says so.
static RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The menu item that stops the recording; enabled only while one runs.
static REC_STOP: std::sync::OnceLock<MenuItem<tauri::Wry>> = std::sync::OnceLock::new();
/// The app is closing and the main thread is busy saving a recording: the tray is left as it is.
static CLOSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called once the app is on its way out, before the main thread waits for
/// the background jobs. Every change of the icon or the menu is done by the
/// main thread, and the caller waits for it: a job that ends during that wait
/// would hold a thread of the async runtime until the process is gone.
pub fn closing() {
    CLOSING.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Colour of the recording dot.
const DOT: [f32; 3] = [255.0, 59.0, 48.0];

/// Paints the recording dot over an RGBA picture: a filled red circle of radius
/// `width / 5` in the top right corner, with an edge one pixel soft. Nothing
/// outside the square around the circle changes.
fn with_dot(rgba: &mut [u8], width: u32, height: u32) {
    let radius = width / 5;
    if radius == 0 || rgba.len() < width as usize * height as usize * 4 {
        return;
    }
    let side = 2 * radius;
    let r = radius as f32;
    let (cx, cy) = (width as f32 - r, r);
    for y in 0..side.min(height) {
        for x in width - side..width {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let cover = (r + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            if cover <= 0.0 {
                continue;
            }
            let at = (y as usize * width as usize + x as usize) * 4;
            // The dot over what was there, both with straight alpha.
            let under = f32::from(rgba[at + 3]) / 255.0 * (1.0 - cover);
            let alpha = cover + under;
            for (c, dot) in DOT.iter().enumerate() {
                let mixed = (dot * cover + f32::from(rgba[at + c]) * under) / alpha;
                rgba[at + c] = mixed.round() as u8;
            }
            rgba[at + 3] = (alpha * 255.0).round() as u8;
        }
    }
}

/// The icon for a state, with the red dot while the screen is being recorded.
fn state_image(
    state: AssistantState,
    recording: bool,
) -> tauri::Result<tauri::image::Image<'static>> {
    let icon = tauri::image::Image::from_bytes(state_icon(state))?;
    if !recording {
        return Ok(icon);
    }
    let (width, height) = (icon.width(), icon.height());
    let mut rgba = icon.rgba().to_vec();
    with_dot(&mut rgba, width, height);
    Ok(tauri::image::Image::new_owned(rgba, width, height))
}

/// A screen recording started or ended: the dot on the icon, the words in the
/// tooltip and the menu item «stop recording» follow it.
pub fn set_recording(app: &AppHandle, name: &str, state: AssistantState, on: bool) {
    use std::sync::atomic::Ordering;
    // A running recording reports every second: only a change is drawn.
    if RECORDING.swap(on, Ordering::Relaxed) == on || CLOSING.load(Ordering::Relaxed) {
        return;
    }
    if let Some(item) = REC_STOP.get() {
        let _ = item.set_enabled(on);
    }
    show_state(app, name, state);
}

/// The menu item «stop recording»: the recording is saved, as with the button
/// on its card in the chat.
fn stop_recording(app: &AppHandle) {
    let Some(ctx) = app.try_state::<std::sync::Arc<crate::app::Ctx>>() else {
        return;
    };
    let Some(job) = crate::bgjobs::recording(&ctx.jobs) else {
        return;
    };
    if let Err(e) = ctx.jobs.stop(job.id) {
        log::warn!(target: "jobs", "the recording was not stopped from the tray: id={} error={e}", job.id);
    }
}

/// Shows the assistant state on the tray icon.
pub fn show_state(app: &AppHandle, name: &str, state: AssistantState) {
    use std::sync::atomic::Ordering;
    if CLOSING.load(Ordering::Relaxed) {
        return;
    }
    let Some(tray) = app.tray_by_id("mika") else {
        return;
    };
    let recording = RECORDING.load(Ordering::Relaxed);
    match state_image(state, recording) {
        Ok(icon) => {
            let _ = tray.set_icon(Some(icon));
        }
        Err(e) => log::warn!("tray icon is broken: {e}"),
    }
    let _ = tray.set_tooltip(Some(tooltip_with(name, state, recording)));
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

    #[test]
    fn tooltip_with_names_the_recording_after_the_state() {
        assert_eq!(tooltip_with("Мика", AssistantState::Idle, false), "Мика");
        assert_eq!(
            tooltip_with("Мика", AssistantState::Idle, true),
            "Мика · идёт запись экрана"
        );
        assert_eq!(
            tooltip_with("Мика", AssistantState::Thinking, true),
            "Мика · думаю · идёт запись экрана"
        );
        assert_eq!(
            tooltip_with("Мика", AssistantState::Thinking, false),
            tooltip("Мика", AssistantState::Thinking)
        );
    }

    /// Pixels of a picture, each `[r, g, b, a]`.
    fn pixels(rgba: &[u8]) -> Vec<[u8; 4]> {
        rgba.as_chunks::<4>().0.to_vec()
    }

    #[test]
    fn with_dot_paints_only_the_top_right_corner() {
        const BACK: [u8; 4] = [10, 20, 30, 255];
        let (w, h) = (32u32, 32u32);
        let mut rgba: Vec<u8> = BACK
            .iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect();
        with_dot(&mut rgba, w, h);
        let px = pixels(&rgba);
        // Radius 32 / 5 = 6: the circle lives in the square x 20..32, y 0..12.
        let mut painted = 0;
        for y in 0..h {
            for x in 0..w {
                let p = px[(y * w + x) as usize];
                if x >= 20 && y < 12 {
                    painted += usize::from(p != BACK);
                } else {
                    assert_eq!(p, BACK, "pixel {x},{y} outside the corner changed");
                }
                assert_eq!(p[3], 255, "an opaque picture stays opaque at {x},{y}");
            }
        }
        // The centre is the pure colour, the corners of the square are untouched.
        assert_eq!(px[(6 * w + 26) as usize], [255, 59, 48, 255]);
        assert_eq!(px[31], BACK);
        assert_eq!(px[20], BACK);
        // About the area of a circle of radius 6.
        assert!((100..=140).contains(&painted), "{painted} pixels painted");
    }

    #[test]
    fn with_dot_over_a_clear_picture_keeps_the_rest_clear() {
        let (w, h) = (16u32, 16u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        with_dot(&mut rgba, w, h);
        let px = pixels(&rgba);
        // Radius 3, centre at 13,3.
        assert_eq!(px[(3 * w + 13) as usize], [255, 59, 48, 255]);
        assert_eq!(px[(10 * w + 5) as usize], [0, 0, 0, 0]);
        // A pixel on the soft edge is the same red, only fainter.
        let edge = px[(w + 10) as usize];
        assert_eq!([edge[0], edge[1], edge[2]], [255, 59, 48]);
        assert!(edge[3] > 0 && edge[3] < 255, "{edge:?}");
    }

    #[test]
    fn with_dot_leaves_a_tiny_or_short_buffer_alone() {
        let mut tiny = vec![7u8; 4 * 4 * 4];
        with_dot(&mut tiny, 4, 4);
        assert!(tiny.iter().all(|b| *b == 7), "radius 0 paints nothing");
        let mut short = vec![7u8; 10];
        with_dot(&mut short, 32, 32);
        assert!(short.iter().all(|b| *b == 7));
    }

    #[test]
    fn every_state_icon_takes_the_recording_dot() {
        for s in [
            AssistantState::Idle,
            AssistantState::Listening,
            AssistantState::Thinking,
            AssistantState::Speaking,
            AssistantState::AwaitingConfirm,
        ] {
            let plain = state_image(s, false).unwrap();
            let dotted = state_image(s, true).unwrap();
            assert_eq!(
                (dotted.width(), dotted.height()),
                (plain.width(), plain.height()),
                "{s:?}"
            );
            assert_eq!(dotted.rgba().len(), plain.rgba().len(), "{s:?}");
            assert_ne!(dotted.rgba(), plain.rgba(), "{s:?}");
            // The dot sits where the state mark is not: the bottom half is the same picture.
            let half = plain.rgba().len() / 2;
            assert_eq!(dotted.rgba()[half..], plain.rgba()[half..], "{s:?}");
        }
    }
}
