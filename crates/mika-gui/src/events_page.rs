//! UI commands for scenarios by event and for what she is watching for
//! (decision 0031). They stand on the extensions page.

use std::sync::Arc;

use mika_core::config::EventsConfig;
use mika_core::triggers::{text_mark, Trigger, TriggerAction};
use mika_tools::observe::WaitView;

use crate::app::Ctx;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// A scenario as the page draws it.
#[derive(serde::Serialize)]
pub struct TriggerView {
    #[serde(flatten)]
    trigger: Trigger,
    /// Runs on the day the page is opened.
    fired_today: u32,
    /// The saved command it runs was deleted: the next event switches the scenario off.
    command_missing: bool,
    /// The saved command no longer says what was confirmed when the scenario
    /// was created: it does not run, and the next event switches it off.
    command_changed: bool,
    /// What the saved command says on this computer now. A scenario runs it
    /// when nobody is looking, so the page shows it whole next to the switch:
    /// a scenario restored from a backup comes switched off, with the mark of
    /// a text that was confirmed on another computer (decision 0034), and
    /// the name of the command alone does not say what it will do.
    #[serde(skip_serializing_if = "Option::is_none")]
    command_text: Option<String>,
}

#[derive(serde::Serialize)]
pub struct EventsView {
    enabled: bool,
    max_per_hour: u32,
    triggers: Vec<TriggerView>,
    waits: Vec<WaitView>,
}

/// `command_text` gives the text of a saved command by its name.
pub(crate) fn view(
    cfg: &EventsConfig,
    triggers: Vec<Trigger>,
    command_text: impl Fn(&str) -> Option<String>,
    waits: Vec<WaitView>,
    today: &str,
) -> EventsView {
    let triggers = triggers
        .into_iter()
        .map(|trigger| {
            let (command_missing, command_changed, command_text) = match &trigger.action {
                TriggerAction::Command(name) => match command_text(name) {
                    Some(text) => (false, text_mark(&text) != trigger.pin, Some(text)),
                    None => (true, false, None),
                },
                TriggerAction::Instruction(_) => (false, false, None),
            };
            TriggerView {
                fired_today: trigger.fired_today(today),
                command_missing,
                command_changed,
                command_text,
                trigger,
            }
        })
        .collect();
    EventsView {
        enabled: cfg.enabled,
        // The limit the watcher really applies, whatever stands in config.toml.
        max_per_hour: cfg.max_per_hour.clamp(1, 60),
        triggers,
        waits,
    }
}

#[tauri::command]
pub fn get_events(ctx: CtxState<'_>) -> Result<EventsView, String> {
    let shared = &ctx.tool_env().shared;
    let triggers = shared.triggers.list().map_err(|e| e.to_string())?;
    let cfg = ctx.config.lock().events.clone();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    Ok(view(
        &cfg,
        triggers,
        |name| ctx.commands.find(name).map(|command| command.prompt),
        shared.waits.list(),
        &today,
    ))
}

/// Off puts every scenario on pause; the watches go on.
#[tauri::command]
pub fn set_events_enabled(ctx: CtxState<'_>, value: bool) -> Result<(), String> {
    ctx.config.lock().events.enabled = value;
    ctx.save_config()
}

