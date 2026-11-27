//! Avatar models the user brings in: the commands of the «Персонаж» page
//! and the signal that tells a page whether its window is seen.
//!
//! The page never names a file: the dialog is opened here, and a model is
//! known to the page only by its id. The journal gets the id and the kind of
//! an error, never a title, an author or a path.

use std::sync::Arc;

use base64::Engine;
use mika_core::avatar::{is_model_id, ModelEntry, ModelError, Profile, Spec, Store};
use mika_core::Emotion;
use tauri::{AppHandle, Emitter};

use crate::file_dialog::pick_open;
use crate::strings::tr;
use crate::tray::UI_CHANNEL;

type CtxState<'a> = tauri::State<'a, Arc<crate::app::Ctx>>;

/// Pictures of one model: the ten emotions twice and the whole figure.
const MAX_STILLS: usize = 21;

fn store() -> Store {
    Store::new(mika_core::paths::models_dir())
}

/// A model as the page lists it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ModelView {
    id: String,
    title: String,
    author: String,
    license: String,
    spec: Spec,
    size_bytes: u64,
    has_stills: bool,
}

impl From<ModelEntry> for ModelView {
    fn from(entry: ModelEntry) -> Self {
        ModelView {
            id: entry.id,
            title: entry.profile.title,
            author: entry.profile.author,
            license: entry.profile.license,
            spec: entry.profile.spec,
            size_bytes: entry.size_bytes,
            has_stills: entry.has_stills,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AvatarModels {
    /// The model shown now; empty is the built-in avatar.
    active: String,
    models: Vec<ModelView>,
}

/// A picture of a model on its way to or from the page.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Still {
    name: String,
    png_base64: String,
}

/// The id to show: `configured` when its folder is there, else the built-in avatar.
pub fn active_model(configured: &str, present: &[String]) -> String {
    if present.iter().any(|id| id == configured) {
        configured.to_string()
    } else {
        String::new()
    }
}

/// A model was deleted: when it was the chosen one, the choice goes back to
/// the built-in avatar. Says whether the setting changed.
fn forget_deleted(chosen: &mut String, deleted: &str) -> bool {
    let was_chosen = !deleted.is_empty() && chosen == deleted;
    if was_chosen {
        chosen.clear();
    }
    was_chosen
}

/// The text of an error for the user, in the interface language.
pub fn error_text(error: &ModelError) -> String {
    let key = format!("error.model_{}", error.kind());
    match error {
        ModelError::Unsupported(name) => mika_core::i18n::tr_args(&key, &[("name", name)]),
        _ => tr(&key),
    }
}

/// The text of an error of an animation: its own limit and its own format,
/// the rest as for a model.
pub fn motion_error_text(error: &ModelError) -> String {
    match error {
        ModelError::TooBig => tr("error.motion_too_big"),
        ModelError::NotVrm | ModelError::NotGlb => tr("error.motion_not_vrma"),
        other => error_text(other),
    }
}

fn failed_motion(what: &str, error: &ModelError) -> String {
    log::warn!(target: "app", "avatar animation: {what} failed: kind={}", error.kind());
    motion_error_text(error)
}

/// What the page is told about its window.
fn shown_payload(label: &str, shown: bool) -> serde_json::Value {
    serde_json::json!({ "action": "window_shown", "label": label, "shown": shown })
}

/// Why the page could not show a model, as the journal names it. The page's
/// own text never reaches a record.
fn failure_kind(kind: &str) -> &'static str {
    match kind {
        "webgl" => "webgl",
        "load" => "load",
        "context_lost" => "context_lost",
        _ => "other",
    }
}

/// The id as a record may carry it: only the shape of a real one.
fn safe_id(id: &str) -> &str {
    if is_model_id(id) {
        id
    } else {
        "?"
    }
}

fn still_in(still: &Still) -> Result<(String, Vec<u8>), ModelError> {
    // A canvas gives a data URL; the base64 is what follows the comma.
    let text = still
        .png_base64
        .rsplit_once(',')
        .map_or(still.png_base64.as_str(), |(_, data)| data);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| ModelError::BadStill)?;
    Ok((still.name.clone(), bytes))
}

