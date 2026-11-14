//! Reminders, timers, alarms and scheduled tasks. One table, one scheduler.

use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::events::{Event, EventBus};
use crate::storage::{now, Db, DbError};

/// What happens when the time comes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderKind {
    /// Show the text to the user.
    Remind,
    /// Hand the text to the assistant as a message.
    Task,
}

impl ReminderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReminderKind::Remind => "remind",
            ReminderKind::Task => "task",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "remind" => Some(ReminderKind::Remind),
            "task" => Some(ReminderKind::Task),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    None,
    Daily,
    Weekly,
    /// Every N minutes, N >= 1.
    Every(u32),
}

impl Repeat {
    pub fn encode(self) -> String {
        match self {
            Repeat::None => "none".into(),
            Repeat::Daily => "daily".into(),
            Repeat::Weekly => "weekly".into(),
            Repeat::Every(m) => format!("every:{m}"),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "" | "none" => Some(Repeat::None),
            "daily" => Some(Repeat::Daily),
            "weekly" => Some(Repeat::Weekly),
            other => other
                .strip_prefix("every:")
                .and_then(|m| m.trim().parse().ok())
                .filter(|m| *m >= 1)
                .map(Repeat::Every),
        }
    }

    fn step_s(self) -> Option<i64> {
        match self {
            Repeat::None => None,
            Repeat::Daily => Some(86_400),
            Repeat::Weekly => Some(7 * 86_400),
            Repeat::Every(m) => Some(m as i64 * 60),
        }
    }

    /// «каждый день», «каждые 30 мин».
    pub fn describe(self) -> String {
        match self {
            Repeat::None => String::new(),
            Repeat::Daily => "каждый день".into(),
            Repeat::Weekly => "каждую неделю".into(),
            Repeat::Every(m) => format!("каждые {m} мин"),
        }
    }
}

/// First time after `now` that a repeating reminder is due again. Unix seconds.
pub fn next_due(due: i64, repeat: Repeat, now: i64) -> Option<i64> {
    let step = repeat.step_s()?;
    let missed = ((now - due).max(0) / step) + 1;
    Some(due + missed * step)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reminder {
    pub id: i64,
    /// Unix seconds.
    pub due_at: i64,
    pub text: String,
    pub kind: ReminderKind,
    pub repeat: Repeat,
}

/// Local `YYYY-MM-DD HH:MM` of a unix time.
pub fn format_due(due_at: i64) -> String {
    Local
        .timestamp_opt(due_at, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| due_at.to_string())
}

/// Reads `HH:MM` (today, or tomorrow when that time has passed),
/// `YYYY-MM-DD HH:MM` or `YYYY-MM-DDTHH:MM`. Returns unix seconds in local time.
pub fn parse_when(text: &str, now: chrono::DateTime<Local>) -> Option<i64> {
    let t = text.trim().replace('T', " ");
    let t = t.trim_end_matches(":00Z").trim();
    let local = |dt: NaiveDateTime| Local.from_local_datetime(&dt).earliest();
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%d.%m.%Y %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(t, fmt) {
            return local(dt).map(|d| d.timestamp());
        }
    }
    if let Ok(time) = NaiveTime::parse_from_str(t, "%H:%M") {
        let today = local(now.date_naive().and_time(time))?;
        if today > now {
            return Some(today.timestamp());
        }
        let tomorrow = now.date_naive().succ_opt()?.and_time(time);
        return local(tomorrow).map(|d| d.timestamp());
    }
    None
}

/// Reads `YYYY-MM-DD` or `DD.MM.YYYY`.
pub fn parse_date(text: &str) -> Option<NaiveDate> {
    let t = text.trim();
    NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(t, "%d.%m.%Y"))
        .ok()
}

#[derive(Clone)]
pub struct ReminderStore {
    db: Arc<Db>,
}

impl ReminderStore {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    pub fn add(
        &self,
        due_at: i64,
        text: &str,
        kind: ReminderKind,
        repeat: Repeat,
    ) -> Result<i64, DbError> {
        let id = self.db.with(|c| {
            c.execute(
                "INSERT INTO reminders (created_at, due_at, text, kind, repeat) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![now(), due_at, text.trim(), kind.as_str(), repeat.encode()],
            )?;
            Ok(c.last_insert_rowid())
        })?;
        // What to remind of is the user's text: the record has its size only.
        log::info!(
            "reminder set: id={id} kind={} repeat={} due_in_s={} chars={}",
            kind.as_str(),
            repeat.encode(),
            due_at - Local::now().timestamp(),
            text.trim().chars().count()
        );
        Ok(id)
    }

