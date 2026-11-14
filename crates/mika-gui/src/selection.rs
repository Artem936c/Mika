//! Text selected in another program: copy it on a shortcut, act on it, paste
//! the answer back. The user starts every step with a key or a button.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mika_core::policy::blind::is_blind_title;
use mika_tools::cliphist::ClipLoan;
use mika_tools::sys::WindowInfo;
use tauri::{AppHandle, Emitter};

use crate::app::Ctx;

/// Longest selection taken, in characters.
const MAX_CHARS: usize = 8000;

/// Text copied from a window, and that window for pasting the answer back.
#[derive(Debug, Clone)]
pub struct Selection {
    pub window: WindowInfo,
    pub text: String,
}

/// What to do with the selected text. Returns the request and whether the
/// answer replaces the selection.
pub fn instruction(action: &str) -> Option<(&'static str, bool)> {
    if let Some(found) = crate::lang::selection_instruction(action) {
        return Some(found);
    }
    Some(match action {
        "translate" => (
            "Переведи текст ниже: русский на английский, любой другой язык на русский. Ответь только переводом, без пояснений и кавычек.",
            true,
        ),
        "fix" => (
            "Исправь в тексте ниже орфографию, пунктуацию и грамматику, смысл и стиль не меняй. Ответь только исправленным текстом, без пояснений и кавычек.",
            true,
        ),
        "rephrase" => (
            "Перепиши текст ниже понятнее и вежливее, смысл не меняй. Ответь только новым текстом, без пояснений и кавычек.",
            true,
        ),
        "shorter" => (
            "Сократи текст ниже вдвое, сохрани главное. Ответь только новым текстом, без пояснений и кавычек.",
            true,
        ),
        "explain" => ("Объясни простыми словами, что значит текст ниже.", false),
        _ => return None,
    })
}

/// The message sent to the assistant for `action`.
pub fn request(action: &str, text: &str) -> Option<String> {
    let (what, _) = instruction(action)?;
    Some(format!("{what}\n\n{text}"))
}

/// Waits until Ctrl, Alt, Shift and Win are up: a copy sent while the user
/// still holds Alt of the shortcut would become Ctrl+Alt+C.
fn wait_keys_up(limit: Duration) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    let started = Instant::now();
    while started.elapsed() < limit {
        // SAFETY: reads the key state; no pointers involved.
        let down = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .iter()
            .any(|k| unsafe { GetAsyncKeyState(*k as i32) } as u16 & 0x8000 != 0);
        if !down {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

const VK_C: u16 = 0x43;
const VK_V: u16 = 0x56;
const VK_CONTROL: u16 = 0x11;

/// Copies the selection of the window in front and gives the clipboard back
/// what it held. Blocking.
pub fn grab(ctx: &Ctx) -> Result<Selection, String> {
    let env = ctx.tool_env();
    let own = mika_tools::own_process();
    let window = env
        .windows
        .foreground()
        .filter(|w| w.process != own)
        .ok_or_else(|| crate::strings::tr("selection.no_window"))?;
    if is_blind_title(&window.title) {
        return Err(crate::strings::tr("selection.blind"));
    }
    wait_keys_up(Duration::from_millis(1500));
    // Her mark and what she puts back are not the user's copies.
    let loan = ClipLoan::start(env.clipboard.clone(), &env.shared.clips);
    // A mark tells a real copy from an empty selection.
    let mark = format!("\u{2063}mika-{}", std::process::id());
    env.clipboard.set(&mark)?;
    env.input.chord(&[VK_CONTROL, VK_C])?;
    let started = Instant::now();
    let mut text = String::new();
    while started.elapsed() < Duration::from_millis(800) {
        std::thread::sleep(Duration::from_millis(40));
        match env.clipboard.get() {
            Ok(t) if t != mark => {
                text = t;
                break;
            }
            _ => {}
        }
    }
    if !loan.saved() {
        // Nothing to put back: at least her mark does not stay.
        let _ = env.clipboard.set("");
    }
    drop(loan);
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(crate::strings::tr("selection.empty"));
    }
    let text: String = text.chars().take(MAX_CHARS).collect();
    Ok(Selection { window, text })
}

/// Puts `text` in place of the selection: back to the window, paste, and the
/// clipboard gets back what it held, after an error too. Blocking.
pub fn paste(ctx: &Ctx, sel: &Selection, text: &str) -> Result<(), String> {
    let env = ctx.tool_env();
    let alive = env
        .windows
        .list()
        .into_iter()
        .find(|w| w.id == sel.window.id)
        .ok_or_else(|| crate::strings::tr("selection.window_gone"))?;
    if is_blind_title(&alive.title) {
        return Err(crate::strings::tr("selection.blind"));
    }
    env.windows.focus(alive.id)?;
    std::thread::sleep(Duration::from_millis(250));
    let _loan = ClipLoan::start(env.clipboard.clone(), &env.shared.clips);
    env.clipboard.set(text)?;
    env.input.chord(&[VK_CONTROL, VK_V])?;
    // The program reads the clipboard after the key: give it time.
    std::thread::sleep(Duration::from_millis(400));
    Ok(())
}

/// The shortcut: take the selection and show it in the chat.
pub fn on_shortcut(app: &AppHandle, ctx: Arc<Ctx>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let worker = ctx.clone();
        let taken = tauri::async_runtime::spawn_blocking(move || grab(&worker))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
        crate::tray::open_chat(&app);
        match taken {
            Ok(sel) => {
                let payload = serde_json::json!({
                    "action": "selection",
                    "text": sel.text,
                    "app": sel.window.process,
                });
                *ctx.selection.lock() = Some(sel);
                let _ = app.emit(crate::tray::UI_CHANNEL, payload);
            }
            Err(message) => {
                ctx.bus.publish(mika_core::Event::Error { message });
            }
        }
    });
}

