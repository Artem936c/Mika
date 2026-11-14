//! UI commands for the pages added after the first release: tools, island,
//! memory, reminders, extensions.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mika_core::config::{
    expressive_model, McpServerConfig, TtsEngine, EMBEDDING_SECRET_KEY, IMAGE_SECRET_KEY,
    SPEECH_SECRET_KEY, TELEGRAM_SECRET_KEY,
};
use mika_core::marks::{strip_marks, tokens, Token};
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

// ---- rhythm of the day ----

#[derive(serde::Serialize)]
pub struct RhythmSettings {
    rhythm: mika_core::config::RhythmConfig,
    tracking: mika_core::config::TrackingConfig,
    clip_count: usize,
    /// Seconds of focus left, when it runs.
    focus_left_s: Option<i64>,
    calendar_enabled: bool,
    remind_min: u32,
    has_calendar_url: bool,
    calendar_events: usize,
    /// `HH:MM` of the last good read.
    calendar_read_at: Option<String>,
    calendar_error: Option<String>,
    docs_folders: Vec<String>,
    docs: mika_tools::docs::IndexStatus,
}

/// Asked every few seconds while the page is open. Off the main thread: the
/// index of documents may hold the base, and the Credential Manager is read.
#[tauri::command]
pub async fn get_rhythm_settings(ctx: CtxState<'_>) -> Result<RhythmSettings, String> {
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || rhythm_settings(&ctx))
        .await
        .map_err(|e| e.to_string())
}

fn rhythm_settings(ctx: &Ctx) -> RhythmSettings {
    let c = ctx.config.lock().clone();
    let shared = &ctx.tool_env().shared;
    let (calendar_events, read, calendar_error) = shared.calendar.status();
    RhythmSettings {
        rhythm: c.rhythm,
        tracking: c.tracking,
        clip_count: shared.clips.len(),
        focus_left_s: shared.focus.left(chrono::Local::now().timestamp()),
        calendar_enabled: c.calendar.enabled,
        remind_min: c.calendar.remind_min,
        has_calendar_url: has_secret(ctx, mika_core::config::CALENDAR_SECRET_KEY),
        calendar_events,
        calendar_read_at: read
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()),
        calendar_error,
        // A repeat saved by an older version would break the list on the page.
        docs_folders: crate::rhythm::unique_folders(c.docs.folders),
        docs: shared.docs.status(),
    }
}

/// Breaks and her lines of her own.
#[tauri::command]
pub fn set_rhythm(
    ctx: CtxState<'_>,
    breaks: bool,
    break_every_min: u32,
    lively: bool,
) -> Result<(), String> {
    {
        let mut c = ctx.config.lock();
        c.rhythm.breaks = breaks;
        c.rhythm.break_every_min = break_every_min.clamp(15, 240);
        c.rhythm.lively = lively;
    }
    ctx.save_config()
}

/// Time in programs and copied texts. Turning the history off forgets it.
#[tauri::command]
pub fn set_tracking(ctx: CtxState<'_>, app_time: bool, clipboard: bool) -> Result<(), String> {
    {
        let mut c = ctx.config.lock();
        c.tracking.app_time = app_time;
        c.tracking.clipboard = clipboard;
    }
    let shared = &ctx.tool_env().shared;
    shared
        .app_time_on
        .store(app_time, std::sync::atomic::Ordering::Relaxed);
    shared
        .clipboard_on
        .store(clipboard, std::sync::atomic::Ordering::Relaxed);
    if !clipboard {
        shared.clips.clear();
    }
    ctx.save_config()
}