fn still_out(name: String, bytes: &[u8]) -> Still {
    Still {
        name,
        png_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

/// Tells the pages whether window `label` (`island` or `avatar`) is seen:
/// a model is not drawn in a window nobody sees.
pub fn window_shown(app: &AppHandle, label: &str, shown: bool) {
    if label == crate::window::AVATAR_LABEL {
        crate::window::AVATAR_SHOWN.store(shown, std::sync::atomic::Ordering::Relaxed);
    }
    let _ = app.emit(UI_CHANNEL, shown_payload(label, shown));
}

/// Tells every window to read the chosen model, its profile and pictures again.
fn changed(app: &AppHandle) {
    let _ = app.emit(
        UI_CHANNEL,
        serde_json::json!({ "action": "avatar_model_changed" }),
    );
}

fn failed(what: &str, error: &ModelError) -> String {
    log::warn!(target: "app", "avatar model: {what} failed: kind={}", error.kind());
    error_text(error)
}

#[tauri::command]
pub fn avatar_models(ctx: CtxState<'_>) -> AvatarModels {
    let models = store().list();
    let configured = ctx.config.lock().avatar.model.clone();
    let present: Vec<String> = models.iter().map(|m| m.id.clone()).collect();
    let active = active_model(&configured, &present);
    if active.is_empty() && !configured.is_empty() {
        static NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !NOTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            log::warn!(
                target: "app",
                "the chosen avatar model is gone, showing the built-in one: id={}",
                safe_id(&configured)
            );
        }
    }
    AvatarModels {
        active,
        models: models.into_iter().map(ModelView::from).collect(),
    }
}

/// Lets the user pick a `.vrm` file and takes it in. `None` when the dialog
/// was closed.
#[tauri::command]
pub async fn avatar_model_add(window: tauri::WebviewWindow) -> Result<Option<ModelView>, String> {
    let owner = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = pick_open(owner, &tr("persona.model_pick"), ("VRM", "*.vrm"))? else {
            return Ok(None);
        };
        let entry = store().import(&path).map_err(|e| failed("import", &e))?;
        log::info!(
            target: "app",
            "avatar model imported: id={} spec={:?} bytes={} expressions={}",
            entry.id,
            entry.profile.spec,
            entry.size_bytes,
            entry.profile.expressions.len()
        );
        Ok(Some(ModelView::from(entry)))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Chooses the model to show; an empty id is the built-in avatar.
#[tauri::command]
pub fn avatar_model_select(app: AppHandle, ctx: CtxState<'_>, id: String) -> Result<(), String> {
    if !id.is_empty() {
        store().entry(&id).map_err(|e| failed("select", &e))?;
    }
    ctx.config.lock().avatar.model = id.clone();
    ctx.save_config()?;
    log::info!(
        target: "app",
        "avatar model chosen: id={}",
        if id.is_empty() { "builtin" } else { id.as_str() }
    );
    changed(&app);
    Ok(())
}

#[tauri::command]
pub async fn avatar_model_delete(
    app: AppHandle,
    ctx: CtxState<'_>,
    id: String,
) -> Result<(), String> {
    let gone = id.clone();
    tauri::async_runtime::spawn_blocking(move || store().delete(&gone))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| failed("delete", &e))?;
    let was_chosen = forget_deleted(&mut ctx.config.lock().avatar.model, &id);
    if was_chosen {
        ctx.save_config()?;
    }
    log::info!(target: "app", "avatar model deleted: id={id} was_chosen={was_chosen}");
    changed(&app);
    Ok(())
}

#[tauri::command]
pub fn avatar_model_profile(id: String) -> Result<Profile, String> {
    store()
        .entry(&id)
        .map(|entry| entry.profile)
        .map_err(|e| failed("profile", &e))
}

/// The model file as raw bytes. The store checks it again before reading.
#[tauri::command]
pub async fn avatar_model_bytes(id: String) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || store().bytes(&id))
        .await
        .map_err(|e| e.to_string())?
        .map(tauri::ipc::Response::new)
        .map_err(|e| failed("load", &e))
}

fn motions() -> mika_core::avatar::MotionStore {
    mika_core::avatar::MotionStore::new(mika_core::paths::motions_dir())
}

/// Tells every window the list of animations changed.
fn motions_changed(app: &AppHandle) {
    let _ = app.emit(
        UI_CHANNEL,
        serde_json::json!({ "action": "avatar_motions_changed" }),
    );
}

/// The animations the user brought in, sorted by name.
#[tauri::command]
pub fn avatar_motions() -> Vec<mika_core::avatar::MotionEntry> {
    motions().list()
}

