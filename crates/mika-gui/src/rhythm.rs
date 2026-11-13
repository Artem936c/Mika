//! The rhythm of the day: focus and breaks, greetings, time in programs,
//! copied texts, meetings from the calendar, the index of documents.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, Timelike};
use mika_core::config::CALENDAR_SECRET_KEY;
use mika_core::emotion::Emotion;
use mika_core::Event;
use mika_tools::calendar::{meeting_line, starting_soon, Calendar};
use mika_tools::usage::UsageStore;

use crate::app::Ctx;
use crate::everyday::idle_seconds;
use crate::strings::tr;

/// How often the rhythm loop looks around.
const TICK: Duration = Duration::from_secs(5);
/// Away this long, she greets on return.
const AWAY_S: u64 = 30 * 60;
/// Greetings at most this often.
const GREET_GAP_S: i64 = 2 * 3600;
/// A pause this long counts as a break.
const BREAK_PAUSE_S: u64 = 5 * 60;
/// A sample of the program in front, seconds.
const SAMPLE_S: u64 = 15;
/// Someone is at the computer: input within this time.
const PRESENT_S: u64 = 120;
/// How often the calendar link is read again.
const CALENDAR_EVERY: Duration = Duration::from_secs(15 * 60);
/// How often the documents are looked over.
const DOCS_EVERY: Duration = Duration::from_secs(60 * 60);

/// Key of the greeting for the hour.
pub fn greeting_key(hour: u32) -> &'static str {
    match hour {
        5..=11 => "life.morning",
        12..=17 => "life.day",
        18..=23 => "life.evening",
        _ => "life.night",
    }
}

/// One of the `|`-separated variants, picked by `seed`.
pub fn pick(variants: &str, seed: u64) -> String {
    let all: Vec<&str> = variants
        .split('|')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect();
    if all.is_empty() {
        return String::new();
    }
    all[(seed % all.len() as u64) as usize].to_string()
}

fn seed() -> u64 {
    Local::now().timestamp_subsec_nanos() as u64
}

/// Folder paths from the settings, with `~` for the user's folder.
pub fn folder_paths(folders: &[String], profile: &str) -> Vec<PathBuf> {
    folders
        .iter()
        .map(|f| f.trim())
        .filter(|f| !f.is_empty())
        .map(|f| match f.strip_prefix('~') {
            Some(rest) => PathBuf::from(format!("{profile}{rest}")),
            None => PathBuf::from(f),
        })
        .collect()
}

/// `webcal://` is the same link over https.
pub fn calendar_url(raw: &str) -> String {
    let raw = raw.trim();
    match raw.strip_prefix("webcal://") {
        Some(rest) => format!("https://{rest}"),
        None => raw.to_string(),
    }
}

/// What the loop remembers between ticks.
#[derive(Default)]
struct Watch {
    away: bool,
    last_greeting: i64,
    late_night: String,
    /// When the current stretch of work began, unix seconds.
    working_since: Option<i64>,
    /// Program seconds not yet written.
    usage: HashMap<String, u64>,
    sample_left: u64,
    flush_left: u32,
    pruned_on: String,
}

impl Ctx {
    /// A line of her own: in the chat, and aloud when replies are spoken.
    pub fn remark(&self, text: &str, kind: &str, aloud: bool) {
        if text.trim().is_empty() {
            return;
        }
        self.bus.publish(Event::Remark {
            text: text.to_string(),
            kind: kind.to_string(),
        });
        if aloud && self.speaker.get() {
            self.voice
                .speak(&mika_audio::speech::clean_for_speech(text));
        }
    }

