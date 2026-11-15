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