    /// Open reminders, the nearest first.
    pub fn list(&self) -> Result<Vec<Reminder>, DbError> {
        self.select("WHERE done = 0 ORDER BY due_at, id", i64::MAX)
    }

    fn select(&self, tail: &str, due_before: i64) -> Result<Vec<Reminder>, DbError> {
        let sql = format!("SELECT id, due_at, text, kind, repeat FROM reminders {tail}");
        self.db.with(|c| {
            let mut st = c.prepare(&sql)?;
            let has_param = sql.contains("?1");
            let map = |r: &rusqlite::Row<'_>| {
                Ok(Reminder {
                    id: r.get(0)?,
                    due_at: r.get(1)?,
                    text: r.get(2)?,
                    kind: ReminderKind::parse(&r.get::<_, String>(3)?)
                        .unwrap_or(ReminderKind::Remind),
                    repeat: Repeat::parse(&r.get::<_, String>(4)?).unwrap_or(Repeat::None),
                })
            };
            let rows = if has_param {
                st.query_map([due_before], map)?.collect()
            } else {
                st.query_map([], map)?.collect()
            };
            rows
        })
    }

    pub fn cancel(&self, id: i64) -> Result<bool, DbError> {
        let cancelled = self
            .db
            .with(|c| c.execute("DELETE FROM reminders WHERE id = ?1 AND done = 0", [id]))
            .map(|n| n > 0)?;
        if cancelled {
            log::info!("reminder cancelled: id={id}");
        }
        Ok(cancelled)
    }

    /// Reminders whose time has come. One-shot ones are closed, repeating ones move
    /// to their next time, so each is returned once per occurrence.
    pub fn take_due(&self, now: i64) -> Result<Vec<Reminder>, DbError> {
        let due = self.select("WHERE done = 0 AND due_at <= ?1 ORDER BY due_at, id", now)?;
        for r in &due {
            self.db.with(|c| match next_due(r.due_at, r.repeat, now) {
                Some(next) => c.execute(
                    "UPDATE reminders SET due_at = ?2 WHERE id = ?1",
                    params![r.id, next],
                ),
                None => c.execute("UPDATE reminders SET done = 1 WHERE id = ?1", [r.id]),
            })?;
        }
        Ok(due)
    }
}

/// How often a store that keeps failing is written to the journal, in looks
/// of the scheduler: every ten minutes.
const FAILURE_NOTE_EVERY: u64 = 600;