#[tauri::command]
pub fn clear_app_time(ctx: CtxState<'_>) -> Result<(), String> {
    mika_tools::usage::UsageStore(ctx.db.clone())
        .clear()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_clipboard_history(ctx: CtxState<'_>) {
    ctx.tool_env().shared.clips.clear();
}

#[tauri::command]
pub fn start_focus(ctx: CtxState<'_>, minutes: u64) {
    ctx.tool_env()
        .shared
        .focus
        .start(minutes, chrono::Local::now().timestamp());
}

/// Ends focus now; what came meanwhile is told at once.
#[tauri::command]
pub fn stop_focus(ctx: CtxState<'_>) {
    if let Some(held) = ctx.tool_env().shared.focus.stop() {
        ctx.focus_over(&held);
    }
}

/// The calendar: on or off, minutes before a meeting, and the link.
/// `url`: `None` keeps the stored link, an empty string deletes it.
#[tauri::command]
pub fn save_calendar(
    ctx: CtxState<'_>,
    enabled: bool,
    remind_min: u32,
    url: Option<String>,
) -> Result<(), String> {
    if let Some(u) = url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        if !u.starts_with("https://") && !u.starts_with("webcal://") {
            return Err(crate::strings::tr("error.calendar_url"));
        }
    }
    store_secret(&ctx, mika_core::config::CALENDAR_SECRET_KEY, url)?;
    {
        let mut c = ctx.config.lock();
        c.calendar.enabled = enabled;
        c.calendar.remind_min = remind_min.min(120);
    }
    ctx.save_config()?;
    ctx.calendar_wake.notify_one();
    Ok(())
}

/// Folders of documents, each once; a new one must exist. The index follows
/// at once. Off the main thread: asking a network drive that is away can
/// take a minute.
#[tauri::command]
pub async fn set_docs_folders(ctx: CtxState<'_>, folders: Vec<String>) -> Result<(), String> {
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || save_docs_folders(&ctx, folders))
        .await
        .map_err(|e| e.to_string())?
}

fn save_docs_folders(ctx: &Ctx, folders: Vec<String>) -> Result<(), String> {
    let folders = crate::rhythm::unique_folders(folders);
    let seen = ctx.config.lock().docs.folders.clone();
    // Not under the lock: a network folder may answer in a minute.
    if let Some(raw) =
        crate::rhythm::missing_folder(&folders, &seen, ctx.user_profile(), |p| p.is_dir())
    {
        return Err(crate::strings::tr("error.docs_folder").replace("{path}", &raw));
    }
    {
        // Another change may have been saved while the new folders were
        // checked: only this one is applied to it.
        let mut c = ctx.config.lock();
        c.docs.folders = crate::rhythm::merge_folders(&seen, &folders, &c.docs.folders);
    }
    ctx.save_config()?;
    ctx.docs_wake.notify_one();
    Ok(())
}

