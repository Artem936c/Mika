//! Two things the user does with a hotkey, without the model choosing a tool:
//! dictation into the window in front and a question about a picture of it.
//! The work and its guards are in `Ctx` (app.rs); here are the key handlers
//! and the commands of the chat card and the settings page (decision 0030).

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::app::{Ctx, ScreenAskView};
use crate::hotkeys::{self, HotkeysView};
use crate::strings::tr;
use crate::window;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// The dictation key went down or up. The island comes above a full-screen
/// program so that the recording is seen, but the keyboard stays where the
/// user is typing: the chat is not opened and no window of ours takes the focus.
pub fn on_dictation(app: &AppHandle, ctx: &Arc<Ctx>, pressed: bool) {
    if pressed && !window::WINDOWED.load(Ordering::Relaxed) {
        window::summon(app);
    }
    ctx.dictation_key(pressed);
}

/// What the page is told about a picture that waits for a question.
fn screen_ask_payload(view: &ScreenAskView) -> serde_json::Value {
    serde_json::json!({
        "action": "screen_ask",
        "app": view.app,
        "title": view.title,
        "width": view.width,
        "height": view.height,
    })
}

/// The screen question key: the picture is taken first, while the window the
/// user means is still in front; only then the chat opens with the card. A
/// refusal is a line of hers in the island, and the user's window keeps the focus.
pub fn on_screen_ask(app: &AppHandle, ctx: Arc<Ctx>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match ctx.screen_ask_capture().await {
            Ok(view) => {
                crate::tray::open_chat(&app);
                let _ = app.emit(crate::tray::UI_CHANNEL, screen_ask_payload(&view));
            }
            Err(message) => {
                if !window::WINDOWED.load(Ordering::Relaxed) {
                    window::summon(&app);
                }
                ctx.remark(&message, "screen_ask", false);
            }
        }
    });
}

/// The switch of dictation. The master switch of the microphone is not touched:
/// the page says when it is off.
#[tauri::command]
pub fn dictation_set_enabled(
    app: AppHandle,
    ctx: CtxState<'_>,
    value: bool,
) -> Result<HotkeysView, String> {
    ctx.config.lock().dictation.enabled = value;
    let saved = ctx.save_config();
    // The hotkey follows the switch even if the file could not be written.
    hotkeys::apply(&app, &ctx, "dictation");
    if !value {
        ctx.cancel_listening();
    }
    saved?;
    Ok(hotkeys::view(&ctx))
}

/// The switch of the question about the screen. The master switch of the
/// screen is not touched: the page says when it is off.
#[tauri::command]
pub fn screen_ask_set_enabled(
    app: AppHandle,
    ctx: CtxState<'_>,
    value: bool,
) -> Result<HotkeysView, String> {
    ctx.config.lock().screen_ask.enabled = value;
    let saved = ctx.save_config();
    hotkeys::apply(&app, &ctx, "screen_ask");
    if !value {
        ctx.screen_ask_cancel();
    }
    saved?;
    Ok(hotkeys::view(&ctx))
}

fn png_data_url(png: &[u8]) -> String {
    use base64::Engine;
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    )
}

/// The waiting picture as a data URL for the chat card.
#[tauri::command]
pub fn screen_ask_preview(ctx: CtxState<'_>) -> Result<String, String> {
    ctx.screen_ask_png()
        .map(|png| png_data_url(&png))
        .ok_or_else(|| tr("screen_ask.err_gone"))
}

/// «Спросить»: the picture and the typed question go to the model with vision.
#[tauri::command]
pub fn screen_ask_send(ctx: CtxState<'_>, question: String) -> Result<(), String> {
    ctx.inner().screen_ask_send(&question)
}

/// The question by voice: records a phrase and sends it with the picture.
/// `false`: this call only finished the recording that was running. An empty
/// error means the recording was cancelled; the page shows nothing for it.
#[tauri::command]
pub async fn screen_ask_listen(ctx: CtxState<'_>) -> Result<bool, String> {
    // Every refusal of sending is made before a phrase is recorded.
    let ctx = ctx.inner().clone();
    ctx.screen_ask_hear().await
}

/// The card was closed: the picture is thrown away.
#[tauri::command]
pub fn screen_ask_cancel(ctx: CtxState<'_>) {
    ctx.screen_ask_cancel();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_what_the_page_reads() {
        let view = ScreenAskView {
            app: "excel.exe".into(),
            title: "Отчёт за октябрь.xlsx — Excel".into(),
            width: 1600,
            height: 900,
        };
        assert_eq!(
            screen_ask_payload(&view),
            serde_json::json!({
                "action": "screen_ask",
                "app": "excel.exe",
                "title": "Отчёт за октябрь.xlsx — Excel",
                "width": 1600,
                "height": 900,
            })
        );
    }

    #[test]
    fn preview_is_a_png_data_url() {
        assert_eq!(
            png_data_url(b"\x89PNG\r\n"),
            "data:image/png;base64,iVBORw0K"
        );
        assert_eq!(png_data_url(&[]), "data:image/png;base64,");
    }

    #[test]
    fn texts_of_the_card_and_the_refusals_exist() {
        for key in [
            "screen_ask.err_gone",
            "screen_ask.err_off",
            "screen_ask.err_screen_off",
            "screen_ask.err_no_window",
            "screen_ask.err_own_window",
            "screen_ask.err_blind",
            "screen_ask.err_failed",
            "screen_ask.err_busy",
            "dictation.err_unsure",
            "dictation.err_busy",
            "dictation.err_no_window",
            "dictation.err_own_window",
            "dictation.err_blind",
            "dictation.err_password",
            "dictation.err_keys_held",
            "dictation.err_no_speech",
            "dictation.err_failed",
            "decision.direct",
            "by.hotkey",
        ] {
            assert_ne!(tr(key), key, "{key} is missing in ru.json");
        }
        assert!(tr("dictation.err_failed").contains("{why}"));
        assert!(tr("screen_ask.err_failed").contains("{why}"));
    }
}
