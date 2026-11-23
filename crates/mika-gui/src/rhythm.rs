//! The rhythm of the day: focus and breaks, greetings, time in programs,
//! copied texts, meetings from the calendar, the index of documents.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Local, NaiveDate, Timelike};
use mika_audio::SpeakerSwitch;
use mika_core::config::{Config, CALENDAR_SECRET_KEY};
use mika_core::emotion::Emotion;
use mika_core::Event;
use mika_tools::calendar::{meeting_line, starting_soon, Calendar};
use mika_tools::cliphist::ClipHistory;
use mika_tools::usage::UsageStore;
use parking_lot::Mutex;

use crate::app::Ctx;
use crate::strings::tr;

/// How often the rhythm loop looks around.
const TICK: Duration = Duration::from_secs(5);
/// A gap between two ticks longer than this, seconds, means the computer slept.
const SLEPT_S: u64 = 3 * TICK.as_secs();
/// How often the clipboard watch looks at the window in front and the number
/// of the last clipboard change.
const CLIP_EVERY: Duration = Duration::from_millis(500);
/// A copy noticed this soon after a window of the blind zone was in front is
/// not kept.
const BLIND_MEMORY: Duration = Duration::from_secs(3);
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

/// One folder written another way: letter case, slashes, a separator at the end.
fn folder_key(path: &str) -> String {
    path.trim()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

/// Folders without empty lines and repeats; the first spelling stays.
pub fn unique_folders(folders: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    folders
        .into_iter()
        .map(|f| f.trim().to_string())
        .filter(|f| {
            let key = folder_key(f);
            !key.is_empty() && seen.insert(key)
        })
        .collect()
}

/// The first of `folders` that is new, not in `saved`, and is not a folder.
/// Saved ones are not asked again: a network drive may be away for now, and
/// asking it can take a minute.
pub fn missing_folder(
    folders: &[String],
    saved: &[String],
    profile: &str,
    is_dir: impl Fn(&Path) -> bool,
) -> Option<String> {
    folders
        .iter()
        .filter(|f| !saved.contains(f))
        .find(|f| {
            folder_paths(std::slice::from_ref(*f), profile)
                .first()
                .is_none_or(|p| !is_dir(p))
        })
        .cloned()
}

/// The folders to save when the user turned the list `seen` into `wanted` and
/// meanwhile the saved list became `now`: another change went through while
/// the new folders were checked. Only this change is applied to `now`: a
/// folder removed meanwhile does not come back, one added meanwhile stays.
pub fn merge_folders(seen: &[String], wanted: &[String], now: &[String]) -> Vec<String> {
    if now == seen {
        return wanted.to_vec();
    }
    let has = |list: &[String], f: &str| list.iter().any(|x| folder_key(x) == folder_key(f));
    let kept = now
        .iter()
        .filter(|f| !(has(seen, f) && !has(wanted, f)))
        .cloned();
    let added = wanted.iter().filter(|f| !has(seen, f)).cloned();
    unique_folders(kept.chain(added).collect())
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
    /// When the loop looked last, unix seconds.
    last_tick: Option<i64>,
    sample_left: u64,
    pruned_on: Option<NaiveDate>,
    /// The day old conversations were last deleted (decision 0052).
    purged_on: Option<NaiveDate>,
}

impl Watch {
    /// Notes the user at `at`, `input_idle` seconds after the last input.
    /// Returns the idle seconds to go by and whether the user has just come
    /// back after being away. A long gap between ticks means the computer
    /// slept: it counts as time away, even when the input that woke it is fresh.
    fn presence(&mut self, input_idle: u64, at: i64) -> (u64, bool) {
        let gap = self.last_tick.map_or(0, |t| (at - t).max(0) as u64);
        self.last_tick = Some(at);
        let idle = if gap > SLEPT_S {
            input_idle.max(gap)
        } else {
            input_idle
        };
        // A stretch of work ends with a pause of a few minutes.
        if idle >= BREAK_PAUSE_S {
            self.working_since = None;
        } else if self.working_since.is_none() {
            self.working_since = Some(at - idle as i64);
        }
        let mut back = false;
        if idle >= AWAY_S {
            self.away = true;
        } else if self.away && idle < 10 {
            self.away = false;
            back = true;
        }
        (idle, back)
    }
}

/// Time in programs for one tick. Old days go once a day, whether counting is
/// on or not. A sample goes to the base at once, under the day it was taken:
/// nothing waits in memory to be lost at exit or to come back after «Стереть
/// учёт». `front` is the program in front, when someone is there.
fn count_app_time(
    w: &mut Watch,
    store: &UsageStore,
    today: NaiveDate,
    on: bool,
    front: impl FnOnce() -> Option<String>,
) {
    if w.pruned_on != Some(today) {
        w.pruned_on = Some(today);
        if let Err(e) = store.prune(today) {
            log::warn!("cannot forget old time in programs: {e}");
        }
    }
    if !on {
        return;
    }
    if w.sample_left > TICK.as_secs() {
        w.sample_left -= TICK.as_secs();
        return;
    }
    w.sample_left = SAMPLE_S;
    if let Some(app) = front() {
        if let Err(e) = store.add(today, &HashMap::from([(app, SAMPLE_S)])) {
            log::warn!("cannot save time in programs: {e}");
        }
    }
}

/// When a window of the blind zone was last seen in front. The clipboard is
/// read a moment after the copy, and by then the user may be in another
/// window: a copy is judged by where it was likely made.
#[derive(Default)]
struct BlindWatch(Option<Instant>);

impl BlindWatch {
    fn look(&mut self, blind: bool, now: Instant) {
        if blind {
            self.0 = Some(now);
        }
    }

    fn recent(&self, now: Instant) -> bool {
        self.0
            .is_some_and(|t| now.saturating_duration_since(t) <= BLIND_MEMORY)
    }
}

/// Keeps a copied text while the history is on. The setting is checked under
/// its lock: turning the history off writes it first and clears the history
/// after, so a copy read meanwhile cannot come back once it is cleared.
fn keep_copy(config: &Mutex<Config>, clips: &ClipHistory, text: &str, app: &str, at: i64) -> bool {
    let config = config.lock();
    config.tracking.clipboard && clips.push(text, app, at)
}

/// Whether a line of hers sounds: replies are spoken and the system sound is
/// on. Muted, nothing is made: a paid synthesizer is not asked for silence.
pub(crate) fn speaks(speaker: &SpeakerSwitch) -> bool {
    speaker.get() && speaker.heard()
}

impl Ctx {
    /// A line of her own: in the chat, and aloud when replies are spoken.
    pub fn remark(&self, text: &str, kind: &str, aloud: bool) {
        self.remark_said(text, kind, aloud.then_some(text));
    }

    /// A line of her own in the chat; `said`, when given, sounds in its place.
    /// During the pause after a full stop (decision 0045) a lively line or a
    /// break is skipped and the rest only shows: nothing sounds.
    pub fn remark_said(&self, text: &str, kind: &str, said: Option<&str>) {
        if text.trim().is_empty() {
            return;
        }
        let pause = self.session.pause();
        let paused = pause.is_on();
        if paused {
            if let Some(skipped) = mika_core::pause::held_in_pause(kind) {
                pause.skip(skipped, "");
                return;
            }
        }
        self.bus.publish(Event::Remark {
            text: text.to_string(),
            kind: kind.to_string(),
        });
        if let Some(said) = said.filter(|_| !paused && speaks(&self.speaker)) {
            self.voice
                .speak(&mika_audio::speech::clean_for_speech(said));
        }
    }

    /// Focus is over: what came meanwhile goes into the conversation and the
    /// chat. Aloud she says how many came; the texts sound only when
    /// notifications may be read aloud.
    pub fn focus_over(&self, held: &[String]) {
        if !held.is_empty() {
            self.session
                .agent
                .append_note(&format!("Пока шёл фокус, пришло:\n{}", held.join("\n")));
        }
        let read_aloud = self.config.lock().notifications.read_aloud;
        let said = mika_tools::focus::spoken_summary(held, read_aloud);
        self.remark_said(&mika_tools::focus::summary(held), "focus", Some(&said));
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
        // The user's own input: what she sent herself does not count.
        let (idle, back) = w.presence(self.idle_seconds(), at);
        let (rhythm, app_time) = {
            let c = self.config.lock();
            (c.rhythm.clone(), c.tracking.app_time)
        };
        let shared = &self.tool_env().shared;

        if let Some(held) = shared.focus.expired(at) {
            self.bus.publish(Event::EmotionCue {
                emotion: Emotion::Joy,
            });
            self.focus_over(&held);
        }
        let focused = shared.focus.active(at);
        // Budget warnings that waited for the end of focus (decision 0028):
        // after its summary, as silent lines, never among the notifications.
        if !focused {
            self.release_cost_alerts();
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
        if back && rhythm.lively && !focused && at - w.last_greeting >= GREET_GAP_S {
            w.last_greeting = at;
            self.bus.publish(Event::EmotionCue {
                emotion: Emotion::Joy,
            });
            self.remark(&pick(&tr(greeting_key(now.hour())), seed()), "life", true);
        }
        let night = (1..=4).contains(&now.hour());
        let tonight = now.date_naive().to_string();
        if rhythm.lively && night && idle < 60 && !focused && w.late_night != tonight {
            w.late_night = tonight;
            self.bus.publish(Event::EmotionCue {
                emotion: Emotion::Sleepy,
            });
            self.remark(&pick(&tr("life.late"), seed()), "life", false);
        }

        // Old conversations: at the first tick after start and once a day.
        if w.purged_on != Some(now.date_naive()) {
            w.purged_on = Some(now.date_naive());
            // SQLite and the disk: off the async threads.
            let ctx = self.clone();
            tauri::async_runtime::spawn_blocking(move || ctx.purge_conversations());
        }

        // Time in programs: the window in front, while someone is there.
        shared
            .app_time_on
            .store(app_time, std::sync::atomic::Ordering::Relaxed);
        let store = UsageStore(self.db.clone());
        count_app_time(w, &store, now.date_naive(), app_time, || {
            if idle >= PRESENT_S {
                return None;
            }
            let own = mika_tools::own_process();
            self.tool_env()
                .windows
                .foreground()
                .map(|f| f.process)
                .filter(|p| !p.is_empty() && *p != own)
        });
    }

    /// Copied texts, while the user allows it. Reads only on a change.
    fn start_clipboard_watch(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            let mut last = mika_tools::win::clipboard_sequence();
            let mut blind = BlindWatch::default();
            loop {
                tokio::time::sleep(CLIP_EVERY).await;
                let Some(ctx) = me.upgrade() else { break };
                let shared = ctx.tool_env().shared.clone();
                let on = ctx.config.lock().tracking.clipboard;
                shared
                    .clipboard_on
                    .store(on, std::sync::atomic::Ordering::Relaxed);
                // Copies in password, bank and sign-in windows are never kept,
                // also when the user switched away before the clipboard was read.
                let now = Instant::now();
                let front = if on {
                    ctx.tool_env().windows.foreground()
                } else {
                    None
                };
                blind.look(
                    front
                        .as_ref()
                        .is_some_and(|f| mika_core::policy::blind::is_blind_title(&f.title)),
                    now,
                );
                let seq = mika_tools::win::clipboard_sequence();
                if seq == last {
                    continue;
                }
                last = seq;
                if !on
                    || blind.recent(now)
                    || shared.clips.paused()
                    || mika_tools::win::excluded_from_history()
                {
                    continue;
                }
                let clipboard = ctx.tool_env().clipboard.clone();
                let got = tauri::async_runtime::spawn_blocking(move || {
                    let text = clipboard.get().ok()?;
                    // Changed while it was read, maybe to a marked copy of a
                    // password manager: what was read is not kept, and the new
                    // copy is looked at on the next round.
                    let same = mika_tools::win::clipboard_sequence() == seq
                        && !mika_tools::win::excluded_from_history();
                    same.then_some(text)
                })
                .await;
                if let Ok(Some(text)) = got {
                    let app = front.map(|f| f.process).unwrap_or_default();
                    keep_copy(
                        &ctx.config,
                        &shared.clips,
                        &text,
                        &app,
                        Local::now().timestamp(),
                    );
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
                            .await;
                            // A parse that fell over is a failed read, not an
                            // empty calendar: the meetings already known stay.
                            match parsed {
                                Ok(parsed) => {
                                    // Read every quarter of an hour: detail.
                                    log::debug!("calendar read: events={}", parsed.len());
                                    state.set(parsed, Local::now().timestamp());
                                }
                                Err(e) => {
                                    log::warn!("calendar not understood, the meetings known before stay: {e}");
                                    state.fail(&tr("calendar.not_ics"));
                                }
                            }
                        }
                        Ok(_) => state.fail(&tr("calendar.not_ics")),
                        Err(e) => {
                            // The link is a secret and is never part of the text.
                            log::warn!(
                                "calendar not read, the meetings known before stay: {}",
                                mika_core::journal::error_text(&e)
                            );
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
                let config = ctx.config.clone();
                let profile = ctx.user_profile().to_string();
                let index = ctx.tool_env().shared.docs.clone();
                drop(ctx);
                // Asked again before each file and each batch of vectors: a folder
                // removed mid-pass is not read further and never reaches the
                // embedding model. The settings, not the app, are held: closing
                // the app does not look like an empty list that wipes the index.
                let current = move || folder_paths(&config.lock().docs.folders, &profile);
                let cancel = tokio_util::sync::CancellationToken::new();
                // A pass that went through writes its own record with the counts.
                if let Err(e) = index.run_with(current, &cancel).await {
                    log::warn!(
                        "documents not indexed: {}",
                        mika_core::journal::error_text(&e)
                    );
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

    #[test]
    fn a_folder_is_kept_once() {
        assert_eq!(
            unique_folders(vec![
                "D:\\Договоры".into(),
                " d:/договоры\\ ".into(),
                "".into(),
                "~\\Documents".into(),
                "D:\\Договоры".into(),
            ]),
            ["D:\\Договоры", "~\\Documents"]
        );
    }

    #[test]
    fn only_new_folders_are_checked() {
        let asked = std::cell::RefCell::new(Vec::new());
        let is_dir = |p: &Path| {
            asked.borrow_mut().push(p.to_path_buf());
            p != Path::new("E:\\Нет")
        };
        let saved = ["\\\\nas\\Документы".to_string()];
        let folders = [
            "\\\\nas\\Документы".to_string(),
            "~\\Documents".to_string(),
            "E:\\Нет".to_string(),
        ];
        assert_eq!(
            missing_folder(&folders, &saved, "C:\\Users\\me", is_dir),
            Some("E:\\Нет".to_string())
        );
        // The saved network folder is not asked: it may hang for a minute.
        assert_eq!(
            *asked.borrow(),
            [
                PathBuf::from("C:\\Users\\me\\Documents"),
                PathBuf::from("E:\\Нет")
            ]
        );
        assert_eq!(
            missing_folder(&saved, &saved, "C:\\Users\\me", |_| false),
            None
        );
    }

    #[test]
    fn a_change_saved_meanwhile_is_not_undone() {
        let list = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let seen = list(&["D:\\Работа", "D:\\Личное"]);
        // Nothing else changed: the list is saved as sent.
        let wanted = list(&["D:\\Работа", "D:\\Личное", "\\\\nas\\Документы"]);
        assert_eq!(merge_folders(&seen, &wanted, &seen), wanted);
        // The network folder took a while to answer; meanwhile «Личное» was
        // removed. It does not come back, the network folder is added.
        let now = list(&["D:\\Работа"]);
        assert_eq!(
            merge_folders(&seen, &wanted, &now),
            ["D:\\Работа", "\\\\nas\\Документы"]
        );
        // Two quick additions: the first does not wipe the second.
        let now = list(&["D:\\Работа", "D:\\Личное", "E:\\Второе"]);
        let wanted = list(&["D:\\Работа", "D:\\Личное", "E:\\Первое"]);
        assert_eq!(
            merge_folders(&seen, &wanted, &now),
            ["D:\\Работа", "D:\\Личное", "E:\\Второе", "E:\\Первое"]
        );
        // A removal that comes second removes, whatever the spelling, and
        // keeps what was added meanwhile.
        let wanted = list(&["D:\\Работа"]);
        assert_eq!(
            merge_folders(
                &seen,
                &wanted,
                &list(&["d:/работа", "D:\\Личное", "E:\\Новое"])
            ),
            ["d:/работа", "E:\\Новое"]
        );
    }

    #[test]
    fn a_sleep_counts_as_time_away() {
        let mut w = Watch::default();
        // Forty minutes of work, then the lid closes with no idle time seen.
        assert_eq!(w.presence(0, 10_000), (0, false));
        assert_eq!(w.working_since, Some(10_000));
        assert_eq!(w.presence(1, 10_005), (1, false));
        // Three hours later the touchpad wakes it: the input is fresh.
        let woke = 10_005 + 3 * 3600;
        let (idle, back) = w.presence(1, woke);
        assert!(idle >= AWAY_S && !back);
        assert!(w.away && w.working_since.is_none());
        // The next tick greets, and the stretch of work starts anew.
        assert_eq!(w.presence(2, woke + 5), (2, true));
        assert_eq!(w.working_since, Some(woke + 3));
        // A late tick of a few seconds is not a sleep.
        assert_eq!(w.presence(0, woke + 15), (0, false));
        assert_eq!(w.working_since, Some(woke + 3));
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn time_in_programs_is_written_at_once_and_old_days_go_when_off() {
        let store = UsageStore(Arc::new(mika_core::storage::Db::open_in_memory().unwrap()));
        let old = HashMap::from([("chrome.exe".to_string(), 600)]);
        store.add(day("2026-08-01"), &old).unwrap();
        let all = |s: &UsageStore| s.report(day("2026-01-01"), day("2026-12-31")).unwrap();
        let mut w = Watch::default();
        // Counting is off: nothing is asked or written, the old day goes anyway.
        count_app_time(&mut w, &store, day("2026-11-13"), false, || {
            panic!("the window in front is not asked while counting is off")
        });
        assert!(all(&store).is_empty());
        // On: the first sample is in the base at once, under its own day.
        let front = || Some("telegram.exe".to_string());
        count_app_time(&mut w, &store, day("2026-11-13"), true, front);
        assert_eq!(all(&store), [("telegram.exe".to_string(), SAMPLE_S)]);
        // The next samples come every 15 seconds; one just after midnight
        // belongs to the new day.
        count_app_time(&mut w, &store, day("2026-11-13"), true, front);
        count_app_time(&mut w, &store, day("2026-11-13"), true, front);
        count_app_time(&mut w, &store, day("2026-11-14"), true, front);
        assert_eq!(
            store.report(day("2026-11-14"), day("2026-11-14")).unwrap(),
            [("telegram.exe".to_string(), SAMPLE_S)]
        );
        // Wiped: nothing kept in memory comes back with the next sample.
        store.clear().unwrap();
        count_app_time(&mut w, &store, day("2026-11-14"), true, || None);
        count_app_time(&mut w, &store, day("2026-11-14"), true, || None);
        count_app_time(&mut w, &store, day("2026-11-14"), true, || None);
        assert!(all(&store).is_empty());
    }

    #[test]
    fn a_copy_soon_after_a_blind_window_is_not_kept() {
        let t = Instant::now();
        let mut b = BlindWatch::default();
        b.look(false, t);
        assert!(!b.recent(t));
        // The bank tab was in front, then Alt+Tab before the clipboard was read.
        b.look(true, t + Duration::from_millis(500));
        b.look(false, t + Duration::from_millis(1000));
        assert!(b.recent(t + Duration::from_millis(1500)));
        assert!(!b.recent(t + Duration::from_secs(5)));
    }

    #[test]
    fn a_copy_is_not_kept_once_the_history_is_off() {
        let config = Mutex::new(Config::default());
        let clips = ClipHistory::default();
        config.lock().tracking.clipboard = true;
        assert!(keep_copy(&config, &clips, "первая", "notepad.exe", 1));
        config.lock().tracking.clipboard = false;
        clips.clear();
        assert!(!keep_copy(&config, &clips, "вторая", "notepad.exe", 2));
        assert!(clips.is_empty());
    }

    #[test]
    fn she_is_silent_while_the_sound_is_off() {
        let muted = SpeakerSwitch::new(true).with_audible(Arc::new(|| false));
        assert!(!speaks(&muted));
        let heard = SpeakerSwitch::new(true).with_audible(Arc::new(|| true));
        assert!(speaks(&heard));
        assert!(!speaks(&SpeakerSwitch::new(false)));
    }
}