#[tauri::command]
pub fn reindex_docs(ctx: CtxState<'_>) {
    ctx.docs_wake.notify_one();
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

// ---- ffmpeg ----

/// Which FFmpeg the video and sound tools would run now, and the downloaded copy.
#[derive(serde::Serialize)]
pub struct FfmpegView {
    found: bool,
    /// `settings`, `path` or `downloaded`.
    source: Option<&'static str>,
    /// ffmpeg.exe that runs, or empty.
    path: String,
    version: String,
    /// The build is GPL: Mika still runs it only as a separate program.
    gpl: bool,
    setting: String,
    /// The saved path holds no ffmpeg.exe with ffprobe.exe next to it.
    setting_invalid: bool,
    downloaded: bool,
    downloading: bool,
    bytes: u64,
    download_mb: u32,
}

/// Looks on disk and asks `ffmpeg -version`, off the async threads.
async fn ffmpeg_view(ctx: &Ctx) -> Result<FfmpegView, String> {
    let setting = ctx.config.lock().tools.ffmpeg_path.clone();
    tauri::async_runtime::spawn_blocking(move || ffmpeg_view_now(setting))
        .await
        .map_err(|e| e.to_string())
}

fn ffmpeg_view_now(setting: String) -> FfmpegView {
    let status = mika_tools::ffmpeg::status(&setting);
    let version = status
        .paths
        .as_ref()
        .and_then(|p| match mika_tools::ffmpeg::version(p) {
            Ok(v) => Some(v),
            Err(e) => {
                log::warn!("ffmpeg -version failed: {e}");
                None
            }
        });
    FfmpegView {
        found: status.paths.is_some(),
        source: status.paths.as_ref().map(|p| p.source.as_str()),
        path: status
            .paths
            .as_ref()
            .map(|p| p.ffmpeg.display().to_string())
            .unwrap_or_default(),
        version: version
            .as_ref()
            .map(|v| v.version.clone())
            .unwrap_or_default(),
        gpl: version.is_some_and(|v| v.gpl),
        setting,
        setting_invalid: status.setting_invalid,
        downloaded: crate::ffmpeg_get::installed(),
        downloading: crate::ffmpeg_get::downloading(),
        bytes: crate::ffmpeg_get::size(),
        download_mb: crate::ffmpeg_get::DOWNLOAD_MB,
    }
}

#[tauri::command]
pub async fn get_ffmpeg_status(ctx: tauri::State<'_, Arc<Ctx>>) -> Result<FfmpegView, String> {
    ffmpeg_view(&ctx).await
}

/// Saves the path to the user's own FFmpeg. A path without ffmpeg.exe is kept
/// and shown as such: the tools search PATH and the downloaded copy meanwhile.
#[tauri::command]
pub async fn set_ffmpeg_path(
    ctx: tauri::State<'_, Arc<Ctx>>,
    value: String,
) -> Result<FfmpegView, String> {
    let value = value.trim().to_string();
    ctx.config.lock().tools.ffmpeg_path = value.clone();
    *ctx.ffmpeg_path.write() = value;
    ctx.save_config()?;
    ffmpeg_view(&ctx).await
}

/// Downloads FFmpeg on the user's press, in the settings or on the chat card.
/// Progress goes to the window as `ffmpeg-progress` with the share done.
#[tauri::command]
pub async fn install_ffmpeg(
    app: AppHandle,
    ctx: tauri::State<'_, Arc<Ctx>>,
) -> Result<FfmpegView, String> {
    use tauri::Emitter;
    let last = std::sync::atomic::AtomicU8::new(u8::MAX);
    crate::ffmpeg_get::install(move |share| {
        if last.swap(share, std::sync::atomic::Ordering::Relaxed) != share {
            let _ = app.emit("ffmpeg-progress", share);
        }
    })
    .await?;
    ffmpeg_view(&ctx).await
}

/// Deletes the copy Mika downloaded; the user's own FFmpeg stays.
#[tauri::command]
pub async fn remove_ffmpeg(ctx: tauri::State<'_, Arc<Ctx>>) -> Result<FfmpegView, String> {
    tauri::async_runtime::spawn_blocking(crate::ffmpeg_get::remove)
        .await
        .map_err(|e| e.to_string())??;
    ffmpeg_view(&ctx).await
}

/// Opens a file a video or sound tool wrote, or shows it in its folder. Other
/// paths are refused: the window cannot open arbitrary files through this.
#[tauri::command]
pub fn open_av_file(ctx: CtxState<'_>, path: String, reveal: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let p = std::path::PathBuf::from(&path);
    if !ctx.av_outputs.contains(&p) || !p.is_file() {
        return Err(crate::strings::tr("error.av_file"));
    }
    let mut explorer = std::process::Command::new("explorer.exe");
    if reveal {
        explorer.raw_arg(format!("/select,\"{}\"", p.display()));
    } else {
        explorer.arg(&p);
    }
    explorer.spawn().map(|_| ()).map_err(|e| e.to_string())
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
        *ctx.home_city.write() = c.island.city.clone();
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
    /// Voice messages are recognised and run as commands (decision 0029).
    voice_in: bool,
    /// A voice message is answered with a voice message too.
    voice_reply: bool,
    /// A recognition model is set.
    stt_ready: bool,
    /// The provider speaks and has a speech model: a voice reply can be made.
    tts_ready: bool,
    /// FFmpeg is found: sound that comes as WAV still becomes a voice message.
    ffmpeg_found: bool,
    /// Longest voice message taken, seconds.
    voice_max_s: u32,
}

/// The Telegram block of the settings as the window shows it.
fn telegram_view(
    config: &mika_core::config::Config,
    has_token: bool,
    status: String,
    ffmpeg_found: bool,
) -> TelegramView {
    TelegramView {
        enabled: config.telegram.enabled,
        user_id: config.telegram.user_id,
        has_token,
        status,
        voice_in: config.telegram.voice_in,
        voice_reply: config.telegram.voice_reply,
        stt_ready: !config.voice.stt_model.trim().is_empty(),
        tts_ready: config.voice.provider_speaks(),
        ffmpeg_found,
        voice_max_s: mika_providers::telegram::voice::VOICE_MAX_S,
    }
}

/// The view from what is saved now. Looks for FFmpeg on disk: file stats only.
fn telegram_view_now(ctx: &Ctx) -> TelegramView {
    let has_token = has_secret(ctx, TELEGRAM_SECRET_KEY);
    let status = ctx.telegram_status.lock().clone();
    let ffmpeg_setting = ctx.ffmpeg_path.read().clone();
    let ffmpeg_found = mika_tools::ffmpeg::locate(&ffmpeg_setting).is_some();
    let config = ctx.config.lock();
    telegram_view(&config, has_token, status, ffmpeg_found)
}

#[tauri::command]
pub fn get_telegram(ctx: CtxState<'_>) -> TelegramView {
    telegram_view_now(&ctx)
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

/// Voice messages in Telegram: taken or not, answered by voice or not
/// (decision 0029). The bridge reads both at every message, so it is not restarted.
#[tauri::command]
pub fn set_telegram_voice(
    ctx: CtxState<'_>,
    voice_in: bool,
    voice_reply: bool,
) -> Result<TelegramView, String> {
    {
        let mut c = ctx.config.lock();
        c.telegram.voice_in = voice_in;
        c.telegram.voice_reply = voice_reply;
    }
    ctx.save_config()?;
    Ok(telegram_view_now(&ctx))
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
    tts_style: String,
    tone_by_mood: bool,
    brief_long: bool,
    /// The switch of expressive speech (decision 0026).
    tts_expressive: bool,
    /// The saved speech model performs voice marks, like Drama 3.
    expressive_model: bool,
    /// The switch of the direction by her mood (decision 0029).
    mood_directions: bool,
}

/// A long reply retold briefly aloud, or read out whole.
#[tauri::command]
pub fn set_brief_speech(ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.config.lock().voice.brief_long = value;
    ctx.speaker.set_brief(value);
    ctx.save_config()
}

/// Replies marked up for a voice that performs the marks (decision 0026).
/// The agent reads the setting at every step, so nothing else is told.
#[tauri::command]
pub fn set_expressive_speech(ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.config.lock().voice.tts_expressive = value;
    ctx.save_config()
}

/// A phrase without marks of its own gets a direction by her mood
/// (decision 0029). The voice reads the setting at every sentence.
#[tauri::command]
pub fn set_mood_directions(ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.config.lock().voice.mood_directions = value;
    ctx.save_config()
}

/// The voice in words for the provider, and whether tone follows her mood.
#[tauri::command]
pub fn set_voice_style(ctx: CtxState<'_>, style: String, tone_by_mood: bool) -> Result<(), String> {
    {
        let mut c = ctx.config.lock();
        c.voice.tts_style = style.trim().chars().take(500).collect();
        c.voice.tone_by_mood = tone_by_mood;
    }
    ctx.save_config()?;
    ctx.apply_voice_engine();
    Ok(())
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
    let marks_model = expressive_model(&c.tts_model);
    Ok(VoiceSettings {
        speak_replies: c.speak_replies,
        voice: c.voice,
        rate: c.rate,
        has_russian: mika_audio::win::pick(&voices, "").is_some(),
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
        tts_style: c.tts_style,
        tone_by_mood: c.tone_by_mood,
        brief_long: c.brief_long,
        tts_expressive: c.tts_expressive,
        expressive_model: marks_model,
        mood_directions: c.mood_directions,
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
/// Answers whether the saved speech model performs voice marks (decision 0026).
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
) -> Result<bool, String> {
    store_secret(&ctx, SPEECH_SECRET_KEY, api_key)?;
    let marks_model = {
        let mut c = ctx.config.lock();
        c.voice.tts = tts;
        c.voice.tts_model = tts_model.trim().to_string();
        c.voice.tts_voice = tts_voice.trim().to_string();
        c.voice.stt_model = stt_model.trim().to_string();
        c.voice.stt_language = stt_language.trim().chars().take(8).collect();
        c.voice.speech_base_url = base_url.trim().to_string();
        c.voice.input_device = input_device.trim().to_string();
        expressive_model(&c.voice.tts_model)
    };
    ctx.set_input_device(&input_device);
    ctx.save_config()?;
    // The automatic Windows voice follows the speech language (decision 0035).
    crate::lang::pick_voice(&ctx);
    ctx.apply_voice_engine();
    Ok(marks_model)
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
    let expressive = ctx.config.lock().voice.expressive();
    ctx.voice.stop();
    match test_phrase(&text, expressive) {
        TestPhrase::Plain(text) => ctx.voice.speak_reply(&text),
        TestPhrase::Marked { marked, plain } => ctx.voice.speak_marked(&marked, &plain),
    }
}

/// What "Проверить голос" says.
#[derive(Debug, PartialEq)]
enum TestPhrase {
    /// Said as before decision 0026: the voice gets it as it is.
    Plain(String),
    /// With voice marks for a voice that performs them, and the same words
    /// without marks for any other (decision 0026).
    Marked { marked: String, plain: String },
}

/// The first 300 characters of `text`. With expressive speech on, its marks
/// go to the voice to be heard; a phrase with no words besides marks is said
/// as before. With the option off its marks are cut, or the voice would read
/// them aloud; a phrase without marks is said as before.
fn test_phrase(text: &str, expressive: bool) -> TestPhrase {
    let text: String = text.chars().take(300).collect();
    if expressive {
        let marked = mika_audio::speech::clean_marked_for_speech(&text);
        let plain = mika_audio::speech::clean_for_speech(&strip_marks(&text));
        if !marked.is_empty() && !plain.is_empty() {
            return TestPhrase::Marked { marked, plain };
        }
        return TestPhrase::Plain(text);
    }
    let marks = tokens(&text).iter().any(|t| !matches!(t, Token::Text(_)));
    TestPhrase::Plain(if marks { strip_marks(&text) } else { text })
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

    #[test]
    fn the_test_phrase_keeps_its_marks_only_with_expressive_speech() {
        let text = "[sighs, tired] Ну что ж. <whisper>Иди без меня.</whisper> Я догоню.";
        assert_eq!(
            test_phrase(text, true),
            TestPhrase::Marked {
                marked: text.into(),
                plain: "Ну что ж. Иди без меня. Я догоню.".into(),
            }
        );
        // With the option off the marks are cut, not read aloud.
        assert_eq!(
            test_phrase(text, false),
            TestPhrase::Plain("Ну что ж. Иди без меня. Я догоню.".into())
        );
        let typed = "Привет! Это Мика. Так я звучу.";
        assert_eq!(
            test_phrase(typed, true),
            TestPhrase::Marked {
                marked: typed.into(),
                plain: typed.into(),
            }
        );
        // A phrase without marks is said exactly as before.
        for phrase in [typed, "  Смотри [1] и a < b  "] {
            assert_eq!(test_phrase(phrase, false), TestPhrase::Plain(phrase.into()));
        }
    }

    #[test]
    fn the_marked_test_phrase_of_the_window_is_performed() {
        let phrase = "[cheerful] Привет! Это Мика. <whisper>Так я звучу.</whisper>";
        assert_eq!(
            test_phrase(phrase, true),
            TestPhrase::Marked {
                marked: phrase.into(),
                plain: "Привет! Это Мика. Так я звучу.".into(),
            }
        );
    }

    #[test]
    fn a_test_phrase_of_marks_alone_is_said_as_before() {
        for text in ["[sigh]", "<whisper></whisper>", "", "  "] {
            assert_eq!(
                test_phrase(text, true),
                TestPhrase::Plain(text.into()),
                "{text}"
            );
        }
    }

    #[test]
    fn the_test_phrase_is_cut_to_300_characters() {
        let long = "ё".repeat(400);
        assert_eq!(
            test_phrase(&long, false),
            TestPhrase::Plain("ё".repeat(300))
        );
        let TestPhrase::Marked { marked, plain } = test_phrase(&format!("[sigh] {long}"), true)
        else {
            panic!("a phrase with words keeps its marks");
        };
        assert_eq!(marked, format!("[sigh] {}", "ё".repeat(293)));
        assert_eq!(plain, "ё".repeat(293));
    }

    #[test]
    fn the_telegram_view_of_fresh_settings_has_voice_off() {
        let config = mika_core::config::Config::default();
        let view = telegram_view(&config, false, "off".into(), false);
        assert!(!view.enabled && !view.has_token);
        assert_eq!(view.user_id, 0);
        assert_eq!(view.status, "off");
        assert!(!view.voice_in && !view.voice_reply);
        assert!(!view.stt_ready && !view.tts_ready && !view.ffmpeg_found);
        assert_eq!(view.voice_max_s, 120);
    }

    #[test]
    fn the_telegram_view_tells_what_voice_messages_can_use() {
        let mut config = mika_core::config::Config::default();
        config.telegram.enabled = true;
        config.telegram.user_id = 123_456_789;
        config.voice.stt_model = "qwen/qwen3-asr-0.6b".into();
        // The window reads exactly these names.
        assert_eq!(
            serde_json::to_value(telegram_view(&config, true, "on".into(), false)).unwrap(),
            serde_json::json!({
                "enabled": true,
                "user_id": 123_456_789,
                "has_token": true,
                "status": "on",
                "voice_in": false,
                "voice_reply": false,
                "stt_ready": true,
                "tts_ready": false,
                "ffmpeg_found": false,
                "voice_max_s": 120,
            })
        );

        config.telegram.voice_in = true;
        config.telegram.voice_reply = true;
        config.voice.tts = TtsEngine::Provider;
        config.voice.tts_model = "fish-audio/drama-3".into();
        let view = telegram_view(&config, true, "on".into(), true);
        assert!(view.voice_in && view.voice_reply);
        assert!(view.stt_ready && view.tts_ready && view.ffmpeg_found);

        // The switches are shown as saved, whatever the voice can do.
        config.voice.tts = TtsEngine::Windows;
        let view = telegram_view(&config, true, "on".into(), true);
        assert!(view.voice_in && view.voice_reply && !view.tts_ready);
        // A provider voice without a model makes no voice message either.
        config.voice.tts = TtsEngine::Provider;
        config.voice.tts_model = "  ".into();
        config.voice.stt_model = " ".into();
        let view = telegram_view(&config, true, "on".into(), true);
        assert!(!view.tts_ready && !view.stt_ready);
    }

    #[test]
    fn the_texts_of_the_telegram_voice_switches_name_the_real_limits() {
        use mika_providers::telegram::voice::VOICE_FRESH_S;
        let taken = crate::strings::tr("ext.tg_voice_in_sub");
        assert!(
            taken.contains(&format!("больше {} минут назад", VOICE_FRESH_S / 60)),
            "{taken}"
        );
        let said = crate::strings::tr("ext.tg_voice_reply_sub");
        assert!(
            said.contains(&format!(
                "не больше {} знаков",
                mika_audio::speech::VOICE_NOTE_CHARS
            )),
            "{said}"
        );
    }
}