    /// Focus ends, breaks, greetings, time in programs. One loop, every few seconds.
    pub fn start_rhythm(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            let mut w = Watch::default();
            loop {
                tokio::time::sleep(TICK).await;
                let Some(ctx) = me.upgrade() else { break };
                ctx.rhythm_tick(&mut w);
            }
        });
        self.start_clipboard_watch();
        self.watch_successes();
    }

    fn rhythm_tick(self: &Arc<Self>, w: &mut Watch) {
        let now = Local::now();
        let at = now.timestamp();
        let idle = idle_seconds();
        let (rhythm, app_time) = {
            let c = self.config.lock();
            (c.rhythm.clone(), c.tracking.app_time)
        };
        let shared = &self.tool_env().shared;

        if let Some(held) = shared.focus.expired(at) {
            if !held.is_empty() {
                self.session
                    .agent
                    .append_note(&format!("Пока шёл фокус, пришло:\n{}", held.join("\n")));
            }
            self.bus.publish(Event::EmotionCue {
                emotion: Emotion::Joy,
            });
            self.remark(&mika_tools::focus::summary(&held), "focus", true);
        }
        let focused = shared.focus.active(at);

        // A stretch of work ends with a pause of a few minutes.
        if idle >= BREAK_PAUSE_S {
            w.working_since = None;
        } else if w.working_since.is_none() {
            w.working_since = Some(at - idle as i64);
        }
        let every = rhythm.break_every_min.clamp(15, 240) as i64 * 60;
        if let Some(since) = w.working_since {
            if rhythm.breaks && !focused && at - since >= every {
                w.working_since = Some(at);
                let minutes = (every / 60).to_string();
                let line = pick(&tr("rhythm.break"), seed()).replace("{min}", &minutes);
                self.remark(&line, "break", true);
            }
        }

        // Greetings and «уже поздно», when the user wants her lively.
        if idle >= AWAY_S {
            w.away = true;
        } else if w.away && idle < 10 {
            w.away = false;
            if rhythm.lively && !focused && at - w.last_greeting >= GREET_GAP_S {
                w.last_greeting = at;
                self.bus.publish(Event::EmotionCue {
                    emotion: Emotion::Joy,
                });
                self.remark(&pick(&tr(greeting_key(now.hour())), seed()), "life", true);
            }
        }
        let night = (1..=4).contains(&now.hour());
        let tonight = now.date_naive().to_string();
        if rhythm.lively && night && idle < 60 && !focused && w.late_night != tonight {
            w.late_night = tonight.clone();
            self.bus.publish(Event::EmotionCue {
                emotion: Emotion::Sleepy,
            });
            self.remark(&pick(&tr("life.late"), seed()), "life", false);
        }

        // Time in programs: the window in front, while someone is there.
        shared
            .app_time_on
            .store(app_time, std::sync::atomic::Ordering::Relaxed);
        if !app_time {
            w.usage.clear();
            return;
        }
        if w.sample_left > TICK.as_secs() {
            w.sample_left -= TICK.as_secs();
        } else {
            w.sample_left = SAMPLE_S;
            if idle < PRESENT_S {
                let own = mika_tools::own_process();
                if let Some(front) = self.tool_env().windows.foreground() {
                    if !front.process.is_empty() && front.process != own {
                        *w.usage.entry(front.process).or_default() += SAMPLE_S;
                    }
                }
            }
            w.flush_left = w.flush_left.saturating_sub(1);
            if w.flush_left == 0 && !w.usage.is_empty() {
                w.flush_left = 4;
                let store = UsageStore(self.db.clone());
                let today = now.date_naive();
                if let Err(e) = store.add(today, &w.usage) {
                    log::warn!("cannot save time in programs: {e}");
                }
                w.usage.clear();
                if w.pruned_on != tonight {
                    w.pruned_on = tonight;
                    let _ = store.prune(today);
                }
            }
        }
    }

    /// Copied texts, while the user allows it. Reads only on a change.
    fn start_clipboard_watch(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            let mut last = mika_tools::win::clipboard_sequence();
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let Some(ctx) = me.upgrade() else { break };
                let shared = ctx.tool_env().shared.clone();
                let on = ctx.config.lock().tracking.clipboard;
                shared
                    .clipboard_on
                    .store(on, std::sync::atomic::Ordering::Relaxed);
                let seq = mika_tools::win::clipboard_sequence();
                if seq == last {
                    continue;
                }
                last = seq;
                if !on || shared.clips.paused() || mika_tools::win::excluded_from_history() {
                    continue;
                }
                let windows = ctx.tool_env().windows.clone();
                let clipboard = ctx.tool_env().clipboard.clone();
                let got = tauri::async_runtime::spawn_blocking(move || {
                    let front = windows.foreground();
                    // Copies in password, bank and sign-in windows are never kept.
                    if front
                        .as_ref()
                        .is_some_and(|f| mika_core::policy::blind::is_blind_title(&f.title))
                    {
                        return None;
                    }
                    let text = clipboard.get().ok()?;
                    Some((text, front.map(|f| f.process).unwrap_or_default()))
                })
                .await;
                if let Ok(Some((text, app))) = got {
                    shared.clips.push(&text, &app, Local::now().timestamp());
                }
            }
        });
    }

    /// A finished task makes her glad.
    fn watch_successes(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let mut events = self.bus.subscribe();
        tauri::async_runtime::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                let event = match events.recv().await {
                    Ok(e) => e,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                let Some(ctx) = me.upgrade() else { break };
                if let Event::ToolFinished { name, ok: true, .. } = event {
                    if name == "todo_done" && ctx.config.lock().rhythm.lively {
                        ctx.bus.publish(Event::EmotionCue {
                            emotion: Emotion::Joy,
                        });
                    }
                }
            }
        });
    }

    /// Reads the calendar link now and then and reminds of meetings.
    pub fn start_calendar(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let wake = self.calendar_wake.clone();
        tauri::async_runtime::spawn(async move {
            let mut last_read: Option<std::time::Instant> = None;
            let mut announced: HashSet<(String, i64)> = HashSet::new();
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                    _ = wake.notified() => last_read = None,
                }
                let Some(ctx) = me.upgrade() else { break };
                let cfg = ctx.config.lock().calendar.clone();
                let state = ctx.tool_env().shared.calendar.clone();
                let url = ctx
                    .secrets
                    .get(CALENDAR_SECRET_KEY)
                    .ok()
                    .flatten()
                    .map(|s| s.expose().to_string())
                    .filter(|s| !s.trim().is_empty());
                let Some(url) = url.filter(|_| cfg.enabled) else {
                    state.disconnect();
                    last_read = None;
                    continue;
                };
                if last_read.is_none_or(|t| t.elapsed() >= CALENDAR_EVERY) {
                    last_read = Some(std::time::Instant::now());
                    match ctx.http().get(&calendar_url(&url)).await {
                        Ok(text) if text.contains("BEGIN:VCALENDAR") => {
                            let parsed = tauri::async_runtime::spawn_blocking(move || {
                                Calendar::parse(&text)
                            })
                            .await
                            .unwrap_or_default();
                            log::info!("calendar read: {} events", parsed.len());
                            state.set(parsed, Local::now().timestamp());
                        }
                        Ok(_) => state.fail(&tr("calendar.not_ics")),
                        Err(e) => {
                            log::warn!("calendar not read: {e}");
                            state.fail(&e);
                        }
                    }
                }
                if cfg.remind_min == 0 {
                    continue;
                }
                let now = Local::now().naive_local();
                let edge = now + chrono::Duration::minutes(cfg.remind_min as i64 + 1);
                let soon = state
                    .between(now, edge)
                    .map(|m| starting_soon(&m, now, cfg.remind_min))
                    .unwrap_or_default();
                for m in soon {
                    let key = (m.uid.clone(), m.start.and_utc().timestamp());
                    if !announced.insert(key) {
                        continue;
                    }
                    let minutes = (m.start - now).num_minutes().max(1);
                    let line = tr("calendar.soon")
                        .replace("{min}", &minutes.to_string())
                        .replace("{what}", &meeting_line(&m, false));
                    ctx.remark(&line, "calendar", true);
                }
                announced.retain(|(_, start)| *start > now.and_utc().timestamp() - 3600);
            }
        });
    }

    /// Reads the chosen folders at start, every hour and on request.
    pub fn start_docs(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let wake = self.docs_wake.clone();
        tauri::async_runtime::spawn(async move {
            // Start-up is busy enough: the first pass waits a minute.
            let mut wait = Duration::from_secs(60);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = wake.notified() => {}
                }
                wait = DOCS_EVERY;
                let Some(ctx) = me.upgrade() else { break };
                let folders = ctx.config.lock().docs.folders.clone();
                let paths = folder_paths(&folders, ctx.user_profile());
                let index = ctx.tool_env().shared.docs.clone();
                drop(ctx);
                let cancel = tokio_util::sync::CancellationToken::new();
                match index.run(&paths, &cancel).await {
                    Ok(report) => log::info!("documents: {report}"),
                    Err(e) => log::warn!("documents not indexed: {e}"),
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greetings_by_hour_and_variants() {
        assert_eq!(greeting_key(7), "life.morning");
        assert_eq!(greeting_key(13), "life.day");
        assert_eq!(greeting_key(22), "life.evening");
        assert_eq!(greeting_key(3), "life.night");
        assert_eq!(pick("а|б| в ", 5), "в");
        assert_eq!(pick("", 5), "");
        for key in [
            "life.morning",
            "life.day",
            "life.evening",
            "life.night",
            "life.late",
            "rhythm.break",
            "calendar.soon",
            "calendar.not_ics",
        ] {
            assert_ne!(tr(key), key, "{key} is missing in ru.json");
        }
        assert!(tr("rhythm.break").contains("{min}"));
        assert!(tr("calendar.soon").contains("{what}"));
    }

    #[test]
    fn folders_and_links() {
        assert_eq!(
            folder_paths(
                &["~\\Documents".into(), " ".into(), "D:\\Работа".into()],
                "C:\\Users\\me"
            ),
            [
                PathBuf::from("C:\\Users\\me\\Documents"),
                PathBuf::from("D:\\Работа")
            ]
        );
        assert_eq!(
            calendar_url("webcal://p.example/a.ics"),
            "https://p.example/a.ics"
        );
        assert_eq!(calendar_url(" https://x/a.ics "), "https://x/a.ics");
    }
}
