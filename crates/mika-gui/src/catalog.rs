//! Settings commands: the provider's models and voices to choose from.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mika_core::config::{
    Config, ProviderKind, EMBEDDING_SECRET_KEY, IMAGE_SECRET_KEY, SPEECH_SECRET_KEY,
};
use mika_core::secrets::Secret;
use mika_providers::catalog::{self, ModelInfo, Purpose};
use parking_lot::Mutex;

use crate::app::Ctx;

/// A model list is asked again only after this long.
const CACHE_FOR: Duration = Duration::from_secs(600);

#[derive(serde::Serialize)]
pub struct ModelList {
    models: Vec<ModelInfo>,
    /// `₽`, `$` or empty when the provider is not known.
    currency: &'static str,
    /// `false`: the provider gave no abilities, so the whole list is shown.
    filtered: bool,
}

#[derive(serde::Serialize)]
pub struct VoiceList {
    voices: Vec<String>,
    /// `provider`: the provider listed them; `known`: documented for the model family.
    source: &'static str,
}

/// Address and key used for `purpose`: the section's own, or the LLM's.
fn endpoint(
    cfg: &Config,
    ctx: &Ctx,
    purpose: Purpose,
    base_url: Option<String>,
    kind: ProviderKind,
) -> (String, Option<Secret>, bool) {
    let section = match purpose {
        Purpose::Chat | Purpose::Vision => cfg.llm.base_url.clone(),
        Purpose::Image => cfg.image.base_url.clone(),
        Purpose::Embedding => cfg.embedding.base_url.clone(),
        Purpose::Speech | Purpose::Transcription => cfg.voice.speech_base_url.clone(),
    };
    let own = base_url
        .filter(|b| !b.trim().is_empty())
        .or(Some(section).filter(|b| !b.trim().is_empty()));
    let llm_like = matches!(purpose, Purpose::Chat | Purpose::Vision);
    let base = match own {
        Some(b) => b.trim().trim_end_matches('/').to_string(),
        None if llm_like => kind.default_base_url().trim_end_matches('/').to_string(),
        None => cfg.llm.effective_base_url(),
    };
    let own_key = match purpose {
        Purpose::Image => Some(IMAGE_SECRET_KEY),
        Purpose::Embedding => Some(EMBEDDING_SECRET_KEY),
        Purpose::Speech | Purpose::Transcription => Some(SPEECH_SECRET_KEY),
        Purpose::Chat | Purpose::Vision => None,
    };
    let llm_key = kind.secret_key();
    let key = own_key
        .into_iter()
        .chain(std::iter::once(llm_key.as_str()))
        .find_map(|k| ctx.secrets.get(k).ok().flatten().filter(|s| !s.is_empty()));
    let anthropic = llm_like && kind == ProviderKind::Anthropic;
    (base, key, anthropic)
}

/// Model lists by provider address, with the time they were fetched.
type Cache = Mutex<HashMap<String, (Instant, Vec<ModelInfo>)>>;

fn cache() -> &'static Cache {
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Models of the provider that suit `purpose` (`chat`, `vision`, `image`,
/// `embedding`, `tts`, `stt`). `base_url` and `kind` come from the form being
/// edited, so the list follows unsaved changes; the stored key is used.
#[tauri::command]
pub async fn list_models(
    ctx: tauri::State<'_, Arc<Ctx>>,
    purpose: String,
    base_url: Option<String>,
    kind: Option<String>,
) -> Result<ModelList, String> {
    let purpose = Purpose::parse(&purpose).ok_or("неизвестное назначение модели")?;
    let cfg = ctx.config.lock().clone();
    let kind = kind
        .and_then(|k| serde_json::from_value::<ProviderKind>(serde_json::Value::String(k)).ok())
        .unwrap_or(cfg.llm.kind);
    let (base, key, anthropic) = endpoint(&cfg, &ctx, purpose, base_url, kind);
    let cached = cache()
        .lock()
        .get(&base)
        .filter(|(at, _)| at.elapsed() < CACHE_FOR)
        .map(|(_, list)| list.clone());
    let all = match cached {
        Some(list) => list,
        None => {
            let list = catalog::fetch_models(&base, key.as_ref(), anthropic).await?;
            cache()
                .lock()
                .insert(base.clone(), (Instant::now(), list.clone()));
            list
        }
    };
    let fit: Vec<ModelInfo> = all
        .iter()
        .filter(|m| catalog::fits(m, purpose))
        .cloned()
        .collect();
    let filtered = !fit.is_empty();
    Ok(ModelList {
        models: if filtered { fit } else { all },
        currency: catalog::currency(&base),
        filtered,
    })
}

/// Voices for a speech model: the provider's list when it has one, else the
/// voices documented for the model family.
#[tauri::command]
pub async fn list_voices(
    ctx: tauri::State<'_, Arc<Ctx>>,
    model: String,
    base_url: Option<String>,
) -> Result<VoiceList, String> {
    let cfg = ctx.config.lock().clone();
    let (base, key, _) = endpoint(&cfg, &ctx, Purpose::Speech, base_url, cfg.llm.kind);
    let voices = catalog::fetch_voices(&base, key.as_ref(), model.trim()).await;
    if !voices.is_empty() {
        return Ok(VoiceList {
            voices,
            source: "provider",
        });
    }
    Ok(match catalog::known_voices(&model) {
        Some(list) => VoiceList {
            voices: list.iter().map(|v| v.to_string()).collect(),
            source: "known",
        },
        None => VoiceList {
            voices: Vec::new(),
            source: "none",
        },
    })
}
