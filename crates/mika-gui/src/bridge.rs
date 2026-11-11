//! Event bridge from the core bus to the Svelte UI, and UI commands.

use std::sync::Arc;

use mika_core::{AssistantState, EventBus, StateHandle};
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast::error::RecvError;

use crate::window::IslandRect;

pub const EVENT_CHANNEL: &str = "mika://event";

/// Forwards every bus event to the UI as JSON.
pub fn spawn_bridge(app: AppHandle, bus: EventBus) {
    let mut rx = bus.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Err(e) = app.emit(EVENT_CHANNEL, &event) {
                        log::warn!("emit failed: {e}");
                    }
                }
                Err(RecvError::Lagged(n)) => log::warn!("UI bridge lagged by {n} events"),
                Err(RecvError::Closed) => break,
            }
        }
    });
}

#[tauri::command]
pub fn get_state(state: tauri::State<'_, StateHandle>) -> AssistantState {
    state.get()
}

#[tauri::command]
pub fn debug_set_state(state: tauri::State<'_, StateHandle>, next: AssistantState) -> bool {
    if !cfg!(debug_assertions) {
        return false;
    }
    state.set(next)
}

#[tauri::command]
pub fn set_island_rect(rect: tauri::State<'_, Arc<Mutex<IslandRect>>>, value: IslandRect) {
    *rect.lock() = value;
}

// ---- chat ----

type CtxState<'a> = tauri::State<'a, Arc<crate::app::Ctx>>;

/// Answers an open question or starts a reply in the background.
#[tauri::command]
pub fn send_message(ctx: CtxState<'_>, text: String) {
    let session = ctx.session.clone();
    // Session::submit spawns on tokio; run it inside the Tauri runtime.
    tauri::async_runtime::spawn(async move {
        session.submit(&text);
    });
}

/// Stops the running reply, drops queued ones and denies an open question.
#[tauri::command]
pub fn stop(ctx: CtxState<'_>) {
    ctx.session.stop();
}

#[derive(serde::Serialize)]
pub struct HistoryItem {
    role: &'static str,
    text: String,
}

#[tauri::command]
pub fn get_history(ctx: CtxState<'_>) -> Vec<HistoryItem> {
    use mika_core::llm::Role;
    ctx.session
        .agent
        .history()
        .into_iter()
        .filter(|m| !m.content.trim().is_empty())
        .filter_map(|m| match m.role {
            Role::User => Some(HistoryItem {
                role: "user",
                text: m.content,
            }),
            Role::Assistant => Some(HistoryItem {
                role: "assistant",
                text: m.content,
            }),
            _ => None,
        })
        .collect()
}

#[tauri::command]
pub fn clear_history(ctx: CtxState<'_>) -> Result<(), String> {
    ctx.session.agent.clear_history();
    ctx.db
        .clear_messages(crate::app::CONVERSATION)
        .map_err(|e| e.to_string())
}

// ---- confirmations and permissions ----

