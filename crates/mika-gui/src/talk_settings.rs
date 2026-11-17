//! The port her settings change through by conversation (decision 0046).
//! Each change calls the command the settings page calls for it: the same
//! checks, the same side effects (the wake listener, the Telegram bridge, the
//! hotkeys), saved to `config.toml` the same way. Then every window is told,
//! so an open settings page draws the new value.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mika_core::config::{Persona, TtsEngine};
use mika_core::session::Origin;
use mika_core::settings::{self, SettingsPort, Value};
use parking_lot::Mutex;
use tauri::{AppHandle, Manager};

use crate::app::Ctx;
use crate::strings::tr;

/// The installed Windows voices are asked again only after this long.
const VOICES_FOR: Duration = Duration::from_secs(60);

pub struct TalkSettings {
    app: AppHandle,
    voices: Mutex<Option<(Instant, Vec<String>)>>,
}

impl TalkSettings {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            voices: Mutex::new(None),
        }
    }

    fn ctx(&self) -> tauri::State<'_, Arc<Ctx>> {
        self.app.state::<Arc<Ctx>>()
    }

    /// Names of the installed Windows voices, kept for a minute: listing
    /// them waits for Windows.
    fn windows_voices(&self) -> Vec<String> {
        let mut kept = self.voices.lock();
        if let Some((at, names)) = kept.as_ref() {
            if at.elapsed() < VOICES_FOR {
                return names.clone();
            }
        }
        let names: Vec<String> = self
            .ctx()
            .voice
            .voices()
            .into_iter()
            .map(|v| v.name)
            .collect();
        *kept = Some((Instant::now(), names.clone()));
        names
    }

    /// The level of her own sound in the Windows mixer.
    fn own_volume(&self) -> Result<u8, String> {
        let apps = self.ctx().tool_env().volume.apps()?;
        own_level(&apps, std::process::id()).ok_or_else(|| tr("error.talk_volume"))
    }
}

/// The level of the process `pid` among the programs of the mixer.
fn own_level(apps: &[mika_tools::sys::AppVolume], pid: u32) -> Option<u8> {
    apps.iter().find(|a| a.pid == pid).map(|a| a.level)
}

/// A persona by its id in the settings list.
fn persona(id: &str) -> Option<Persona> {
    serde_json::from_value(serde_json::json!(id)).ok()
}

#[async_trait]
impl SettingsPort for TalkSettings {
    fn read(&self, key: &str) -> Result<Value, String> {
        match key {
            "voice.volume" => self.own_volume().map(|level| Value::Number(level as f64)),
            "autostart" => Ok(Value::Bool(crate::platform::autostart_enabled())),
            key => settings::read(&self.ctx().config.lock(), key)
                .ok_or_else(|| tr("error.talk_setting")),
        }
    }

    fn options(&self, key: &str) -> Vec<String> {
        if key != "voice.voice" {
            return Vec::new();
        }
        let voice = self.ctx().config.lock().voice.clone();
        if voice.tts == TtsEngine::Provider {
            // The voices documented for the model: listing the provider's
            // own would wait for the network.
            return mika_providers::catalog::known_voices(&voice.tts_model)
                .map(|names| names.iter().map(|n| n.to_string()).collect())
                .unwrap_or_default();
        }
        self.windows_voices()
    }

    fn in_telegram(&self) -> bool {
        self.ctx().session.origin() == Origin::Telegram
    }