/// Lets the user pick a `.vrma` file and takes it in. `None` when the dialog was closed.
#[tauri::command]
pub async fn avatar_motion_add(
    app: AppHandle,
    window: tauri::WebviewWindow,
) -> Result<Option<mika_core::avatar::MotionEntry>, String> {
    let owner = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    let added = tauri::async_runtime::spawn_blocking(move || {
        let Some(path) = pick_open(owner, &tr("persona.motion_pick"), ("VRMA", "*.vrma"))? else {
            return Ok(None);
        };
        let entry = motions()
            .import(&path)
            .map_err(|e| failed_motion("import", &e))?;
        // The name came from the user's file: it stays out of the journal.
        log::info!(target: "app", "avatar animation imported: id={} ms={}", entry.id, entry.ms);
        Ok(Some(entry))
    })
    .await
    .map_err(|e| e.to_string())?;
    if matches!(added, Ok(Some(_))) {
        motions_changed(&app);
    }
    added
}

#[tauri::command]
pub fn avatar_motion_rename(
    app: AppHandle,
    id: String,
    name: String,
) -> Result<mika_core::avatar::MotionEntry, String> {
    let entry = motions()
        .rename(&id, &name)
        .map_err(|e| failed_motion("rename", &e))?;
    motions_changed(&app);
    Ok(entry)
}

#[tauri::command]
pub fn avatar_motion_delete(app: AppHandle, ctx: CtxState<'_>, id: String) -> Result<(), String> {
    motions()
        .delete(&id)
        .map_err(|e| failed_motion("delete", &e))?;
    log::info!(target: "app", "avatar animation deleted: id={}", safe_id(&id));
    // A deleted animation is no longer the one she loops at rest.
    if ctx.config.lock().avatar.forget_motion(&id) {
        ctx.save_config()?;
        look_changed(&app);
    }
    motions_changed(&app);
    Ok(())
}

/// The animation file as raw bytes. The store checks it again before reading.
#[tauri::command]
pub async fn avatar_motion_bytes(id: String) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || motions().bytes(&id))
        .await
        .map_err(|e| e.to_string())?
        .map(tauri::ipc::Response::new)
        .map_err(|e| failed_motion("load", &e))
}

/// Plays an animation in every window that shows a model: the «▶» of the page.
#[tauri::command]
pub fn avatar_motion_play(ctx: CtxState<'_>, id: String) -> Result<(), String> {
    let entry = motions()
        .list()
        .into_iter()
        .find(|m| m.id == id)
        .ok_or_else(|| failed_motion("play", &ModelError::Unknown))?;
    ctx.bus.publish(mika_core::Event::AvatarGesture {
        name: entry.name,
        motion: Some(entry.id),
    });
    Ok(())
}

/// The look of the model and the manner of her character (decision 0055).
#[derive(serde::Serialize)]
pub struct LookView {
    #[serde(flatten)]
    look: mika_core::config::AvatarLook,
    /// The id of the ready character in force; `custom` for an own one.
    persona: &'static str,
}

/// How a model looks and moves: pose, size, frame, cursor, idle actions,
/// and whose manner she moves in.
#[tauri::command]
pub fn avatar_look(ctx: CtxState<'_>) -> LookView {
    let c = ctx.config.lock();
    LookView {
        look: c.avatar.look(),
        persona: manner_of(&c.assistant),
    }
}

/// The character whose manner her body takes: an own character, active or
/// left without one, moves as `custom`.
pub fn manner_of(a: &mika_core::config::AssistantConfig) -> &'static str {
    if a.active_persona().is_some() {
        mika_core::config::Persona::Custom.id()
    } else {
        a.persona.id()
    }
}

/// Takes the look from the «Персонаж» page; a new size resizes the desktop window.
#[tauri::command]
pub fn avatar_set_look(
    app: AppHandle,
    ctx: CtxState<'_>,
    look: mika_core::config::AvatarLook,
) -> Result<mika_core::config::AvatarLook, String> {
    let (before, now) = {
        let mut c = ctx.config.lock();
        let before = c.avatar.scale;
        c.avatar.set_look(look);
        (before, c.avatar.look())
    };
    ctx.save_config()?;
    if (before - now.scale).abs() > f32::EPSILON {
        if let Err(e) = crate::window::resize_desktop_avatar(&app, now.scale) {
            log::warn!(target: "app", "the avatar window was not resized: {e}");
        }
    }
    look_changed(&app);
    Ok(now)
}

