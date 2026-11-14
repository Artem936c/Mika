//! The evening summary of the day: its settings and the summary itself
//! (decision 0028).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{Local, NaiveDateTime, NaiveTime};
use mika_core::AssistantState;

use crate::app::Ctx;
use crate::strings::tr;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// A summary is being made. One at a time: the button of the settings and
/// the evening check may meet, and the model would be asked twice.
static SUMMING: AtomicBool = AtomicBool::new(false);

/// The place of the summary being made; free again when dropped.
struct Turn;

impl Turn {
    /// `None` while another summary holds the place. A `Turn` is made only
    /// when the place was free: dropping one frees it.
    fn take() -> Option<Turn> {
        if SUMMING.swap(true, Ordering::AcqRel) {
            None
        } else {
            Some(Turn)
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        SUMMING.store(false, Ordering::Release);
    }
}

#[derive(serde::Serialize)]
pub struct EveningView {
    enabled: bool,
    /// Local `HH:MM` after which the summary comes.
    time: String,
}

fn view(ctx: &Ctx) -> EveningView {
    let c = ctx.config.lock();
    EveningView {
        enabled: c.briefing.evening_enabled,
        time: c.briefing.evening_time.clone(),
    }
}

/// The time of the summary as it is saved, when it reads as `HH:MM`.
pub fn summary_time(raw: &str) -> Option<(String, NaiveTime)> {
    let raw = raw.trim();
    let at = NaiveTime::parse_from_str(raw, "%H:%M").ok()?;
    Some((raw.to_string(), at))
}

/// The day to mark as already summed up when the switch goes from `was_on`
/// to `on`: turned on after its time, the summary waits for tomorrow instead
/// of coming at once.
pub fn skipped_day(was_on: bool, on: bool, at: NaiveTime, now: NaiveDateTime) -> Option<String> {
    (on && !was_on && now.time() >= at).then(|| now.date().format("%Y-%m-%d").to_string())
}

/// The summary as the chat shows it. The island and the avatar bubble show
/// only the first line of a remark: the head goes there, the text to the chat.
fn shown_text(text: &str) -> String {
    format!("{}\n{}", tr("day.head"), text.trim())
}

/// What sounds: the whole summary, or its first phrase when long replies are
/// retold briefly.
fn said_text(text: &str, brief: bool) -> String {
    if brief && text.chars().count() > mika_audio::speech::LONG_REPLY {
        mika_audio::speech::fallback_brief(text)
    } else {
        text.to_string()
    }
}

#[tauri::command]
pub fn get_evening_summary(ctx: CtxState<'_>) -> EveningView {
    view(&ctx)
}

/// The evening summary: on or off and its time, `HH:MM`.
#[tauri::command]
pub fn set_evening_summary(
    ctx: CtxState<'_>,
    enabled: bool,
    time: String,
) -> Result<EveningView, String> {
    let Some((time, at)) = summary_time(&time) else {
        return Err(tr("error.briefing_time"));
    };
    {
        let mut c = ctx.config.lock();
        let now = Local::now().naive_local();
        if let Some(day) = skipped_day(c.briefing.evening_enabled, enabled, at, now) {
            c.briefing.evening_last_date = day;
        }
        c.briefing.evening_enabled = enabled;
        c.briefing.evening_time = time;
    }
    ctx.save_config()?;
    Ok(view(&ctx))
}

/// Why no summary was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoSummary {
    /// Another summary is being made right now.
    Busy,
    /// The facts of the day could not be gathered.
    Failed(String),
}

/// What the button of the settings answers when no summary was made.
fn refusal_text(why: &NoSummary) -> String {
    match why {
        NoSummary::Busy => tr("error.evening_busy"),
        NoSummary::Failed(e) => tr("error.evening_failed").replace("{why}", e),
    }
}

/// The summary of today right now, whatever the switch and the time are.
/// The summary of the evening still comes at its time.
#[tauri::command]
pub async fn evening_summary_now(ctx: CtxState<'_>) -> Result<(), String> {
    let ctx = ctx.inner().clone();
    if ctx.state() != AssistantState::Idle {
        return Err(tr("error.evening_busy"));
    }
    ctx.evening_summary()
        .await
        .map_err(|why| refusal_text(&why))
}

