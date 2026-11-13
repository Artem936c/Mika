//! UI commands for the pages added after the first release: tools, island,
//! memory, reminders, extensions.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mika_core::config::{
    McpServerConfig, TtsEngine, EMBEDDING_SECRET_KEY, IMAGE_SECRET_KEY, SPEECH_SECRET_KEY,
    TELEGRAM_SECRET_KEY,
};
use mika_core::memory::{Embedder, Fact};
use mika_core::schedule::{Reminder, ReminderKind};
use mika_core::secrets::Secret;
use parking_lot::Mutex;
use tauri::AppHandle;

use crate::app::Ctx;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

fn has_secret(ctx: &Ctx, key: &str) -> bool {
    matches!(ctx.secrets.get(key), Ok(Some(s)) if !s.is_empty())
}

/// `None` keeps the stored key, an empty string deletes it.
fn store_secret(ctx: &Ctx, key: &str, value: Option<String>) -> Result<(), String> {
    match value {
        Some(v) => ctx
            .secrets
            .set(key, &Secret::new(v.trim()))
            .map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

// ---- tools ----

#[derive(serde::Serialize)]
pub struct ToolGroup {
    id: &'static str,
    enabled: bool,
}

#[derive(serde::Serialize)]
pub struct ToolSettings {
    web_enabled: bool,
    screen_enabled: bool,
    news_feed: String,
    groups: Vec<ToolGroup>,
    tool_count: usize,
    notifications: mika_core::config::NotificationsConfig,
    briefing_enabled: bool,
    briefing_time: String,
}

#[tauri::command]
pub fn get_tool_settings(ctx: CtxState<'_>) -> ToolSettings {
    let c = ctx.config.lock();
    ToolSettings {
        web_enabled: c.tools.web_enabled,
        screen_enabled: c.privacy.screen_enabled,
        news_feed: c.tools.news_feed.clone(),
        notifications: c.notifications.clone(),
        briefing_enabled: c.briefing.enabled,
        briefing_time: c.briefing.time.clone(),
        groups: mika_tools::GROUPS
            .iter()
            .map(|g| ToolGroup {
                id: g,
                enabled: !c.tools.disabled_groups.iter().any(|d| d == g),
            })
            .collect(),
        tool_count: ctx.session.agent.registry().len(),
    }
}

#[tauri::command]
pub fn set_tool_group(ctx: CtxState<'_>, group: String, enabled: bool) -> Result<usize, String> {
    if !mika_tools::GROUPS.contains(&group.as_str()) {
        return Err(format!("unknown tool group {group}"));
    }
    {
        let mut c = ctx.config.lock();
        c.tools.disabled_groups.retain(|g| g != &group);
        if !enabled {
            c.tools.disabled_groups.push(group);
        }
    }
    ctx.save_config()?;
    ctx.rebuild_registry();
    Ok(ctx.session.agent.registry().len())
}

#[tauri::command]
pub fn set_screen_enabled(app: AppHandle, ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.set_screen_enabled(value)?;
    crate::bridge::settings_changed(&app);
    Ok(())
}

/// Notifications of other programs: on or off, which programs, aloud or not.
#[tauri::command]
pub fn set_notifications(
    ctx: CtxState<'_>,
    enabled: bool,
    only_messengers: bool,
    read_aloud: bool,
) -> Result<(), String> {
    {
        let mut c = ctx.config.lock();
        c.notifications.enabled = enabled;
        c.notifications.only_messengers = only_messengers;
        c.notifications.read_aloud = read_aloud;
    }
    if !enabled {
        // Without notifications nobody would tell her a watched person wrote.
        ctx.tool_env().shared.watches.stop_all();
    }
    ctx.save_config()
}

/// The morning summary: on or off and its time, `HH:MM`.
#[tauri::command]
pub fn set_briefing(ctx: CtxState<'_>, enabled: bool, time: String) -> Result<(), String> {
    let time = time.trim().to_string();
    if chrono::NaiveTime::parse_from_str(&time, "%H:%M").is_err() {
        return Err(crate::strings::tr("error.briefing_time"));
    }
    {
        let mut c = ctx.config.lock();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        // Turned on after its time today: the summary waits for tomorrow.
        if enabled && !c.briefing.enabled {
            let past = chrono::Local::now().time()
                >= chrono::NaiveTime::parse_from_str(&time, "%H:%M").unwrap_or_default();
            if past {
                c.briefing.last_date = today;
            }
        }
        c.briefing.enabled = enabled;
        c.briefing.time = time;
    }
    ctx.save_config()
}

#[tauri::command]
pub fn set_news_feed(ctx: CtxState<'_>, value: String) -> Result<(), String> {
    let value = value.trim().to_string();
    if !value.is_empty() && !value.starts_with("http://") && !value.starts_with("https://") {
        return Err(crate::strings::tr("error.feed_url"));
    }
    ctx.config.lock().tools.news_feed = value.clone();
    *ctx.news_feed.write() = value;
    ctx.save_config()
}

// ---- island ----

#[derive(serde::Serialize)]
pub struct IslandSettings {
    city: String,
    hide_in_games: bool,
    monitor: String,
    monitors: Vec<String>,
}

#[tauri::command]
pub fn get_island_settings(window: tauri::WebviewWindow, ctx: CtxState<'_>) -> IslandSettings {
    let c = ctx.config.lock();
    IslandSettings {
        city: c.island.city.clone(),
        hide_in_games: c.island.hide_in_games,
        monitor: c.window.monitor.clone().unwrap_or_default(),
        monitors: crate::window::monitor_names(&window),
    }
}

#[tauri::command]
pub fn set_island_settings(
    ctx: CtxState<'_>,
    city: String,
    hide_in_games: bool,
) -> Result<(), String> {
    {
        let mut c = ctx.config.lock();
        c.island.city = city.trim().chars().take(60).collect();
        c.island.hide_in_games = hide_in_games;
    }
    *IDLE_WEATHER.lock() = None;
    ctx.save_config()
}

/// Moves the island to another monitor; an empty name means the primary one.
#[tauri::command]
pub fn set_island_monitor(app: AppHandle, ctx: CtxState<'_>, name: String) -> Result<(), String> {
    let name = name.trim().to_string();
    ctx.config.lock().window.monitor = (!name.is_empty()).then(|| name.clone());
    ctx.save_config()?;
    crate::window::move_island(&app, &name).map_err(|e| e.to_string())
}

#[derive(serde::Serialize, Clone)]
pub struct IdleWeather {
    temp: i64,
    text: String,
}

#[derive(serde::Serialize)]
pub struct IdleReminder {
    due_at: i64,
    text: String,
}

#[derive(serde::Serialize)]
pub struct IdleInfo {
    weather: Option<IdleWeather>,
    reminder: Option<IdleReminder>,
    /// What plays now, when something does.
    track: Option<IdleTrack>,
}

#[derive(serde::Serialize)]
pub struct IdleTrack {
    title: String,
    artist: String,
}

/// City, when it was asked, and the answer. A failed request is cached too,
/// so a wrong city name does not cause a request every minute.
type WeatherCache = Option<(String, Instant, Option<IdleWeather>)>;
static IDLE_WEATHER: Mutex<WeatherCache> = Mutex::new(None);
const WEATHER_TTL: Duration = Duration::from_secs(30 * 60);

/// What the idle island shows next to the name: weather and the nearest reminder.
#[tauri::command]
pub async fn idle_info(ctx: tauri::State<'_, Arc<Ctx>>) -> Result<IdleInfo, String> {
    let reminder = ctx
        .reminders
        .list()
        .ok()
        .and_then(|l| l.into_iter().find(|r| r.kind == ReminderKind::Remind))
        .map(|r| IdleReminder {
            due_at: r.due_at,
            text: r.text,
        });
    let media = ctx.media();
    let track = tauri::async_runtime::spawn_blocking(move || media.now_playing())
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
        .filter(|t| t.playing)
        .map(|t| IdleTrack {
            title: t.title,
            artist: t.artist,
        });
    let (city, web) = {
        let c = ctx.config.lock();
        (c.island.city.clone(), c.tools.web_enabled)
    };
    if city.is_empty() || !web {
        return Ok(IdleInfo {
            weather: None,
            reminder,
            track,
        });
    }
    let cached = IDLE_WEATHER
        .lock()
        .as_ref()
        .filter(|(c, at, _)| c == &city && at.elapsed() < WEATHER_TTL)
        .map(|(_, _, w)| w.clone());
    let weather = match cached {
        Some(w) => w,
        None => {
            let http = ctx.http();
            let fresh = match mika_tools::web::brief_weather(http.as_ref(), &city).await {
                Ok((temp, text)) => Some(IdleWeather { temp, text }),
                Err(e) => {
                    log::warn!("idle weather failed: {e}");
                    None
                }
            };
            *IDLE_WEATHER.lock() = Some((city, Instant::now(), fresh.clone()));
            fresh
        }
    };
    Ok(IdleInfo {
        weather,
        reminder,
        track,
    })
}

// ---- memory ----

#[derive(serde::Serialize)]
pub struct MemoryView {
    facts: Vec<Fact>,
    archived_messages: usize,
    embedding_base_url: String,
    embedding_model: String,
    has_embedding_key: bool,
    llm_base_url: String,
}

#[tauri::command]
pub fn get_memory(ctx: CtxState<'_>) -> Result<MemoryView, String> {
    let c = ctx.config.lock();
    Ok(MemoryView {
        facts: ctx.memory.facts.list().map_err(|e| e.to_string())?,
        archived_messages: ctx.db.archive_size().map_err(|e| e.to_string())?,
        embedding_base_url: c.embedding.base_url.clone(),
        embedding_model: c.embedding.model.clone(),
        has_embedding_key: has_secret(&ctx, EMBEDDING_SECRET_KEY),
        llm_base_url: c.llm.effective_base_url(),
    })
}

#[tauri::command]
pub async fn add_fact(ctx: tauri::State<'_, Arc<Ctx>>, text: String) -> Result<i64, String> {
    if text.trim().is_empty() {
        return Err(crate::strings::tr("error.fact_empty"));
    }
    ctx.memory.save(&text).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_fact(ctx: CtxState<'_>, id: i64) -> Result<bool, String> {
    ctx.memory.facts.remove(id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_facts(ctx: CtxState<'_>) -> Result<usize, String> {
    ctx.memory.facts.clear().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_archive(ctx: CtxState<'_>) -> Result<usize, String> {
    ctx.db.clear_archive().map_err(|e| e.to_string())
}

/// A new model makes the stored vectors useless: they are dropped and made again on demand.
#[tauri::command]
pub fn save_embedding_settings(
    ctx: CtxState<'_>,
    base_url: String,
    model: String,
    api_key: Option<String>,
) -> Result<(), String> {
    store_secret(&ctx, EMBEDDING_SECRET_KEY, api_key)?;
    let changed = {
        let mut c = ctx.config.lock();
        let model = model.trim().to_string();
        let changed = c.embedding.model != model;
        c.embedding.base_url = base_url.trim().to_string();
        c.embedding.model = model;
        changed
    };
    if changed {
        ctx.memory.clear_vectors().map_err(|e| e.to_string())?;
    }
    ctx.save_config()
}

/// Length of the vector the embedding model returns for one word.
#[tauri::command]
pub async fn test_embedding(ctx: tauri::State<'_, Arc<Ctx>>) -> Result<usize, String> {
    let live = mika_providers::live::Live::new(ctx.config.clone(), ctx.secrets.clone());
    let v = live.embed(&["проверка".to_string()]).await?;
    Ok(v.first().map_or(0, Vec::len))
}

// ---- reminders ----

#[tauri::command]
pub fn list_reminders(ctx: CtxState<'_>) -> Result<Vec<Reminder>, String> {
    ctx.reminders.list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cancel_reminder(ctx: CtxState<'_>, id: i64) -> Result<bool, String> {
    ctx.reminders.cancel(id).map_err(|e| e.to_string())
}

// ---- user commands ----

#[tauri::command]
pub fn list_commands(ctx: CtxState<'_>) -> Result<Vec<mika_core::commands::Command>, String> {
    ctx.commands.list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_command(ctx: CtxState<'_>, name: String, prompt: String) -> Result<(), String> {
    ctx.commands.save(&name, &prompt).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_command(ctx: CtxState<'_>, name: String) -> Result<bool, String> {
    ctx.commands.delete(&name).map_err(|e| e.to_string())
}

// ---- MCP ----

#[derive(serde::Serialize)]
pub struct McpServerView {
    name: String,
    command: String,
    args: Vec<String>,
    url: String,
    has_token: bool,
    enabled: bool,
    /// Tools the server gave at the last start.
    tools: Option<usize>,
    error: Option<String>,
}

#[tauri::command]
pub fn list_mcp(ctx: CtxState<'_>) -> Vec<McpServerView> {
    let report = ctx.mcp_report.lock().clone();
    ctx.config
        .lock()
        .mcp
        .servers
        .iter()
        .map(|s| {
            let state = report.iter().find(|(n, _)| n == &s.name).map(|(_, r)| r);
            McpServerView {
                name: s.name.clone(),
                command: s.command.clone(),
                args: s.args.clone(),
                url: s.url.clone(),
                has_token: has_secret(&ctx, &mika_core::config::mcp_token_key(&s.name)),
                enabled: s.enabled,
                tools: state.and_then(|r| r.as_ref().ok().copied()),
                error: state.and_then(|r| r.as_ref().err().cloned()),
            }
        })
        .collect()
}

/// Splits a command line into arguments; double quotes keep spaces together.
pub fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Adds a server or replaces the one with the same name, then restarts the servers.
#[tauri::command]
pub async fn save_mcp_server(
    ctx: tauri::State<'_, Arc<Ctx>>,
    name: String,
    command_line: String,
    enabled: bool,
    token: Option<String>,
) -> Result<(), String> {
    store_secret(&ctx, &mika_core::config::mcp_token_key(name.trim()), token)?;
    let name = name.trim().to_string();
    let line = command_line.trim();
    let mut parts = split_args(line);
    if name.is_empty() || parts.is_empty() {
        return Err(crate::strings::tr("error.mcp_empty"));
    }
    // An address instead of a command: the server is reached over HTTP.
    let server = if line.starts_with("http://") || line.starts_with("https://") {
        McpServerConfig {
            name: name.clone(),
            url: line.to_string(),
            enabled,
            ..Default::default()
        }
    } else {
        McpServerConfig {
            name: name.clone(),
            command: parts.remove(0),
            args: parts,
            enabled,
            url: String::new(),
        }
    };
    {
        let mut c = ctx.config.lock();
        match c.mcp.servers.iter_mut().find(|s| s.name == name) {
            Some(existing) => *existing = server,
            None => c.mcp.servers.push(server),
        }
    }
    ctx.save_config()?;
    ctx.reconnect_mcp().await;
    Ok(())
}

#[tauri::command]
pub async fn remove_mcp_server(
    ctx: tauri::State<'_, Arc<Ctx>>,
    name: String,
) -> Result<(), String> {
    let _ = ctx.secrets.delete(&mika_core::config::mcp_token_key(&name));
    ctx.config.lock().mcp.servers.retain(|s| s.name != name);
    ctx.save_config()?;
    ctx.reconnect_mcp().await;
    Ok(())
}

// ---- Telegram ----

#[derive(serde::Serialize)]
pub struct TelegramView {
    enabled: bool,
    user_id: i64,
    has_token: bool,
    /// off, on, no_token, no_owner.
    status: String,
}

#[tauri::command]
pub fn get_telegram(ctx: CtxState<'_>) -> TelegramView {
    let c = ctx.config.lock().telegram.clone();
    TelegramView {
        enabled: c.enabled,
        user_id: c.user_id,
        has_token: has_secret(&ctx, TELEGRAM_SECRET_KEY),
        status: ctx.telegram_status.lock().clone(),
    }
}

#[tauri::command]
pub fn save_telegram(
    ctx: CtxState<'_>,
    enabled: bool,
    user_id: i64,
    token: Option<String>,
) -> Result<String, String> {
    store_secret(&ctx, TELEGRAM_SECRET_KEY, token)?;
    {
        let mut c = ctx.config.lock();
        c.telegram.enabled = enabled;
        c.telegram.user_id = user_id.max(0);
    }
    ctx.save_config()?;
    let ctx: &Arc<Ctx> = &ctx;
    ctx.restart_telegram();
    Ok(ctx.telegram_status.lock().clone())
}

/// Name of the bot behind the stored token.
#[tauri::command]
pub async fn test_telegram(ctx: tauri::State<'_, Arc<Ctx>>) -> Result<String, String> {
    let token = ctx
        .secrets
        .get(TELEGRAM_SECRET_KEY)
        .ok()
        .flatten()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| crate::strings::tr("error.telegram_token"))?;
    mika_providers::telegram::HttpTelegram::new(token)
        .me()
        .await
}

// ---- pictures and vision ----

#[derive(serde::Serialize)]
pub struct MediaProviders {
    vision_model: String,
    image_base_url: String,
    image_model: String,
    has_image_key: bool,
}

#[tauri::command]
pub fn get_media_providers(ctx: CtxState<'_>) -> MediaProviders {
    let c = ctx.config.lock();
    MediaProviders {
        vision_model: c.llm.vision_model.clone(),
        image_base_url: c.image.base_url.clone(),
        image_model: c.image.model.clone(),
        has_image_key: has_secret(&ctx, IMAGE_SECRET_KEY),
    }
}

#[tauri::command]
pub fn save_media_providers(
    ctx: CtxState<'_>,
    vision_model: String,
    image_base_url: String,
    image_model: String,
    image_key: Option<String>,
) -> Result<(), String> {
    store_secret(&ctx, IMAGE_SECRET_KEY, image_key)?;
    {
        let mut c = ctx.config.lock();
        c.llm.vision_model = vision_model.trim().to_string();
        c.image.base_url = image_base_url.trim().to_string();
        c.image.model = image_model.trim().to_string();
    }
    ctx.save_config()
}

/// True for a picture the assistant itself saved: only those are shown in the chat.
pub fn is_own_picture(path: &std::path::Path, user_profile: &str) -> bool {
    let pictures = std::path::Path::new(user_profile).join("Pictures");
    let allowed = [pictures.join("Mika"), pictures.join("Screenshots")];
    let is_image = path.extension().is_some_and(|e| {
        ["png", "jpg", "jpeg", "webp"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    });
    let has_dots = path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir));
    is_image && !has_dots && allowed.iter().any(|dir| path.starts_with(dir))
}

/// A saved picture as a data URL for the chat card.
#[tauri::command]
pub fn load_picture(ctx: CtxState<'_>, path: String) -> Result<String, String> {
    use base64::Engine;
    let p = std::path::PathBuf::from(&path);
    if !is_own_picture(&p, ctx.user_profile()) {
        return Err("not a picture saved by the assistant".into());
    }
    let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
    if bytes.len() > 20 * 1024 * 1024 {
        return Err("picture is too large to show".into());
    }
    let mime = match mika_tools::image::image_extension(&bytes) {
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    };
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

#[tauri::command]
pub fn open_picture(ctx: CtxState<'_>, path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if !is_own_picture(&p, ctx.user_profile()) {
        return Err("not a picture saved by the assistant".into());
    }
    std::process::Command::new("explorer.exe")
        .arg(&p)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---- voice ----

#[derive(serde::Serialize)]
pub struct VoiceSettings {
    speak_replies: bool,
    voice: String,
    rate: f32,
    voices: Vec<mika_audio::VoiceInfo>,
    has_russian: bool,
    tts: TtsEngine,
    tts_model: String,
    tts_voice: String,
    tts_cache: bool,
    tts_cache_bytes: u64,
    stt_model: String,
    stt_language: String,
    speech_base_url: String,
    has_speech_key: bool,
    llm_base_url: String,
    microphone_enabled: bool,
    microphone_found: bool,
    listen_shortcut: String,
    input_device: String,
    input_devices: Vec<String>,
    wake_enabled: bool,
    wake_phrase: String,
    /// `off`, `on`, `starting`, or why the wake phrase cannot listen.
    wake_status: String,
    /// Vosk is downloaded: it hears the name instead of Windows.
    vosk_installed: bool,
    vosk_downloading: bool,
    vosk_bytes: u64,
    follow_up: bool,
    duck_media: bool,
}

/// Async: listing voices waits for Windows and must not block the UI thread.
#[tauri::command]
pub async fn get_voice_settings(ctx: tauri::State<'_, Arc<Ctx>>) -> Result<VoiceSettings, String> {
    let voice = ctx.voice.clone();
    let voices = tauri::async_runtime::spawn_blocking(move || voice.voices())
        .await
        .map_err(|e| e.to_string())?;
    let mic = ctx.mic.clone();
    let (microphone_found, input_devices) = tauri::async_runtime::spawn_blocking(move || {
        (mic.available(), mika_audio::SystemMicrophone::devices())
    })
    .await
    .unwrap_or_default();
    let cache = ctx.speech_cache.clone();
    let (tts_cache_bytes, vosk_bytes) =
        tauri::async_runtime::spawn_blocking(move || (cache.size(), crate::vosk::size()))
            .await
            .unwrap_or_default();
    let (c, microphone_enabled, llm_base_url) = {
        let cfg = ctx.config.lock();
        (
            cfg.voice.clone(),
            cfg.privacy.microphone_enabled,
            cfg.llm.effective_base_url(),
        )
    };
    Ok(VoiceSettings {
        speak_replies: c.speak_replies,
        voice: c.voice,
        rate: c.rate,
        has_russian: voices
            .iter()
            .any(|v| v.language.to_lowercase().starts_with("ru")),
        voices,
        tts: c.tts,
        tts_model: c.tts_model,
        tts_voice: c.tts_voice,
        tts_cache: c.tts_cache,
        tts_cache_bytes,
        stt_model: c.stt_model,
        stt_language: c.stt_language,
        speech_base_url: c.speech_base_url,
        has_speech_key: has_secret(&ctx, SPEECH_SECRET_KEY),
        llm_base_url,
        microphone_enabled,
        microphone_found,
        listen_shortcut: crate::tray::listen_shortcut_label(),
        input_device: c.input_device,
        input_devices,
        wake_enabled: c.wake_enabled,
        wake_phrase: ctx.config.lock().assistant.wake_phrase.clone(),
        wake_status: ctx.wake_status.lock().clone(),
        vosk_installed: crate::vosk::installed(),
        vosk_downloading: crate::vosk::downloading(),
        vosk_bytes,
        follow_up: c.follow_up,
        duck_media: c.duck_media,
    })
}

/// Other programs quieter while she listens and speaks, or not.
#[tauri::command]
pub fn set_duck_media(ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.config.lock().voice.duck_media = value;
    if !value {
        ctx.restore_media();
    }
    ctx.save_config()
}

/// Listen again after an answer to a spoken question, or not.
#[tauri::command]
pub fn set_follow_up(ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.config.lock().voice.follow_up = value;
    if !value {
        ctx.end_voice_dialog();
    }
    ctx.save_config()
}

/// Downloads Vosk on the user's press; the wake phrase switches to it.
/// Progress goes to the window as `vosk-progress` with the share done.
#[tauri::command]
pub async fn install_vosk(
    app: AppHandle,
    ctx: tauri::State<'_, Arc<Ctx>>,
) -> Result<String, String> {
    use tauri::Emitter;
    let last = std::sync::atomic::AtomicU8::new(u8::MAX);
    crate::vosk::install(move |share| {
        if last.swap(share, std::sync::atomic::Ordering::Relaxed) != share {
            let _ = app.emit("vosk-progress", share);
        }
    })
    .await?;
    let ctx: &Arc<Ctx> = &ctx;
    ctx.restart_wake();
    let status = ctx.wake_status.lock().clone();
    Ok(status)
}

/// Deletes Vosk; the recognizer of Windows hears the name after that.
#[tauri::command]
pub async fn remove_vosk(ctx: tauri::State<'_, Arc<Ctx>>) -> Result<String, String> {
    let ctx: Arc<Ctx> = ctx.inner().clone();
    let stopping = ctx.clone();
    // The listener holds the library open: stop it before deleting.
    let removed = tauri::async_runtime::spawn_blocking(move || {
        stopping.stop_wake();
        crate::vosk::remove()
    })
    .await
    .map_err(|e| e.to_string())?;
    ctx.restart_wake();
    removed?;
    let status = ctx.wake_status.lock().clone();
    Ok(status)
}

/// The wake phrase on or off, and the name it listens for.
#[tauri::command]
pub fn set_wake(ctx: CtxState<'_>, enabled: bool, phrase: String) -> Result<String, String> {
    let phrase = phrase.trim().to_string();
    if enabled && phrase.chars().count() < 2 {
        return Err(crate::strings::tr("error.wake_short"));
    }
    {
        let mut c = ctx.config.lock();
        c.voice.wake_enabled = enabled;
        if !phrase.is_empty() {
            c.assistant.wake_phrase = phrase.chars().take(40).collect();
        }
    }
    ctx.save_config()?;
    let ctx: &Arc<Ctx> = &ctx;
    ctx.restart_wake();
    Ok(ctx.wake_status.lock().clone())
}

/// Models and address for speech through a provider.
/// `api_key`: `None` keeps the stored key, an empty string deletes it.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn save_speech_settings(
    ctx: CtxState<'_>,
    tts: TtsEngine,
    tts_model: String,
    tts_voice: String,
    stt_model: String,
    stt_language: String,
    base_url: String,
    input_device: String,
    api_key: Option<String>,
) -> Result<(), String> {
    store_secret(&ctx, SPEECH_SECRET_KEY, api_key)?;
    {
        let mut c = ctx.config.lock();
        c.voice.tts = tts;
        c.voice.tts_model = tts_model.trim().to_string();
        c.voice.tts_voice = tts_voice.trim().to_string();
        c.voice.stt_model = stt_model.trim().to_string();
        c.voice.stt_language = stt_language.trim().chars().take(8).collect();
        c.voice.speech_base_url = base_url.trim().to_string();
        c.voice.input_device = input_device.trim().to_string();
    }
    ctx.set_input_device(&input_device);
    ctx.save_config()?;
    ctx.apply_voice_engine();
    Ok(())
}

/// Keep provider phrases on disk, or order every phrase anew.
#[tauri::command]
pub fn set_tts_cache(ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.config.lock().voice.tts_cache = value;
    ctx.save_config()
}

/// Forgets every kept phrase; answers the bytes left.
#[tauri::command]
pub async fn clear_speech_cache(ctx: tauri::State<'_, Arc<Ctx>>) -> Result<u64, String> {
    let cache = ctx.speech_cache.clone();
    tauri::async_runtime::spawn_blocking(move || {
        cache.clear();
        cache.size()
    })
    .await
    .map_err(|e| e.to_string())
}

/// The user's switch: without it the microphone is never opened.
#[tauri::command]
pub fn set_microphone_enabled(
    app: AppHandle,
    ctx: CtxState<'_>,
    value: bool,
) -> Result<(), String> {
    ctx.config.lock().privacy.microphone_enabled = value;
    if !value {
        ctx.cancel_listening();
    }
    ctx.save_config()?;
    let restart: &Arc<Ctx> = &ctx;
    restart.restart_wake();
    crate::bridge::settings_changed(&app);
    Ok(())
}

/// Starts recording a phrase for the assistant, or finishes the running recording.
#[tauri::command]
pub async fn voice_listen(app: AppHandle, ctx: tauri::State<'_, Arc<Ctx>>) -> Result<(), String> {
    crate::window::summon(&app);
    ctx.listen_and_submit().await;
    Ok(())
}

#[tauri::command]
pub fn voice_cancel(ctx: CtxState<'_>) {
    ctx.cancel_listening();
}

/// Records a phrase and returns what was recognised, without sending it to the assistant.
#[tauri::command]
pub async fn test_microphone(ctx: tauri::State<'_, Arc<Ctx>>) -> Result<String, String> {
    match ctx.hear().await? {
        crate::app::Heard::Text(text) => Ok(text),
        crate::app::Heard::Finished => Ok(String::new()),
    }
}

#[tauri::command]
pub fn set_voice(ctx: CtxState<'_>, voice: String, rate: f32) -> Result<(), String> {
    let rate = if rate.is_finite() {
        rate.clamp(0.5, 2.0)
    } else {
        1.0
    };
    {
        let mut c = ctx.config.lock();
        c.voice.voice = voice.trim().to_string();
        c.voice.rate = rate;
    }
    ctx.voice.configure(voice.trim(), rate);
    ctx.save_config()
}

/// Says a phrase now, whatever the "speak replies" switch says.
#[tauri::command]
pub fn test_voice(ctx: CtxState<'_>, text: String) {
    ctx.voice.stop();
    ctx.voice.speak(&text.chars().take(300).collect::<String>());
}

// ---- first run ----

#[tauri::command]
pub fn finish_onboarding(app: AppHandle, ctx: CtxState<'_>) -> Result<(), String> {
    ctx.config.lock().assistant.onboarded = true;
    ctx.save_config()?;
    crate::bridge::settings_changed(&app);
    Ok(())
}

// ---- updates ----

#[derive(serde::Serialize)]
pub struct UpdateView {
    current: &'static str,
    manifest_url: String,
    check_at_start: bool,
    available: Option<crate::update::Manifest>,
}

#[tauri::command]
pub fn get_update_info(ctx: CtxState<'_>) -> UpdateView {
    let c = ctx.config.lock().updates.clone();
    UpdateView {
        current: env!("CARGO_PKG_VERSION"),
        manifest_url: c.manifest_url,
        check_at_start: c.check_at_start,
        available: ctx.update.lock().clone(),
    }
}

#[tauri::command]
pub fn save_update_settings(
    ctx: CtxState<'_>,
    manifest_url: String,
    check_at_start: bool,
) -> Result<(), String> {
    let url = manifest_url.trim().to_string();
    if !url.is_empty() && !url.starts_with("https://") {
        return Err(crate::strings::tr("error.update_url"));
    }
    {
        let mut c = ctx.config.lock();
        c.updates.manifest_url = url;
        c.updates.check_at_start = check_at_start;
    }
    ctx.save_config()
}

#[tauri::command]
pub async fn check_update(
    ctx: tauri::State<'_, Arc<Ctx>>,
) -> Result<Option<crate::update::Manifest>, String> {
    ctx.check_update().await
}

/// Downloads the version found by the last check, checks its SHA-256, starts
/// the installer and quits. Runs only on the user's click.
#[tauri::command]
pub async fn install_update(app: AppHandle, ctx: tauri::State<'_, Arc<Ctx>>) -> Result<(), String> {
    let manifest = ctx
        .update
        .lock()
        .clone()
        .ok_or_else(|| crate::strings::tr("error.update_none"))?;
    let installer = crate::update::download(&manifest).await?;
    log::info!("installing version {}", manifest.version);
    ctx.session.stop();
    ctx.save_config()?;
    crate::update::run_installer(&installer)?;
    app.exit(0);
    Ok(())
}

// ---- report a problem ----

/// Version, settings without secrets and the tail of the log, for the user to copy.
/// Nothing is sent anywhere.
#[tauri::command]
pub fn problem_report(ctx: CtxState<'_>) -> String {
    let c = ctx.config.lock();
    let log =
        std::fs::read_to_string(mika_core::paths::logs_dir().join("mika.log")).unwrap_or_default();
    let tail: Vec<&str> = log.lines().rev().take(60).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    format!(
        "Mika {}\nWindows: {}\nProvider: {} · {} · {}\nTools: {} (off: {})\nWeb: {} · Screen: {} · Telegram: {} · MCP servers: {}\nEmbeddings: {}\n\n--- log ---\n{}",
        env!("CARGO_PKG_VERSION"),
        std::env::var("OS").unwrap_or_default(),
        c.llm.kind.as_str(),
        c.llm.effective_base_url(),
        c.llm.model,
        ctx.session.agent.registry().len(),
        c.tools.disabled_groups.join(", "),
        c.tools.web_enabled,
        c.privacy.screen_enabled,
        ctx.telegram_status.lock(),
        c.mcp.servers.len(),
        if c.embedding.model.is_empty() { "off" } else { c.embedding.model.as_str() },
        tail.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn args_split_on_spaces_outside_quotes() {
        assert_eq!(
            split_args(r#"npx -y "@scope/server name" C:\data"#),
            vec!["npx", "-y", "@scope/server name", r"C:\data"]
        );
        assert_eq!(split_args("  one  "), vec!["one"]);
        assert_eq!(split_args(r#"a "" b"#), vec!["a", "", "b"]);
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn only_pictures_from_the_assistant_folders_are_shown() {
        let profile = r"C:\Users\me";
        assert!(is_own_picture(
            Path::new(r"C:\Users\me\Pictures\Mika\cat.png"),
            profile
        ));
        assert!(is_own_picture(
            Path::new(r"C:\Users\me\Pictures\Screenshots\Mika 1.PNG"),
            profile
        ));
        for bad in [
            r"C:\Users\me\Pictures\cat.png",
            r"C:\Users\me\Pictures\Mika\..\..\secret.png",
            r"C:\Users\me\Pictures\Mika\notes.txt",
            r"C:\Windows\system.png",
            r"C:\Users\other\Pictures\Mika\cat.png",
        ] {
            assert!(!is_own_picture(Path::new(bad), profile), "{bad}");
        }
    }
}
