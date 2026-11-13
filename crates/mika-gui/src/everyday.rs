//! Background helpers of every day: notifications of other programs, chats she
//! keeps up for the user, the morning summary.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mika_core::config::CALENDAR_SECRET_KEY;
use mika_core::AssistantState;
use mika_tools::notify::{is_messenger, is_private, Toast, ToastReader};

use crate::app::Ctx;

/// How often the notification database is read.
const NOTIFY_EVERY: Duration = Duration::from_secs(2);
/// How often the summary time is checked.
const BRIEFING_EVERY: Duration = Duration::from_secs(30);
/// How long the summary waits for the first read of the calendar link.
const CALENDAR_WAIT: Duration = Duration::from_secs(300);
/// The summary waits for someone at the computer: input within this time.
const PRESENT_S: u64 = 120;
/// Longest text said aloud for one notification, characters.
const SPOKEN_CHARS: usize = 220;

/// Whether this notification is shown to her: not her own, not private, and
/// from a messenger when the user asked for messengers only.
pub fn wanted(t: &Toast, only_messengers: bool) -> bool {
    let own = t.app_id.to_lowercase();
    if own.contains("dev.mika.assistant") || own.ends_with("mika.exe") {
        return false;
    }
    !is_private(t) && (!only_messengers || is_messenger(&t.app_id))
}

/// One line for the chat and the conversation.
pub fn notice_line(t: &Toast) -> String {
    if t.text.is_empty() {
        format!("{} — {}", t.app, t.title)
    } else {
        format!("{} — {}: {}", t.app, t.title, t.text)
    }
}

/// What she says aloud.
pub fn spoken(t: &Toast) -> String {
    let line = if t.text.is_empty() {
        format!("{}: {}", t.app, t.title)
    } else {
        format!("{}, {}: {}", t.app, t.title, t.text)
    };
    let mut out: String = line.chars().take(SPOKEN_CHARS).collect();
    if line.chars().count() > SPOKEN_CHARS {
        out.push('…');
    }
    out
}

/// The task of the morning summary.
pub fn briefing_task(city: &str, calendar: bool) -> String {
    let weather = if city.trim().is_empty() {
        "погода (спроси город, если не знаешь)".to_string()
    } else {
        format!("погода на сегодня в городе {}", city.trim())
    };
    let meetings = if calendar {
        "встречи из календаря на сегодня (calendar_events); "
    } else {
        ""
    };
    format!(
        "Утренняя сводка. Коротко и тепло, по пунктам: поздоровайся; {weather}; {meetings}дела на сегодня (todo_list) и напоминания (reminder_list); \
курс доллара и евро; три главные новости по одной строке. Если что-то не узнать, пропусти без извинений. Вопросов в конце не задавай."
    )
}

/// Whether the summary lists meetings, or `None` while it waits for the first
/// read of the calendar link: at start-up the calendar is read a little later
/// than the summary is due. After `CALENDAR_WAIT` it goes without meetings.
pub(crate) fn briefing_calendar(
    calendar_on: bool,
    read_once: bool,
    waited: Duration,
) -> Option<bool> {
    if !calendar_on || read_once {
        return Some(calendar_on);
    }
    (waited >= CALENDAR_WAIT).then_some(false)
}

/// Seconds since the last key press or mouse move in this session.
pub(crate) fn idle_seconds() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: fills a plain struct owned here.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return 0;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetTickCount() };
    (now.wrapping_sub(info.dwTime) / 1000) as u64
}