impl Ctx {
    /// Sums up today: the facts of the day are gathered on this computer, the
    /// model puts them into a few sentences, and the result is her own line
    /// in the chat, aloud when replies are spoken. Without an answer of the
    /// model a plain line of the same facts is given. An error means that
    /// nothing was said.
    pub(crate) async fn evening_summary(self: &Arc<Self>) -> Result<(), NoSummary> {
        let Some(_turn) = Turn::take() else {
            return Err(NoSummary::Busy);
        };
        let env = mika_tools::day::DayEnv::new(self.tool_env());
        let now = Local::now().naive_local();
        let collected = tauri::async_runtime::spawn_blocking(move || {
            mika_tools::day::collect(&env, now.date(), now)
        })
        .await;
        let facts = match collected {
            Ok(Ok(facts)) => facts,
            Ok(Err(e)) => return Err(NoSummary::Failed(e.to_string())),
            Err(e) => return Err(NoSummary::Failed(e.to_string())),
        };
        // Nothing to tell: the model is not asked.
        if facts.is_empty() {
            self.remark(&tr("day.empty"), "day", true);
            return Ok(());
        }
        let persona = self.config.lock().assistant.persona_prompt();
        let provider = self.session.agent.provider();
        let text = mika_tools::day::evening_text(provider.as_ref(), &persona, &facts).await;
        let shown = shown_text(&text);
        self.remark_said(&shown, "day", Some(&said_text(&text, self.speaker.brief())));
        // In the conversation with what is planned, so «да, составь план» has
        // what to refer to. The titles of meetings and todos come from
        // outside: they go in marked as data, not as her own words.
        let note = match facts.tomorrow_note() {
            Some(tomorrow) => format!("{shown}\n{tomorrow}"),
            None => shown,
        };
        self.session.agent.append_note(&note);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn the_time_reads_as_hours_and_minutes() {
        let nine = NaiveTime::from_hms_opt(21, 0, 0).unwrap();
        assert_eq!(summary_time("21:00"), Some(("21:00".to_string(), nine)));
        assert_eq!(summary_time(" 21:00 "), Some(("21:00".to_string(), nine)));
        for bad in ["", "вечером", "25:00", "21:60", "21", "21:00:30", "9pm"] {
            assert_eq!(summary_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn turned_on_after_its_time_the_summary_waits_for_tomorrow() {
        let nine = NaiveTime::from_hms_opt(21, 0, 0).unwrap();
        // Turned on at 22:15: today is marked as done.
        assert_eq!(
            skipped_day(false, true, nine, at("2026-11-14 22:15")),
            Some("2026-11-14".to_string())
        );
        assert_eq!(
            skipped_day(false, true, nine, at("2026-11-14 21:00")),
            Some("2026-11-14".to_string())
        );
        // Turned on before its time: it comes today.
        assert_eq!(skipped_day(false, true, nine, at("2026-11-14 20:59")), None);
        // Already on (only the time was changed), or turned off: nothing is marked.
        assert_eq!(skipped_day(true, true, nine, at("2026-11-14 22:15")), None);
        assert_eq!(skipped_day(true, false, nine, at("2026-11-14 22:15")), None);
        assert_eq!(
            skipped_day(false, false, nine, at("2026-11-14 22:15")),
            None
        );
    }

    #[test]
    fn the_rule_agrees_with_the_check_of_the_loop() {
        let mut b = mika_core::config::BriefingConfig::default();
        let now = at("2026-11-14 22:15");
        let (time, nine) = summary_time("21:00").unwrap();
        if let Some(day) = skipped_day(b.evening_enabled, true, nine, now) {
            b.evening_last_date = day;
        }
        b.evening_enabled = true;
        b.evening_time = time;
        assert!(!b.evening_due(now));
        assert!(!b.evening_due(at("2026-11-15 20:00")));
        assert!(b.evening_due(at("2026-11-15 21:00")));
    }

    #[test]
    fn one_summary_is_made_at_a_time() {
        // The only test that takes the turn: the flag is one per process.
        let first = Turn::take();
        assert!(first.is_some());
        // A refused second one must not free the place of the first.
        assert!(Turn::take().is_none());
        assert!(Turn::take().is_none());
        drop(first);
        assert!(Turn::take().is_some());
        assert!(!SUMMING.load(Ordering::Acquire));
    }

    #[test]
    fn the_head_is_the_first_line_and_the_text_follows() {
        assert_eq!(
            shown_text(" Сегодня шесть часов за компьютером. \n"),
            "Итог дня:\nСегодня шесть часов за компьютером."
        );
    }

    #[test]
    fn a_long_summary_sounds_briefly_when_replies_are_retold() {
        let short = "Сегодня шесть часов за компьютером. Составить план на завтра?";
        assert_eq!(said_text(short, true), short);
        let long = format!("Первая фраза итога. {}", "Ещё одна фраза. ".repeat(30));
        assert!(long.chars().count() > mika_audio::speech::LONG_REPLY);
        assert_eq!(said_text(&long, false), long);
        let brief = said_text(&long, true);
        assert!(brief.starts_with("Первая фраза итога."), "{brief}");
        assert!(brief.chars().count() < long.chars().count());
    }

    #[test]
    fn the_texts_said_without_the_model_exist() {
        assert!(tr("error.evening_failed").contains("{why}"));
        assert_eq!(refusal_text(&NoSummary::Busy), tr("error.evening_busy"));
        let failed = refusal_text(&NoSummary::Failed("база занята".into()));
        assert!(failed.ends_with("база занята"), "{failed}");
        assert_ne!(failed, tr("error.evening_busy"));
        for key in [
            "day.empty",
            "day.head",
            "error.evening_busy",
            "error.evening_failed",
            "error.briefing_time",
            "costs.alert_day_80",
            "costs.alert_day_100",
            "costs.alert_month_80",
            "costs.alert_month_100",
        ] {
            assert_ne!(tr(key), key, "{key} is missing in ru.json");
        }
        // The island shows the first line of a remark, and turns a final
        // colon into a period.
        assert!(tr("day.head").ends_with(':') && !tr("day.head").contains('\n'));
        assert_eq!(tr("day.empty").lines().count(), 2);
        for key in [
            "costs.alert_day_80",
            "costs.alert_day_100",
            "costs.alert_month_80",
            "costs.alert_month_100",
        ] {
            let text = tr(key);
            assert!(
                text.contains("{spent}") && text.contains("{limit}"),
                "{key}"
            );
            assert_eq!(text.contains("{share}"), key.ends_with("_80"), "{key}");
        }
    }
}