/// The wheel over her on the desktop: a step bigger or smaller.
#[tauri::command]
pub fn avatar_scale_step(app: AppHandle, ctx: CtxState<'_>, delta: f32) -> Result<f32, String> {
    let step = if delta > 0.0 { 0.1 } else { -0.1 };
    let scale = {
        let mut c = ctx.config.lock();
        let next =
            mika_core::config::clamp_avatar_scale(((c.avatar.scale + step) * 10.0).round() / 10.0);
        if (next - c.avatar.scale).abs() < f32::EPSILON {
            return Ok(next);
        }
        c.avatar.scale = next;
        next
    };
    ctx.save_config()?;
    if let Err(e) = crate::window::resize_desktop_avatar(&app, scale) {
        log::warn!(target: "app", "the avatar window was not resized: {e}");
    }
    look_changed(&app);
    Ok(scale)
}

/// The desktop window asks for the cursor: it tells clicks on her from
/// clicks past her. She no longer looks at it (decision 0057), so the
/// island gets none.
#[tauri::command]
pub fn avatar_cursor_wanted(window: tauri::WebviewWindow, on: bool) {
    if window.label() == crate::window::AVATAR_LABEL {
        crate::window::CURSOR_AVATAR.store(on, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The cursor is over her in the desktop window, or not: the window takes
/// clicks on her and lets every other click through.
#[tauri::command]
pub fn avatar_hover(window: tauri::WebviewWindow, over: bool) {
    if window.label() != crate::window::AVATAR_LABEL {
        return;
    }
    crate::window::AVATAR_OVER.store(over, std::sync::atomic::Ordering::Relaxed);
    crate::window::apply_avatar_clicks(&window);
}

/// The user starts to drag the desktop window: where it stops is kept.
#[tauri::command]
pub fn avatar_dragging() {
    crate::window::AVATAR_DRAGGED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Tells every window the look of the model changed.
pub fn look_changed(app: &AppHandle) {
    let _ = app.emit(
        UI_CHANNEL,
        serde_json::json!({ "action": "avatar_look_changed" }),
    );
}

/// `expression` is a name from the profile, or nothing to let the program choose.
#[tauri::command]
pub fn avatar_model_set_slot(
    app: AppHandle,
    id: String,
    emotion: Emotion,
    expression: Option<String>,
) -> Result<Profile, String> {
    let profile = store()
        .set_slot(&id, emotion, expression.as_deref())
        .map_err(|e| failed("slot", &e))?;
    changed(&app);
    Ok(profile)
}

#[tauri::command]
pub fn avatar_model_set_lip(app: AppHandle, id: String, lip: f32) -> Result<Profile, String> {
    let profile = store().set_lip(&id, lip).map_err(|e| failed("lip", &e))?;
    changed(&app);
    Ok(profile)
}

/// The pictures the page drew of the model: PNG, known names only.
#[tauri::command]
pub async fn avatar_model_save_stills(
    app: AppHandle,
    id: String,
    stills: Vec<Still>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        if stills.len() > MAX_STILLS {
            return Err(ModelError::BadStill);
        }
        let decoded = stills.iter().map(still_in).collect::<Result<Vec<_>, _>>()?;
        store().save_stills(&id, &decoded)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| failed("stills", &e))?;
    changed(&app);
    Ok(())
}

#[tauri::command]
pub async fn avatar_model_stills(id: String) -> Vec<Still> {
    tauri::async_runtime::spawn_blocking(move || {
        store()
            .stills(&id)
            .into_iter()
            .map(|(name, bytes)| still_out(name, &bytes))
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// The page could not show the model and went back to the built-in avatar.
/// One record for a model and a kind in a run.
#[tauri::command]
pub fn avatar_model_failed(id: String, kind: String) {
    static NOTED: parking_lot::Mutex<Vec<(String, &'static str)>> =
        parking_lot::const_mutex(Vec::new());
    let entry = (safe_id(&id).to_string(), failure_kind(&kind));
    let mut noted = NOTED.lock();
    if noted.contains(&entry) || noted.len() >= 64 {
        return;
    }
    log::warn!(
        target: "app",
        "the avatar model is not shown, the built-in one is: id={} kind={}",
        entry.0,
        entry.1
    );
    noted.push(entry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::i18n::{tr_in, Lang};

    #[test]
    fn errors_of_an_animation_name_its_own_limits() {
        let big = motion_error_text(&ModelError::TooBig);
        assert!(big.contains("20"), "{big}");
        assert!(!big.contains("200"), "{big}");
        assert_ne!(
            motion_error_text(&ModelError::NotVrm),
            error_text(&ModelError::NotVrm)
        );
        // Other errors read the same for both.
        assert_eq!(
            motion_error_text(&ModelError::External),
            error_text(&ModelError::External)
        );
    }

    #[test]
    fn a_chosen_model_that_is_gone_shows_the_built_in_one() {
        let present = [
            "0123456789abcdef".to_string(),
            "fedcba9876543210".to_string(),
        ];
        assert_eq!(
            active_model("0123456789abcdef", &present),
            "0123456789abcdef"
        );
        assert_eq!(active_model("0123456789abcdef", &[]), "");
        assert_eq!(active_model("aaaaaaaaaaaaaaaa", &present), "");
        assert_eq!(active_model("", &present), "");
    }

    #[test]
    fn every_model_error_has_a_text_in_both_languages() {
        let all = [
            ModelError::NotGlb,
            ModelError::NotVrm,
            ModelError::TooBig,
            ModelError::External,
            ModelError::Unsupported("KHR_x".into()),
            ModelError::Broken,
            ModelError::Unknown,
            ModelError::BadStill,
            ModelError::Io(std::io::Error::other("x")),
        ];
        for error in &all {
            let key = format!("error.model_{}", error.kind());
            let (ru, en) = (tr_in(Lang::Ru, &key), tr_in(Lang::En, &key));
            assert_ne!(ru, key, "{key} is missing in ru.json");
            assert_ne!(en, ru, "{key} is missing in en.json");
            assert_ne!(error_text(error), key);
        }
        assert_ne!(tr_in(Lang::Ru, "persona.model_pick"), "persona.model_pick");
        assert_ne!(
            tr_in(Lang::En, "persona.model_pick"),
            tr_in(Lang::Ru, "persona.model_pick")
        );
    }

    #[test]
    fn an_unsupported_extension_is_named_to_the_user() {
        let text = error_text(&ModelError::Unsupported(
            "KHR_draco_mesh_compression".into(),
        ));
        assert!(text.contains("KHR_draco_mesh_compression"), "{text}");
        assert!(!text.contains("{name}"), "{text}");
    }

    #[test]
    fn the_window_signal_names_the_window_and_the_state() {
        assert_eq!(
            shown_payload("avatar", false),
            serde_json::json!({ "action": "window_shown", "label": "avatar", "shown": false })
        );
        assert_eq!(shown_payload("island", true)["shown"], true);
    }

    #[test]
    fn stills_cross_as_base64_png() {
        let bytes = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
        let still = still_out("joy-talk".into(), &bytes);
        assert_eq!(still.name, "joy-talk");
        assert_eq!(
            still_in(&still).unwrap(),
            ("joy-talk".to_string(), bytes.to_vec())
        );
        let bad = Still {
            name: "joy".into(),
            png_base64: "not base64 !!".into(),
        };
        assert!(matches!(still_in(&bad), Err(ModelError::BadStill)));
        // The page may send a data URL; only the bare base64 is taken.
        let url = Still {
            name: "joy".into(),
            png_base64: format!("data:image/png;base64,{}", still.png_base64),
        };
        assert_eq!(still_in(&url).unwrap().1, bytes.to_vec());
    }

    #[test]
    fn a_failure_kind_from_the_page_is_one_of_ours() {
        for kind in ["webgl", "load", "context_lost"] {
            assert_eq!(failure_kind(kind), kind);
        }
        assert_eq!(failure_kind("x\ny=1"), "other");
        assert_eq!(failure_kind(""), "other");
    }

    #[test]
    fn deleting_the_chosen_model_goes_back_to_the_built_in_one() {
        let mut chosen = "0123456789abcdef".to_string();
        assert!(forget_deleted(&mut chosen, "0123456789abcdef"));
        assert_eq!(chosen, "");
    }

    #[test]
    fn deleting_another_model_keeps_the_choice() {
        let mut chosen = "0123456789abcdef".to_string();
        assert!(!forget_deleted(&mut chosen, "fedcba9876543210"));
        assert_eq!(chosen, "0123456789abcdef");
        // Nothing chosen, nothing to forget.
        let mut none = String::new();
        assert!(!forget_deleted(&mut none, "fedcba9876543210"));
        assert!(!forget_deleted(&mut none, ""));
        assert_eq!(none, "");
    }

    #[test]
    fn a_record_carries_only_an_id_of_the_right_shape() {
        assert_eq!(safe_id("0123456789abcdef"), "0123456789abcdef");
        assert_eq!(safe_id("C:\\Users\\me\\secret.vrm"), "?");
        assert_eq!(safe_id("0123456789abcdef\nkey=1"), "?");
        assert_eq!(safe_id(""), "?");
    }
}