/// Checks the store every second and publishes `Event::ReminderFired`.
/// For `ReminderKind::Task` it also calls `on_task` with the text.
pub async fn run_scheduler(
    store: ReminderStore,
    bus: EventBus,
    on_task: impl Fn(String) + Send + 'static,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Looks in a row that failed: the store is asked every second, the
    // journal is told at the first failure and then once in a while.
    let mut failed: u64 = 0;
    loop {
        tick.tick().await;
        let now = Local::now().timestamp();
        let due = match store.take_due(now) {
            Ok(d) => d,
            Err(e) => {
                if failed.is_multiple_of(FAILURE_NOTE_EVERY) {
                    log::error!(
                        "cannot read reminders, none fires: failed_looks={failed} error={e}"
                    );
                }
                failed += 1;
                continue;
            }
        };
        if failed > 0 {
            log::info!("reminders are read again: failed_looks={failed}");
            failed = 0;
        }
        for r in due {
            log::info!(
                "reminder fired: id={} kind={} repeat={} late_s={}",
                r.id,
                r.kind.as_str(),
                r.repeat.encode(),
                (now - r.due_at).max(0)
            );
            bus.publish(Event::ReminderFired {
                id: r.id,
                text: r.text.clone(),
                kind: r.kind,
                late_s: (now - r.due_at).max(0) as u64,
            });
            if r.kind == ReminderKind::Task {
                on_task(r.text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ReminderStore {
        ReminderStore::new(Arc::new(Db::open_in_memory().unwrap()))
    }

    #[test]
    fn one_shot_fires_once() {
        let s = store();
        let id = s
            .add(100, " чайник ", ReminderKind::Remind, Repeat::None)
            .unwrap();
        assert!(s.take_due(99).unwrap().is_empty());
        let due = s.take_due(100).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!((due[0].id, due[0].text.as_str()), (id, "чайник"));
        assert!(s.take_due(500).unwrap().is_empty());
        assert!(s.list().unwrap().is_empty());
    }

    #[test]
    fn cancelled_reminder_never_fires() {
        let s = store();
        let id = s.add(100, "x", ReminderKind::Remind, Repeat::None).unwrap();
        assert!(s.cancel(id).unwrap());
        assert!(!s.cancel(id).unwrap());
        assert!(s.take_due(1000).unwrap().is_empty());
    }

    #[test]
    fn repeating_reminder_moves_past_now() {
        let s = store();
        s.add(1000, "зарядка", ReminderKind::Task, Repeat::Daily)
            .unwrap();
        // The app was closed for two and a half days: one firing, next time in the future.
        let now = 1000 + 86_400 * 2 + 43_200;
        assert_eq!(s.take_due(now).unwrap().len(), 1);
        let left = s.list().unwrap();
        assert_eq!(left[0].due_at, 1000 + 86_400 * 3);
        assert!(s.take_due(now + 1).unwrap().is_empty());
    }

    #[test]
    fn next_due_steps() {
        assert_eq!(next_due(100, Repeat::None, 500), None);
        assert_eq!(next_due(100, Repeat::Every(1), 100), Some(160));
        assert_eq!(next_due(100, Repeat::Every(1), 161), Some(220));
        assert_eq!(next_due(100, Repeat::Weekly, 50), Some(100 + 7 * 86_400));
    }

    #[test]
    fn repeat_roundtrip() {
        for r in [
            Repeat::None,
            Repeat::Daily,
            Repeat::Weekly,
            Repeat::Every(15),
        ] {
            assert_eq!(Repeat::parse(&r.encode()), Some(r));
        }
        assert_eq!(Repeat::parse("every:0"), None);
        assert_eq!(Repeat::parse("monthly"), None);
    }

    #[test]
    fn parse_when_time_only_rolls_to_tomorrow() {
        let now = Local.with_ymd_and_hms(2026, 11, 11, 15, 0, 0).unwrap();
        let later = parse_when("18:30", now).unwrap();
        assert_eq!(format_due(later), "2026-11-11 18:30");
        let passed = parse_when("09:00", now).unwrap();
        assert_eq!(format_due(passed), "2026-11-12 09:00");
        let full = parse_when("2026-12-31T23:59", now).unwrap();
        assert_eq!(format_due(full), "2026-12-31 23:59");
        assert_eq!(
            format_due(parse_when("01.01.2027 08:00", now).unwrap()),
            "2027-01-01 08:00"
        );
        assert_eq!(parse_when("скоро", now), None);
    }

    #[test]
    fn parse_date_formats() {
        assert_eq!(
            parse_date("2026-11-12"),
            NaiveDate::from_ymd_opt(2026, 11, 12)
        );
        assert_eq!(
            parse_date("12.11.2026"),
            NaiveDate::from_ymd_opt(2026, 11, 12)
        );
        assert_eq!(parse_date("завтра"), None);
    }

    #[tokio::test]
    async fn scheduler_publishes_and_runs_tasks() {
        let s = store();
        let past = Local::now().timestamp() - 120;
        s.add(past, "полить цветы", ReminderKind::Remind, Repeat::None)
            .unwrap();
        s.add(past, "скажи время", ReminderKind::Task, Repeat::None)
            .unwrap();
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        let tasks = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let seen = tasks.clone();
        let job = tokio::spawn(run_scheduler(s, bus, move |t| seen.lock().push(t)));
        tokio::time::sleep(Duration::from_millis(300)).await;
        job.abort();
        let fired: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(fired.len(), 2);
        assert!(matches!(
            &fired[0],
            Event::ReminderFired { text, late_s, kind: ReminderKind::Remind, .. }
                if text == "полить цветы" && *late_s >= 120
        ));
        assert_eq!(*tasks.lock(), vec!["скажи время".to_string()]);
    }
}