#[tauri::command]
pub fn answer_confirm(
    ctx: CtxState<'_>,
    id: u64,
    answer: mika_core::confirm::Answer,
) -> Result<(), String> {
    ctx.session
        .confirms
        .answer(id, answer, mika_core::confirm::DecidedBy::Button)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_pending_confirm(ctx: CtxState<'_>) -> Option<mika_core::confirm::PendingView> {
    ctx.session.confirms.pending()
}

#[tauri::command]
pub fn list_rules(ctx: CtxState<'_>) -> Result<Vec<mika_core::policy::Rule>, String> {
    ctx.rules.list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_rule(ctx: CtxState<'_>, id: i64) -> Result<bool, String> {
    ctx.rules.remove(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn recent_audit(ctx: CtxState<'_>, limit: usize) -> Vec<mika_core::policy::AuditEntry> {
    ctx.audit.recent(limit.min(200))
}

// ---- settings ----

#[derive(serde::Serialize)]
pub struct LlmSettingsView {
    kind: mika_core::config::ProviderKind,
    base_url: String,
    default_base_url: &'static str,
    model: String,
    has_key: bool,
}

#[tauri::command]
pub fn get_llm_settings(ctx: CtxState<'_>) -> LlmSettingsView {
    let llm = ctx.config.lock().llm.clone();
    let has_key = matches!(ctx.secrets.get(&llm.kind.secret_key()), Ok(Some(_)));
    LlmSettingsView {
        kind: llm.kind,
        default_base_url: llm.kind.default_base_url(),
        base_url: llm.base_url,
        model: llm.model,
        has_key,
    }
}

/// `api_key`: `None` keeps the stored key, an empty string deletes it.
#[tauri::command]
pub fn save_llm_settings(
    ctx: CtxState<'_>,
    kind: mika_core::config::ProviderKind,
    base_url: String,
    model: String,
    api_key: Option<String>,
) -> Result<(), String> {
    if let Some(key) = api_key {
        ctx.secrets
            .set(
                &kind.secret_key(),
                &mika_core::secrets::Secret::new(key.trim()),
            )
            .map_err(|e| e.to_string())?;
    }
    {
        let mut cfg = ctx.config.lock();
        cfg.llm.kind = kind;
        cfg.llm.base_url = base_url.trim().to_string();
        cfg.llm.model = model.trim().to_string();
    }
    ctx.save_config()?;
    ctx.reload_provider();
    Ok(())
}

/// Milliseconds to the first chunk of a tiny request.
#[tauri::command]
pub async fn test_provider(ctx: tauri::State<'_, Arc<crate::app::Ctx>>) -> Result<u64, String> {
    let cfg = ctx.config.lock().llm.clone();
    let provider =
        mika_providers::build_provider(&cfg, ctx.secrets.as_ref()).map_err(|e| e.to_string())?;
    mika_providers::probe(provider.as_ref())
        .await
        .map(|d| d.as_millis() as u64)
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct UiSettings {
    web_enabled: bool,
    name: String,
    speak_replies: bool,
    microphone_enabled: bool,
    screen_enabled: bool,
    persona: mika_core::config::Persona,
    custom_prompt: String,
    avatar_in_island: bool,
    avatar_on_desktop: bool,
}

#[tauri::command]
pub fn get_ui_settings(ctx: CtxState<'_>) -> UiSettings {
    let c = ctx.config.lock();
    UiSettings {
        web_enabled: c.tools.web_enabled,
        name: c.assistant.name.clone(),
        speak_replies: c.voice.speak_replies,
        microphone_enabled: c.privacy.microphone_enabled,
        screen_enabled: c.privacy.screen_enabled,
        persona: c.assistant.persona,
        custom_prompt: c.assistant.custom_prompt.clone(),
        avatar_in_island: c.avatar.in_island,
        avatar_on_desktop: c.avatar.on_desktop,
    }
}

/// Longest assistant name, characters.
pub const MAX_NAME: usize = 24;

/// Trimmed name, or the reason it cannot be used.
pub fn clean_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    match name.chars().count() {
        0 => Err(crate::strings::tr("error.name_empty")),
        n if n > MAX_NAME => Err(crate::strings::tr("error.name_long")),
        _ => Ok(name.to_string()),
    }
}

/// Tells every window to read the settings again.
fn settings_changed(app: &AppHandle) {
    let _ = app.emit(
        crate::tray::UI_CHANNEL,
        serde_json::json!({ "action": "settings_changed" }),
    );
}

#[tauri::command]
pub fn set_assistant_name(app: AppHandle, ctx: CtxState<'_>, value: String) -> Result<(), String> {
    let name = clean_name(&value)?;
    ctx.config.lock().assistant.name = name;
    ctx.save_config()?;
    ctx.refresh_prompt();
    settings_changed(&app);
    Ok(())
}

#[tauri::command]
pub fn set_persona(
    app: AppHandle,
    ctx: CtxState<'_>,
    persona: mika_core::config::Persona,
    custom_prompt: String,
) -> Result<(), String> {
    {
        let mut cfg = ctx.config.lock();
        cfg.assistant.persona = persona;
        cfg.assistant.custom_prompt = custom_prompt
            .trim()
            .chars()
            .take(mika_core::config::MAX_CUSTOM_PROMPT)
            .collect();
    }
    ctx.save_config()?;
    ctx.refresh_prompt();
    settings_changed(&app);
    Ok(())
}

/// Async: a window must not be created from a synchronous command on Windows.
#[tauri::command]
pub async fn set_avatar_placement(
    app: AppHandle,
    ctx: tauri::State<'_, Arc<crate::app::Ctx>>,
    in_island: bool,
    on_desktop: bool,
) -> Result<(), String> {
    {
        let mut cfg = ctx.config.lock();
        cfg.avatar.in_island = in_island;
        cfg.avatar.on_desktop = on_desktop;
    }
    ctx.save_config()?;
    crate::window::set_desktop_avatar(&app, on_desktop).map_err(|e| e.to_string())?;
    settings_changed(&app);
    Ok(())
}

#[tauri::command]
pub fn get_emotion(
    emotion: tauri::State<'_, mika_core::emotion::EmotionHandle>,
) -> mika_core::Emotion {
    emotion.get()
}

#[tauri::command]
pub fn set_speak_replies(ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.config.lock().voice.speak_replies = value;
    ctx.save_config()
}

#[tauri::command]
pub fn set_web_enabled(ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.set_web_enabled(value)
}

// ---- general ----

/// Gives the island keyboard focus so the chat input can be typed in.
#[tauri::command]
pub fn focus_island(window: tauri::WebviewWindow) {
    if let Err(e) = window.set_focus() {
        log::warn!("set_focus failed: {e}");
    }
}

#[derive(serde::Serialize)]
pub struct GeneralSettings {
    version: &'static str,
    autostart: bool,
    shortcut: String,
    data_dir: String,
    logs_dir: String,
}

#[tauri::command]
pub fn get_general_settings() -> GeneralSettings {
    GeneralSettings {
        version: env!("CARGO_PKG_VERSION"),
        autostart: crate::platform::autostart_enabled(),
        shortcut: crate::tray::shortcut_label(),
        data_dir: mika_core::paths::data_dir().display().to_string(),
        logs_dir: mika_core::paths::logs_dir().display().to_string(),
    }
}

#[tauri::command]
pub fn set_autostart(value: bool) -> Result<(), String> {
    crate::platform::set_autostart(value)
}

#[tauri::command]
pub fn open_folder(which: String) -> Result<(), String> {
    let dir = match which.as_str() {
        "logs" => mika_core::paths::logs_dir(),
        _ => mika_core::paths::data_dir(),
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer.exe")
        .arg(&dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Deletes keys, autostart, settings, history, rules and logs, then quits.
#[tauri::command]
pub fn wipe_all_data(app: AppHandle, ctx: CtxState<'_>) -> Result<(), String> {
    ctx.session.stop();
    crate::platform::wipe_all_data(ctx.secrets.as_ref())?;
    log::info!("all data wiped, exiting");
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_trimmed_and_limited() {
        assert_eq!(clean_name("  Айри \n"), Ok("Айри".into()));
        assert_eq!(clean_name(&"я".repeat(MAX_NAME)), Ok("я".repeat(MAX_NAME)));
        assert!(clean_name("   ").is_err());
        assert!(clean_name(&"я".repeat(MAX_NAME + 1)).is_err());
    }
}