#[tauri::command]
pub fn set_trigger_enabled(ctx: CtxState<'_>, id: i64, enabled: bool) -> Result<bool, String> {
    ctx.tool_env()
        .shared
        .triggers
        .set_enabled(id, enabled)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_trigger(ctx: CtxState<'_>, id: i64) -> Result<bool, String> {
    ctx.tool_env()
        .shared
        .triggers
        .delete(id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn stop_wait(ctx: CtxState<'_>, id: u32) -> bool {
    ctx.tool_env().shared.waits.stop(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::triggers::TriggerEvent;

    fn trigger(id: i64, name: &str, action: TriggerAction) -> Trigger {
        Trigger {
            id,
            name: name.into(),
            event: TriggerEvent::AudioConnected,
            filter: "sony".into(),
            idle_min: 0,
            action,
            enabled: true,
            cooldown_s: 60,
            max_per_day: 20,
            fired_at: 1_794_650_000,
            fired_day: "2026-11-14".into(),
            fired_count: 2,
            pin: text_mark(TEXT),
        }
    }

    /// The text every saved command of these tests has.
    const TEXT: &str = "Включи плеер";

    fn all(_: &str) -> Option<String> {
        Some(TEXT.into())
    }

    fn sample() -> Vec<Trigger> {
        vec![
            trigger(3, "Наушники", TriggerAction::Command("музыка".into())),
            trigger(4, "Вернулся", TriggerAction::Command("сводка".into())),
            trigger(
                5,
                "После игры",
                TriggerAction::Instruction("Верни громкость".into()),
            ),
        ]
    }

    #[test]
    fn todays_runs_are_counted_only_for_today() {
        let cfg = EventsConfig::default();
        let today = view(&cfg, sample(), all, Vec::new(), "2026-11-14");
        assert!(today.triggers.iter().all(|t| t.fired_today == 2));
        let next_day = view(&cfg, sample(), all, Vec::new(), "2026-11-15");
        assert!(next_day.triggers.iter().all(|t| t.fired_today == 0));
        // What the store counted is passed on as it is.
        assert!(next_day.triggers.iter().all(|t| t.trigger.fired_count == 2));
    }

    #[test]
    fn a_missing_command_is_marked_and_an_instruction_never_is() {
        let v = view(
            &EventsConfig::default(),
            sample(),
            |name| (name == "музыка").then(|| TEXT.to_string()),
            Vec::new(),
            "2026-11-14",
        );
        let missing: Vec<(i64, bool)> = v
            .triggers
            .iter()
            .map(|t| (t.trigger.id, t.command_missing))
            .collect();
        assert_eq!(missing, vec![(3, false), (4, true), (5, false)]);
        // With no command at all an instruction is still fine.
        let none = view(
            &EventsConfig::default(),
            sample(),
            |_| None,
            Vec::new(),
            "2026-11-14",
        );
        assert!(!none.triggers[2].command_missing);
        assert!(none.triggers.iter().all(|t| !t.command_changed));
        // A command that is not there has no text to show.
        assert!(none.triggers.iter().all(|t| t.command_text.is_none()));
    }

    #[test]
    fn a_command_with_another_text_than_was_confirmed_is_marked() {
        let other = "Удали загрузки";
        let v = view(
            &EventsConfig::default(),
            sample(),
            |name| Some(if name == "сводка" { other } else { TEXT }.to_string()),
            Vec::new(),
            "2026-11-14",
        );
        // The page shows what each saved command says now, whole: the text
        // that was confirmed and the one that was not. An instruction is its
        // own text and has none besides.
        let texts: Vec<Option<&str>> = v
            .triggers
            .iter()
            .map(|t| t.command_text.as_deref())
            .collect();
        assert_eq!(texts, vec![Some(TEXT), Some(other), None]);
        let changed: Vec<(i64, bool, bool)> = v
            .triggers
            .iter()
            .map(|t| (t.trigger.id, t.command_changed, t.command_missing))
            .collect();
        assert_eq!(
            changed,
            vec![(3, false, false), (4, true, false), (5, false, false)]
        );
    }

    #[test]
    fn the_switch_and_the_hour_limit_come_from_the_settings() {
        let cfg = EventsConfig {
            enabled: false,
            max_per_hour: 7,
        };
        let v = view(&cfg, Vec::new(), all, Vec::new(), "2026-11-14");
        assert!(!v.enabled);
        assert_eq!(v.max_per_hour, 7);
        // A value the watcher would not take is shown as the one it applies.
        for (written, applied) in [(0, 1), (500, 60)] {
            let cfg = EventsConfig {
                enabled: true,
                max_per_hour: written,
            };
            let v = view(&cfg, Vec::new(), all, Vec::new(), "2026-11-14");
            assert_eq!(v.max_per_hour, applied);
        }
    }

    #[test]
    fn the_page_gets_the_json_it_expects() {
        let waits = vec![WaitView {
            id: 2,
            kind: "process_exits",
            what: "cargo.exe".into(),
            since: 1_794_650_000,
            until: 1_794_653_600,
        }];
        let v = view(
            &EventsConfig::default(),
            vec![trigger(
                3,
                "Наушники",
                TriggerAction::Command("музыка".into()),
            )],
            all,
            waits,
            "2026-11-14",
        );
        assert_eq!(
            serde_json::to_value(&v).unwrap(),
            serde_json::json!({
                "enabled": true,
                "max_per_hour": 12,
                "triggers": [{
                    "id": 3,
                    "name": "Наушники",
                    "event": "audio_connected",
                    "filter": "sony",
                    "idle_min": 0,
                    "action": {"kind": "command", "text": "музыка"},
                    "enabled": true,
                    "cooldown_s": 60,
                    "max_per_day": 20,
                    "fired_at": 1_794_650_000_i64,
                    "fired_day": "2026-11-14",
                    "fired_count": 2,
                    "fired_today": 2,
                    "command_missing": false,
                    "command_changed": false,
                    "command_text": TEXT
                }],
                "waits": [{
                    "id": 2,
                    "kind": "process_exits",
                    "what": "cargo.exe",
                    "since": 1_794_650_000_i64,
                    "until": 1_794_653_600_i64
                }]
            })
        );
    }
}