/// Last reply of the assistant without emotion tags: what is pasted.
pub fn last_reply(ctx: &Ctx) -> Option<String> {
    let reply = ctx
        .session
        .agent
        .history()
        .into_iter()
        .rev()
        .find(|m| {
            m.role == mika_core::llm::Role::Assistant
                && m.tool_calls.is_empty()
                && !m.content.trim().is_empty()
        })?
        .content;
    let clean = without_tags(&reply);
    (!clean.is_empty()).then_some(clean)
}

/// Drops emotion tags such as `[joy]` and the quotes around the whole answer.
fn without_tags(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(len) = rest[open..].find(']') else {
            break;
        };
        let inner = &rest[open + 1..open + len];
        out.push_str(&rest[..open]);
        // A tag is one short word; anything else stays.
        if inner.is_empty() || inner.len() > 20 || !inner.chars().all(char::is_alphabetic) {
            out.push_str(&rest[open..=open + len]);
        }
        rest = &rest[open + len + 1..];
    }
    out.push_str(rest);
    out.trim().trim_matches(['«', '»', '"']).trim().to_string()
}

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// A button on the selection card: send the selected text with the request.
#[tauri::command]
pub fn selection_action(ctx: CtxState<'_>, action: String) -> Result<(), String> {
    let text = ctx
        .selection
        .lock()
        .as_ref()
        .map(|s| s.text.clone())
        .ok_or_else(|| crate::strings::tr("selection.none"))?;
    let message = request(&action, &text).ok_or("unknown action")?;
    ctx.end_voice_dialog();
    let session = ctx.session.clone();
    tauri::async_runtime::spawn(async move { session.submit(&message) });
    Ok(())
}

/// «Вставить»: the last answer replaces the selection in its window.
#[tauri::command]
pub async fn selection_paste(ctx: CtxState<'_>) -> Result<(), String> {
    let ctx = ctx.inner().clone();
    let sel = ctx
        .selection
        .lock()
        .clone()
        .ok_or_else(|| crate::strings::tr("selection.none"))?;
    let text = last_reply(&ctx).ok_or_else(|| crate::strings::tr("selection.no_answer"))?;
    tauri::async_runtime::spawn_blocking(move || paste(&ctx, &sel, &text))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn selection_clear(ctx: CtxState<'_>) {
    *ctx.selection.lock() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_a_request_and_only_transforms_paste() {
        for a in ["translate", "fix", "rephrase", "shorter"] {
            assert!(instruction(a).unwrap().1, "{a}");
        }
        assert!(!instruction("explain").unwrap().1);
        assert!(instruction("delete").is_none());
        let r = request("fix", "превет").unwrap();
        assert!(r.ends_with("\n\nпревет") && r.starts_with("Исправь"));
    }

    #[test]
    fn tags_and_outer_quotes_are_not_pasted() {
        assert_eq!(without_tags("[joy] «Hello, world»"), "Hello, world");
        assert_eq!(without_tags("a[1] b [x y]"), "a[1] b [x y]");
    }
}