    async fn apply(&self, key: &str, value: &Value) -> Result<(), String> {
        use crate::settings as page;
        use crate::{bridge, costs, events_page, hotkey_actions, journal, lang};

        let app = self.app.clone();
        let ctx = self.ctx();
        let state = || app.state::<Arc<Ctx>>();
        let config = ctx.config.lock().clone();
        let unknown = || Err(tr("error.talk_setting"));
        match (key, value) {
            ("privacy.microphone", Value::Bool(on)) => {
                page::set_microphone_enabled(app.clone(), state(), *on)
            }
            ("voice.wake", Value::Bool(on)) => {
                page::set_wake(state(), *on, config.assistant.wake_phrase.clone()).map(|_| ())
            }
            ("dictation", Value::Bool(on)) => {
                hotkey_actions::dictation_set_enabled(app.clone(), state(), *on).map(|_| ())
            }
            ("privacy.screen", Value::Bool(on)) => {
                page::set_screen_enabled(app.clone(), state(), *on)
            }
            ("screen_ask", Value::Bool(on)) => {
                hotkey_actions::screen_ask_set_enabled(app.clone(), state(), *on).map(|_| ())
            }
            ("log.content", Value::Bool(on)) => {
                let mut log = config.log.clone();
                log.content = *on;
                journal::set_journal(state(), log).await.map(|_| ())
            }
            ("notifications", Value::Bool(on)) => {
                let n = &config.notifications;
                page::set_notifications(state(), *on, n.only_messengers, n.read_aloud)
            }
            ("notifications.read_aloud", Value::Bool(on)) => {
                let n = &config.notifications;
                page::set_notifications(state(), n.enabled, n.only_messengers, *on)
            }
            ("tracking.app_time", Value::Bool(on)) => {
                page::set_tracking(state(), *on, config.tracking.clipboard)
            }
            ("tracking.clipboard", Value::Bool(on)) => {
                page::set_tracking(state(), config.tracking.app_time, *on)
            }
            ("tools.web", Value::Bool(on)) => bridge::set_web_enabled(state(), *on),
            ("telegram", Value::Bool(on)) => {
                page::save_telegram(state(), *on, config.telegram.user_id, None).map(|_| ())
            }
            ("telegram.voice_in", Value::Bool(on)) => {
                page::set_telegram_voice(state(), *on, config.telegram.voice_reply).map(|_| ())
            }
            ("telegram.voice_reply", Value::Bool(on)) => {
                page::set_telegram_voice(state(), config.telegram.voice_in, *on).map(|_| ())
            }
            ("events", Value::Bool(on)) => events_page::set_events_enabled(state(), *on),
            ("autostart", Value::Bool(on)) => bridge::set_autostart(*on),
            ("voice.speak_replies", Value::Bool(on)) => {
                bridge::set_speak_replies(app.clone(), state(), *on)
            }
            ("voice.volume", Value::Number(level)) => {
                let level = level.round().clamp(0.0, 100.0) as u8;
                ctx.tool_env()
                    .volume
                    .set_app(std::process::id(), Some(level), None)
            }
            ("voice.rate", Value::Number(rate)) => {
                page::set_voice(state(), config.voice.voice.clone(), *rate as f32)
            }
            ("voice.voice", Value::Text(name)) if config.voice.tts == TtsEngine::Provider => {
                let v = &config.voice;
                page::save_speech_settings(
                    state(),
                    v.tts,
                    v.tts_model.clone(),
                    name.clone(),
                    v.stt_model.clone(),
                    v.stt_language.clone(),
                    v.speech_base_url.clone(),
                    v.input_device.clone(),
                    None,
                )
                .map(|_| ())
            }
            ("voice.voice", Value::Text(name)) => {
                page::set_voice(state(), name.clone(), config.voice.rate)
            }
            ("voice.brief_long", Value::Bool(on)) => page::set_brief_speech(state(), *on),
            ("voice.follow_up", Value::Bool(on)) => page::set_follow_up(state(), *on),
            ("voice.duck_media", Value::Bool(on)) => page::set_duck_media(state(), *on),
            ("voice.tone_by_mood", Value::Bool(on)) => {
                page::set_voice_style(state(), config.voice.tts_style.clone(), *on)
            }
            ("assistant.persona", Value::Text(id)) => match persona(id) {
                Some(p) => bridge::set_persona(
                    app.clone(),
                    state(),
                    p,
                    config.assistant.custom_prompt.clone(),
                ),
                None => unknown(),
            },
            ("rhythm.lively", Value::Bool(on)) => {
                let r = &config.rhythm;
                page::set_rhythm(state(), r.breaks, r.break_every_min, *on)
            }
            ("rhythm.breaks", Value::Bool(on)) => {
                let r = &config.rhythm;
                page::set_rhythm(state(), *on, r.break_every_min, r.lively)
            }
            ("rhythm.break_every_min", Value::Number(minutes)) => {
                let r = &config.rhythm;
                page::set_rhythm(state(), r.breaks, minutes.round() as u32, r.lively)
            }
            ("skills.suggest", Value::Bool(on)) => page::set_skill_offers(state(), *on),
            ("island.hide_in_games", Value::Bool(on)) => {
                page::set_island_settings(state(), config.island.city.clone(), *on)
            }
            ("avatar.in_island", Value::Bool(on)) => {
                bridge::set_avatar_placement(app.clone(), state(), *on, config.avatar.on_desktop)
                    .await
            }
            ("avatar.on_desktop", Value::Bool(on)) => {
                bridge::set_avatar_placement(app.clone(), state(), config.avatar.in_island, *on)
                    .await
            }
            ("i18n.language", Value::Text(code)) => {
                lang::set_language(app.clone(), state(), code.clone()).await
            }
            ("costs.day_limit", Value::Number(limit)) => {
                let limit = costs::check_limit(*limit).map_err(tr)?;
                costs::change(ctx.inner().clone(), move |c| c.day_limit = limit)
                    .await
                    .map(|_| ())
            }
            ("costs.month_limit", Value::Number(limit)) => {
                let limit = costs::check_limit(*limit).map_err(tr)?;
                costs::change(ctx.inner().clone(), move |c| c.month_limit = limit)
                    .await
                    .map(|_| ())
            }
            _ => unknown(),
        }?;
        bridge::settings_changed_by_talk(&app, key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_tools::sys::AppVolume;

    #[test]
    fn her_own_level_is_the_one_of_her_process() {
        let app = |pid, level| AppVolume {
            pid,
            process: "x.exe".into(),
            level,
            muted: false,
        };
        let apps = [app(10, 30), app(42, 75)];
        assert_eq!(own_level(&apps, 42), Some(75));
        assert_eq!(own_level(&apps, 7), None);
    }

    #[test]
    fn every_persona_of_the_list_is_known() {
        for id in ["caring", "cheerful", "calm", "bold"] {
            assert!(persona(id).is_some(), "{id}");
        }
        assert_eq!(persona("bold"), Some(Persona::Bold));
        assert_eq!(persona("grumpy"), None);
    }

    #[test]
    fn every_key_of_the_list_has_a_way_to_be_applied() {
        // The match of `apply` names each key: a key added to the list
        // without a way here would always fail.
        let source = include_str!("talk_settings.rs");
        for s in settings::SETTINGS {
            assert!(
                source.contains(&format!("(\"{}\", Value::", s.key)),
                "{} is not applied",
                s.key
            );
        }
    }
}
