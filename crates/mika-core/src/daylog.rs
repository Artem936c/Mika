//! The day log: what happened today that nothing else keeps. A finished focus
//! session, a todo marked done, a reminder that fired, a task she finished.
//! The evening summary reads it. Kept for `KEEP_DAYS` days, on this computer only.

use std::sync::Arc;

use chrono::{Duration, Local, NaiveDate, TimeZone};
use parking_lot::Mutex;
use rusqlite::params;

use crate::events::{Event, EventBus};
use crate::storage::{Db, DbError};

/// Days of history kept.
pub const KEEP_DAYS: i64 = 60;

/// A focus session that ended; `amount` is its length in minutes.
pub const FOCUS: &str = "focus";
/// A todo that went from open to done; `text` is the todo.
pub const TODO_DONE: &str = "todo_done";
/// A reminder, timer or scheduled task that fired; `text` is its text.
pub const REMINDER: &str = "reminder";
/// A multi-step task she finished; `text` is its title, `amount` is 1 when it went well.
pub const TASK: &str = "task";

/// Longest text of one entry, characters.
const MAX_TEXT: usize = 200;

/// The local day old entries were last deleted on.
static PRUNED: Mutex<Option<NaiveDate>> = Mutex::new(None);

#[derive(Clone)]
pub struct DayLog(pub Arc<Db>);

impl std::fmt::Debug for DayLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DayLog")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayEntry {
    /// Unix seconds.
    pub at: i64,
    pub kind: String,
    pub text: String,
    pub amount: i64,
}