impl Ctx {
    /// Reads new notifications while the user allows it.
    pub fn start_notifications(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            let reader = Arc::new(ToastReader::new(mika_tools::notify::default_database()));
            // Notifications that came before she started listening are old news.
            let mut last: Option<i64> = None;
            let mut warned = false;
            loop {
                tokio::time::sleep(NOTIFY_EVERY).await;
                let Some(ctx) = me.upgrade() else { break };
                let cfg = ctx.config.lock().notifications.clone();
                if !cfg.enabled {
                    last = None;
                    continue;
                }
                let r = reader.clone();
                let got = tauri::async_runtime::spawn_blocking(move || match last {
                    None => r.last_id().map(|id| (id, Vec::new())),
                    Some(after) => r
                        .after(after)
                        .map(|list| (list.last().map_or(after, |t| t.id), list)),
                })
                .await;
                let (newest, toasts) = match got {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => {
                        if !warned {
                            log::warn!("notifications are unavailable: {e}");
                            warned = true;
                        }
                        continue;
                    }
                    Err(_) => continue,
                };
                warned = false;
                last = Some(newest);
                for t in toasts {
                    if wanted(&t, cfg.only_messengers) {
                        ctx.on_notification(&t, cfg.read_aloud);
                    }
                }
            }
        });
    }

    fn on_notification(self: &Arc<Self>, t: &Toast, read_aloud: bool) {
        log::info!("notification from {}", t.app);
        let at = chrono::Local::now().timestamp();
        let watched = (t.app == "Telegram")
            .then(|| self.tool_env().shared.watches.matching(&t.title, at))
            .flatten();
        if let Some(w) = watched {
            self.publish_notification(t);
            let task = mika_tools::watch::reply_task(&w, &t.app, &t.text);
            self.session.submit_task(&task);
            return;
        }
        // During focus it waits for the summary.
        if self.tool_env().shared.focus.hold(&notice_line(t), at) {
            return;
        }
        self.publish_notification(t);
        // Muted Windows hears nothing: the synthesizer is not asked for silence.
        if read_aloud && crate::rhythm::speaks(&self.speaker) {
            self.voice.speak(&spoken(t));
        }
    }

    fn publish_notification(&self, t: &Toast) {
        self.bus.publish(mika_core::Event::Notification {
            app: t.app.clone(),
            title: t.title.clone(),
            text: t.text.clone(),
        });
        // In the conversation, so «ответь ему» knows whom.
        self.session
            .agent
            .append_note(&format!("🔔 Уведомление: {}", notice_line(t)));
    }

    /// Gives the morning summary once a day, when someone is at the computer.
    pub fn start_briefing(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            let mut waiting: Option<Instant> = None;
            loop {
                tokio::time::sleep(BRIEFING_EVERY).await;
                let Some(ctx) = me.upgrade() else { break };
                let now = chrono::Local::now().naive_local();
                let (due, city, calendar_on) = {
                    let c = ctx.config.lock();
                    (
                        c.briefing.due(now),
                        c.island.city.clone(),
                        c.calendar.enabled,
                    )
                };
                let focused = ctx
                    .tool_env()
                    .shared
                    .focus
                    .active(chrono::Local::now().timestamp());
                if !due
                    || focused
                    || idle_seconds() > PRESENT_S
                    || ctx.state() != AssistantState::Idle
                {
                    continue;
                }
                // The same test as the calendar reader's: on, with a link.
                let calendar_on = calendar_on
                    && ctx
                        .secrets
                        .get(CALENDAR_SECRET_KEY)
                        .ok()
                        .flatten()
                        .is_some_and(|s| !s.expose().trim().is_empty());
                let waited = waiting.get_or_insert_with(Instant::now).elapsed();
                let read_once = ctx.tool_env().shared.calendar.connected();
                let Some(calendar) = briefing_calendar(calendar_on, read_once, waited) else {
                    continue;
                };
                waiting = None;
                ctx.config.lock().briefing.last_date = now.date().format("%Y-%m-%d").to_string();
                if let Err(e) = ctx.save_config() {
                    log::warn!("cannot save config: {e}");
                }
                log::info!("morning summary");
                ctx.session.submit_task(&briefing_task(&city, calendar));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toast(app_id: &str, title: &str, text: &str) -> Toast {
        Toast {
            id: 1,
            app_id: app_id.into(),
            app: mika_tools::notify::app_name(app_id),
            title: title.into(),
            text: text.into(),
        }
    }

    #[test]
    fn her_own_private_and_unwanted_notifications_are_skipped() {
        let tg = "TelegramDesktop";
        assert!(wanted(&toast(tg, "Аня", "привет"), true));
        assert!(!wanted(
            &toast("dev.mika.assistant", "Мика", "напоминание"),
            false
        ));
        assert!(!wanted(&toast(tg, "Telegram", "Код входа 12345"), true));
        assert!(!wanted(
            &toast("Microsoft.Windows.Update", "Обновление", "готово"),
            true
        ));
        assert!(wanted(
            &toast("Microsoft.Windows.Update", "Обновление", "готово"),
            false
        ));
    }

    #[test]
    fn lines_and_speech_are_short() {
        let t = toast("TelegramDesktop", "Аня", &"а".repeat(500));
        assert!(notice_line(&t).starts_with("Telegram — Аня: ааа"));
        let s = spoken(&t);
        assert!(s.ends_with('…') && s.chars().count() == SPOKEN_CHARS + 1);
        assert!(briefing_task("Пермь", false).contains("в городе Пермь"));
        assert!(briefing_task("", false).contains("спроси город"));
        assert!(!briefing_task("", false).contains("calendar_events"));
        assert!(briefing_task("", true)
            .contains("встречи из календаря на сегодня (calendar_events); дела"));
    }

    #[test]
    fn the_summary_waits_for_the_first_calendar_read() {
        let start = Duration::ZERO;
        // On, with a link, not read yet: wait instead of going without meetings.
        assert_eq!(briefing_calendar(true, false, start), None);
        assert_eq!(
            briefing_calendar(true, false, CALENDAR_WAIT - Duration::from_secs(1)),
            None
        );
        assert_eq!(briefing_calendar(true, true, start), Some(true));
        assert_eq!(briefing_calendar(false, false, start), Some(false));
        assert_eq!(briefing_calendar(false, true, start), Some(false));
        // The link never answers: the summary goes on its own.
        assert_eq!(briefing_calendar(true, false, CALENDAR_WAIT), Some(false));
    }
}