fn day_text(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// The local date of a unix time.
fn local_day(at: i64) -> NaiveDate {
    Local
        .timestamp_opt(at, 0)
        .earliest()
        .map(|t| t.date_naive())
        .unwrap_or_else(|| Local::now().date_naive())
}

impl DayLog {
    /// Writes an entry at the current time.
    pub fn add(&self, kind: &str, text: &str, amount: i64) -> Result<(), DbError> {
        self.add_at(Local::now().timestamp(), kind, text, amount)
    }

    /// Writes an entry at the unix time `at`. Once a local day it also forgets
    /// what is older than `KEEP_DAYS`.
    pub fn add_at(&self, at: i64, kind: &str, text: &str, amount: i64) -> Result<(), DbError> {
        let text: String = text.trim().chars().take(MAX_TEXT).collect();
        let day = day_text(local_day(at));
        self.0.with(|c| {
            c.execute(
                "INSERT INTO day_log (at, day, kind, text, amount) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![at, day, kind, text, amount],
            )
            .map(|_| ())
        })?;
        let today = Local::now().date_naive();
        let due = {
            let mut pruned = PRUNED.lock();
            let due = *pruned != Some(today);
            *pruned = Some(today);
            due
        };
        if due {
            if let Err(e) = self.prune(today) {
                log::warn!("day log not pruned: {e}");
            }
        }
        Ok(())
    }

    /// Entries of that day, oldest first.
    pub fn day(&self, day: NaiveDate) -> Result<Vec<DayEntry>, DbError> {
        self.0.with(|c| {
            let mut st =
                c.prepare("SELECT at, kind, text, amount FROM day_log WHERE day = ?1 ORDER BY id")?;
            let rows = st.query_map([day_text(day)], |r| {
                Ok(DayEntry {
                    at: r.get(0)?,
                    kind: r.get(1)?,
                    text: r.get(2)?,
                    amount: r.get(3)?,
                })
            })?;
            rows.collect()
        })
    }

    /// Forgets days older than `KEEP_DAYS` before `today`.
    pub fn prune(&self, today: NaiveDate) -> Result<usize, DbError> {
        let edge = day_text(today - Duration::days(KEEP_DAYS));
        self.0
            .with(|c| c.execute("DELETE FROM day_log WHERE day < ?1", [edge]))
    }

    pub fn clear(&self) -> Result<usize, DbError> {
        self.0.with(|c| c.execute("DELETE FROM day_log", []))
    }
}

/// Listens to the bus and writes fired reminders and finished tasks. Focus
/// sessions and done todos are written where they happen.
pub async fn run_recorder(log: DayLog, bus: EventBus) {
    use tokio::sync::broadcast::error::RecvError;
    let mut events = bus.subscribe();
    loop {
        let written = match events.recv().await {
            Ok(Event::ReminderFired { text, .. }) => log.add(REMINDER, &text, 0),
            Ok(Event::TaskUpdated { task }) if task.finished => {
                log.add(TASK, &task.title, task.ok as i64)
            }
            Ok(_) | Err(RecvError::Lagged(_)) => Ok(()),
            Err(RecvError::Closed) => break,
        };
        if let Err(e) = written {
            log::warn!("day log entry not written: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::ReminderKind;
    use crate::task::{Task, TaskStep};

    fn log() -> DayLog {
        DayLog(Arc::new(Db::open_in_memory().unwrap()))
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// Unix seconds of a local wall time.
    fn at(s: &str) -> i64 {
        let t = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        Local
            .from_local_datetime(&t)
            .earliest()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn entries_come_back_by_day_in_the_order_written() {
        let l = log();
        l.add_at(at("2026-11-14 09:00"), FOCUS, "", 25).unwrap();
        l.add_at(at("2026-11-14 10:30"), TODO_DONE, " Оплатить интернет ", 0)
            .unwrap();
        l.add_at(at("2026-11-13 23:59"), REMINDER, "Чайник", 0)
            .unwrap();
        let entries = l.day(day("2026-11-14")).unwrap();
        assert_eq!(
            entries,
            [
                DayEntry {
                    at: at("2026-11-14 09:00"),
                    kind: FOCUS.into(),
                    text: String::new(),
                    amount: 25
                },
                DayEntry {
                    at: at("2026-11-14 10:30"),
                    kind: TODO_DONE.into(),
                    text: "Оплатить интернет".into(),
                    amount: 0
                },
            ]
        );
        assert_eq!(l.day(day("2026-11-13")).unwrap().len(), 1);
        assert!(l.day(day("2026-11-12")).unwrap().is_empty());
        assert_eq!(format!("{l:?}"), "DayLog");
    }

    #[test]
    fn a_long_text_is_cut_to_200_characters() {
        let l = log();
        l.add_at(at("2026-11-14 09:00"), REMINDER, &"я".repeat(500), 0)
            .unwrap();
        let entries = l.day(day("2026-11-14")).unwrap();
        assert_eq!(entries[0].text.chars().count(), 200);
    }

    #[test]
    fn old_days_are_forgotten_and_clear_wipes_all() {
        let l = log();
        l.add_at(at("2026-11-14 09:00"), FOCUS, "", 5).unwrap();
        l.add_at(at("2026-09-15 09:00"), FOCUS, "", 5).unwrap();
        l.add_at(at("2026-09-14 09:00"), FOCUS, "", 5).unwrap();
        // 60 days before 14 November is 15 September: that day stays.
        assert_eq!(l.prune(day("2026-11-14")).unwrap(), 1);
        assert_eq!(l.day(day("2026-09-15")).unwrap().len(), 1);
        assert!(l.day(day("2026-09-14")).unwrap().is_empty());
        assert_eq!(l.clear().unwrap(), 2);
        assert!(l.day(day("2026-11-14")).unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_recorder_writes_fired_reminders_and_finished_tasks() {
        let l = log();
        let bus = EventBus::new(16);
        let job = tokio::spawn(run_recorder(l.clone(), bus.clone()));
        // The recorder subscribes when it is first polled.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let task = |finished: bool, ok: bool| Task {
            title: "Разобрать загрузки".into(),
            steps: Vec::<TaskStep>::new(),
            finished,
            ok,
        };
        bus.publish(Event::ReminderFired {
            id: 1,
            text: "Выключить чайник".into(),
            kind: ReminderKind::Remind,
            late_s: 0,
        });
        bus.publish(Event::ReminderFired {
            id: 2,
            text: "скажи время".into(),
            kind: ReminderKind::Task,
            late_s: 0,
        });
        bus.publish(Event::TaskUpdated {
            task: task(false, false),
        });
        bus.publish(Event::TaskUpdated {
            task: task(true, true),
        });
        bus.publish(Event::UserText { text: "x".into() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        job.abort();
        let entries = l.day(Local::now().date_naive()).unwrap();
        let seen: Vec<(&str, &str, i64)> = entries
            .iter()
            .map(|e| (e.kind.as_str(), e.text.as_str(), e.amount))
            .collect();
        assert_eq!(
            seen,
            [
                (REMINDER, "Выключить чайник", 0),
                (REMINDER, "скажи время", 0),
                (TASK, "Разобрать загрузки", 1),
            ]
        );
    }
}
