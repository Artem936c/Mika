//! Her own settings by conversation (decision 0046): «говори тише»,
//! «включи активационную фразу», «какие у тебя настройки?». The whitelist and
//! the risk of each change live in `mika_core::settings`; the application
//! applies a change through its port, the same way its settings page does.

use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::settings::{self, Setting, SettingsPort, SettingsSlot, Value, SETTINGS};
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::{json, Value as Json};
use tokio_util::sync::CancellationToken;

use crate::util::schema;

const NO_PORT: &str = "менять настройки разговором можно только в приложении Мики";

fn port(slot: &SettingsSlot) -> Result<Arc<dyn SettingsPort>, ToolError> {
    slot.get().ok_or_else(|| ToolError::Failed(NO_PORT.into()))
}

/// The settings she may change, with their values now.
pub struct SettingsGetTool(pub SettingsSlot);

#[async_trait]
impl Tool for SettingsGetTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "settings_get",
            "Её собственные настройки, которые можно менять разговором: ключ, что сейчас стоит, что можно поставить и что это. Вызывай, когда спрашивают «какие у тебя настройки», «какой у тебя голос», «включён ли микрофон», и перед settings_set, если не знаешь ключа, вариантов или текущего значения. Ключей, адресов и паролей тут нет.",
            schema(&[], &[]),
        )
    }

    fn assess(&self, _args: &Json) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть свои настройки",
            "настройки",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Json,
        _approval: Approval,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let port = port(&self.0)?;
        let mut lines = vec![
            "Настройки, которые можно менять разговором (ключ: сейчас; можно; что это):"
                .to_string(),
        ];
        let mut data = Vec::new();
        for s in SETTINGS {
            let options = port.options(s.key);
            let now = match port.read(s.key) {
                Ok(value) => {
                    data.push(json!({ "key": s.key, "value": value.to_json() }));
                    s.shown(&value)
                }
                Err(why) => format!("не узнать ({why})"),
            };
            lines.push(format!(
                "- {}: {now}; {}; {}",
                s.key,
                s.allowed(&options),
                s.about
            ));
        }
        lines.push(
            "Ключи провайдеров, адреса, модели, папки, сочетания клавиш, копии и удаление данных разговором не меняются: только в окне настроек."
                .into(),
        );
        Ok(ToolOutput::new(lines.join("\n")).with_data(json!({ "settings": data })))
    }
}

/// Changes one setting of the whitelist.
pub struct SettingsSetTool(pub SettingsSlot);

/// What a call changes: the setting, its value now and the value asked for.
struct Change {
    setting: &'static Setting,
    old: Value,
    new: Value,
}

impl SettingsSetTool {
    fn change(&self, port: &dyn SettingsPort, args: &Json) -> Result<Change, ToolError> {
        let key = arg_str(args, "key")?;
        let setting = settings::find(key).ok_or_else(|| {
            ToolError::BadArguments(format!(
                "настройку «{}» разговором менять нельзя; можно: {}",
                key.trim(),
                settings::keys()
            ))
        })?;
        let raw = args
            .get("value")
            .filter(|v| !v.is_null())
            .ok_or_else(|| ToolError::BadArguments("нужно значение value".into()))?;
        let old = port.read(setting.key).map_err(ToolError::Failed)?;
        let options = port.options(setting.key);
        let new = setting
            .parse(raw, &old, &options)
            .map_err(ToolError::BadArguments)?;
        if new == old {
            return Err(ToolError::Failed(format!(
                "менять нечего: «{}» уже {}",
                setting.key,
                setting.shown(&old)
            )));
        }
        // What she hears, sees and writes down is turned on at the computer.
        if setting.at_pc_only && setting.turns_on(&old, &new) && port.in_telegram() {
            return Err(ToolError::Failed(format!(
                "включить {} можно только за компьютером, не из Telegram",
                setting.name
            )));
        }
        // What needs another switch on says so; the other one is not turned
        // on along the way, it asks for itself (decision 0054).
        if let Some(other) = settings::needs(setting.key) {
            let off = port.read(other.key).map_err(ToolError::Failed)? != Value::Bool(true);
            if setting.turns_on(&old, &new) && off {
                return Err(ToolError::Failed(format!(
                    "сначала нужно включить {} ({}): без него {} не работает. Спроси пользователя, включить ли его; это отдельная просьба со своим подтверждением",
                    other.name, other.key, setting.name
                )));
            }
        }
        Ok(Change { setting, old, new })
    }
}

#[async_trait]
impl Tool for SettingsSetTool {
    fn spec(&self) -> ToolSpec {
        let keys = format!("one of: {}", settings::keys());
        spec(
            "settings_set",
            "Поменять одну свою настройку по просьбе пользователя, сразу и так же, как в окне настроек. «Говори тише» или «громче» — voice.volume со сдвигом «-15» или «+15» (это её голос; громкость всего компьютера меняет volume). «Говори быстрее» или «медленнее» — voice.rate, «+0.2» или «-0.2». «Смени голос на …» — voice.voice. «Не читай ответы вслух» — voice.speak_replies. «Длинное пересказывай вслух короче» — voice.brief_long. «Включи» или «выключи» активационную фразу — voice.wake, диктовку — dictation, микрофон — privacy.microphone, просмотр экрана — privacy.screen. Характер — assistant.persona, язык интерфейса — i18n.language, перерывы — rhythm.breaks и rhythm.break_every_min, лимиты расходов — costs.day_limit и costs.month_limit. «Посмотри, как я играю» — companion.enabled вкл, «хватит комментировать» — выкл; как часто она смотрит — companion.every_s. Варианты и текущие значения даёт settings_get. Ключи провайдеров, адреса, модели, папки и сочетания клавиш так не меняются.",
            schema(
                &[
                    ("key", "string", &keys),
                    (
                        "value",
                        "string",
                        "Новое значение: «вкл» или «выкл», число, сдвиг от текущего «+10» или «-0.2», вариант из settings_get",
                    ),
                ],
                &["key", "value"],
            ),
        )
    }

    fn assess(&self, args: &Json) -> Result<Assessment, ToolError> {
        let port = port(&self.0)?;
        let change = self.change(port.as_ref(), args)?;
        Ok(change.setting.assess(&change.old, &change.new))
    }

    async fn run(
        &self,
        args: Json,
        _approval: Approval,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let port = port(&self.0)?;
        let Change { setting, old, new } = self.change(port.as_ref(), &args)?;
        let remote = port.in_telegram();
        port.apply(setting.key, &new).await.map_err(|why| {
            log::warn!(
                target: "app",
                "setting not changed by conversation: key={} telegram={remote} error={why}",
                setting.key
            );
            ToolError::Failed(why)
        })?;
        let (was, now) = (setting.shown(&old), setting.shown(&new));
        // The whitelist holds nothing secret: the values may be written.
        log::info!(
            target: "app",
            "setting changed by conversation: key={} from={was} to={now} telegram={remote}",
            setting.key
        );
        Ok(ToolOutput::new(format!(
            "Готово: {} — было {was}, стало {now}.",
            setting.key
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::config::Config;
    use parking_lot::Mutex;

    /// A port over a config of its own; what is applied is written into it.
    #[derive(Default)]
    struct FakePort {
        config: Mutex<Config>,
        volume: Mutex<f64>,
        telegram: bool,
        applied: Mutex<Vec<(String, Value)>>,
        fails: bool,
    }

    #[async_trait]
    impl SettingsPort for FakePort {
        fn read(&self, key: &str) -> Result<Value, String> {
            match key {
                "voice.volume" => Ok(Value::Number(*self.volume.lock())),
                "autostart" => Ok(Value::Bool(false)),
                key => settings::read(&self.config.lock(), key).ok_or("unknown".into()),
            }
        }

        fn options(&self, key: &str) -> Vec<String> {
            match key {
                "voice.voice" => vec!["Microsoft Irina".into(), "Microsoft Pavel".into()],
                key => settings::options(&self.config.lock(), key),
            }
        }

        fn in_telegram(&self) -> bool {
            self.telegram
        }

        async fn apply(&self, key: &str, value: &Value) -> Result<(), String> {
            if self.fails {
                return Err("не сохранить настройки".into());
            }
            self.applied.lock().push((key.to_string(), value.clone()));
            let mut c = self.config.lock();
            match (key, value) {
                ("voice.volume", Value::Number(n)) => *self.volume.lock() = *n,
                ("voice.wake", Value::Bool(b)) => c.voice.wake_enabled = *b,
                ("privacy.microphone", Value::Bool(b)) => c.privacy.microphone_enabled = *b,
                ("assistant.adult", Value::Bool(b)) => c.assistant.adult = *b,
                ("companion.enabled", Value::Bool(b)) => c.companion.enabled = *b,
                ("privacy.screen", Value::Bool(b)) => c.privacy.screen_enabled = *b,
                ("assistant.persona", Value::Text(choice)) => c
                    .assistant
                    .choose_persona(choice)
                    .map_err(|e| e.to_string())?,
                _ => {}
            }
            Ok(())
        }
    }

    fn slot(port: FakePort) -> (SettingsSlot, Arc<FakePort>) {
        let port = Arc::new(port);
        let slot = SettingsSlot::default();
        slot.set(port.clone());
        (slot, port)
    }

    async fn run(tool: &dyn Tool, args: Json) -> Result<ToolOutput, ToolError> {
        tool.assess(&args)?;
        tool.run(args, Approval::for_tests(), CancellationToken::new())
            .await
    }

    #[tokio::test]
    async fn get_lists_every_setting_with_its_value() {
        let mut config = Config::default();
        config.voice.rate = 1.3;
        let (slot, _) = slot(FakePort {
            config: Mutex::new(config),
            volume: Mutex::new(70.0),
            ..Default::default()
        });
        let tool = SettingsGetTool(slot);
        assert_eq!(tool.assess(&json!({})).unwrap().level, RiskLevel::Low);
        let out = run(&tool, json!({})).await.unwrap();
        for s in SETTINGS {
            assert!(out.text.contains(&format!("- {}: ", s.key)), "{}", s.key);
        }
        assert!(out.text.contains("- voice.volume: 70%"));
        assert!(out.text.contains("- voice.rate: 1.3"));
        assert!(out.text.contains("Microsoft Pavel"));
    }

    #[tokio::test]
    async fn get_never_shows_secrets() {
        let mut config = Config::default();
        config.llm.base_url = "https://secret-host.example/v1".into();
        config.llm.model = "secret-model".into();
        config
            .assistant
            .save_custom_persona(None, "Котёнок", "секретный характер")
            .unwrap();
        config.telegram.user_id = 987654321;
        config.updates.manifest_url = "https://updates.example/m.json".into();
        config.research.folder = r"C:\Тайная папка".into();
        config.docs.folders = vec![r"D:\Документы".into()];
        config
            .hotkeys
            .custom
            .insert("listen".into(), "Ctrl+Alt+Q".into());
        let (slot, _) = slot(FakePort {
            config: Mutex::new(config),
            ..Default::default()
        });
        let out = run(&SettingsGetTool(slot), json!({})).await.unwrap();
        let all = format!("{}{}", out.text, out.data.unwrap());
        for secret in [
            "secret-host",
            "secret-model",
            "секретный",
            "987654321",
            "updates.example",
            "Тайная",
            "Документы",
            "Ctrl+Alt+Q",
            "api_key",
            "bot_token",
        ] {
            assert!(!all.contains(secret), "{secret} is shown");
        }
    }

    #[tokio::test]
    async fn set_applies_the_change_through_the_port() {
        let (slot, port) = slot(FakePort {
            volume: Mutex::new(80.0),
            ..Default::default()
        });
        let tool = SettingsSetTool(slot);
        let args = json!({"key": "voice.volume", "value": "-15"});
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert_eq!(a.summary, "Изменить громкость голоса");
        assert_eq!(a.target, "было: 80% → станет: 65%");
        let out = run(&tool, args).await.unwrap();
        assert_eq!(out.text, "Готово: voice.volume — было 80%, стало 65%.");
        assert_eq!(
            port.applied.lock().as_slice(),
            &[("voice.volume".to_string(), Value::Number(65.0))]
        );
    }

    #[tokio::test]
    async fn turning_on_asks_every_time_and_off_is_low() {
        let (slot, port) = slot(FakePort::default());
        let tool = SettingsSetTool(slot);
        let on = json!({"key": "voice.wake", "value": "вкл"});
        let a = tool.assess(&on).unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(a.summary, "Включить активационную фразу");
        assert_eq!(a.target, "было: выкл → станет: вкл");
        // High risk is never allowed forever, whatever the answer was.
        assert_eq!(
            mika_core::policy::forever_block_for(&a),
            Some(mika_core::policy::HIGH_RISK_FOREVER)
        );
        run(&tool, on).await.unwrap();
        assert!(port.config.lock().voice.wake_enabled);
        let off = tool
            .assess(&json!({"key": "voice.wake", "value": false}))
            .unwrap();
        assert_eq!(off.level, RiskLevel::Low);
        assert!(mika_core::policy::forever_block_for(&off).is_none());
    }

    #[tokio::test]
    async fn high_risk_goes_through_the_policy_and_cannot_be_allowed_forever() {
        use mika_core::confirm::{Answer, Confirmations, DecidedBy};
        use mika_core::gate::{Gate, GateDecision};
        use mika_core::policy::{AuditLog, PolicyGate, RuleDecision, RuleStore};
        use mika_core::{EventBus, StateHandle};

        let (slot, _) = slot(FakePort::default());
        let tool = SettingsSetTool(slot);
        let args = json!({"key": "privacy.microphone", "value": "on"});
        let a = tool.assess(&args).unwrap();
        let db = Arc::new(mika_core::storage::Db::open_in_memory().unwrap());
        let rules = RuleStore::new(db.clone());
        // A permanent allow cannot be saved for it.
        assert!(rules
            .add("settings_set", &a.scope, RuleDecision::Allow, a.level)
            .is_err());
        let bus = EventBus::new(64);
        let confirms = Confirmations::new(bus.clone(), StateHandle::new(bus.clone()));
        let gate = PolicyGate::new(rules, AuditLog::new(db), confirms.clone());
        let asking = confirms.clone();
        let answer = tokio::spawn(async move {
            for _ in 0..200 {
                if let Some(p) = asking.pending() {
                    assert_eq!(p.assessment.summary, "Включить микрофон");
                    assert!(!p.allow_always, "the card offers no «always»");
                    asking
                        .answer(p.id, Answer::Once, DecidedBy::Button)
                        .unwrap();
                    return true;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            false
        });
        let decision = gate.check("settings_set", &args, &a).await;
        assert!(answer.await.unwrap(), "the user was asked");
        assert!(matches!(decision, GateDecision::Approved(_)));
    }

    #[tokio::test]
    async fn in_telegram_eyes_and_ears_stay_off() {
        let (slot, port) = slot(FakePort {
            telegram: true,
            ..Default::default()
        });
        let tool = SettingsSetTool(slot);
        for key in [
            "privacy.microphone",
            "voice.wake",
            "dictation",
            "privacy.screen",
            "screen_ask",
            "log.content",
            "assistant.adult",
        ] {
            let err = tool
                .assess(&json!({"key": key, "value": "вкл"}))
                .unwrap_err();
            assert!(
                matches!(&err, ToolError::Failed(why) if why.contains("только за компьютером")),
                "{key}: {err:?}"
            );
        }
        // Other changes from Telegram follow the policy as anywhere.
        let web = tool
            .assess(&json!({"key": "tools.web", "value": "вкл"}))
            .unwrap();
        assert_eq!(web.level, RiskLevel::High);
        // Turning off from Telegram is allowed: it is the safe way.
        port.config.lock().privacy.microphone_enabled = true;
        run(&tool, json!({"key": "privacy.microphone", "value": "выкл"}))
            .await
            .unwrap();
        assert!(!port.config.lock().privacy.microphone_enabled);
    }

    #[tokio::test]
    async fn unknown_keys_and_bad_values_say_what_is_allowed() {
        let (slot, port) = slot(FakePort::default());
        let tool = SettingsSetTool(slot);
        for key in ["llm.api_key", "llm.base_url", "hotkeys.listen", "data_dir"] {
            let err = tool.assess(&json!({"key": key, "value": "x"})).unwrap_err();
            assert!(
                matches!(&err, ToolError::BadArguments(why) if why.contains("voice.volume")),
                "{key}: {err:?}"
            );
        }
        assert!(matches!(
            tool.assess(&json!({"key": "voice.rate", "value": 5})),
            Err(ToolError::BadArguments(why)) if why.contains("от 0.5 до 2")
        ));
        assert!(matches!(
            tool.assess(&json!({"key": "voice.rate"})),
            Err(ToolError::BadArguments(_))
        ));
        // The same value again changes nothing and asks nothing.
        assert!(matches!(
            tool.assess(&json!({"key": "voice.speak_replies", "value": "вкл"})),
            Err(ToolError::Failed(why)) if why.contains("менять нечего")
        ));
        assert!(port.applied.lock().is_empty());
    }

    #[tokio::test]
    async fn an_own_character_is_seen_and_put_on_by_name() {
        let mut config = Config::default();
        config
            .assistant
            .save_custom_persona(None, "Котёнок", "Мурлычешь.")
            .unwrap();
        let (slot, port) = slot(FakePort {
            config: Mutex::new(config),
            ..Default::default()
        });
        // settings_get names it, so the model knows it can be asked for.
        let out = run(&SettingsGetTool(slot.clone()), json!({}))
            .await
            .unwrap();
        assert!(out.text.contains("свои по имени: Котёнок"), "{}", out.text);
        assert!(!out.text.contains("Мурлычешь"), "the text stays out");
        let tool = SettingsSetTool(slot);
        let args = json!({"key": "assistant.persona", "value": "котёнок"});
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert_eq!(a.target, "было: заботливая → станет: Котёнок");
        run(&tool, args).await.unwrap();
        assert_eq!(port.config.lock().assistant.persona_choice(), "Котёнок");
        // A ready one by its Russian name, from Telegram too.
        run(
            &tool,
            json!({"key": "assistant.persona", "value": "деловая"}),
        )
        .await
        .unwrap();
        assert_eq!(port.config.lock().assistant.persona_choice(), "business");
        assert!(matches!(
            tool.assess(&json!({"key": "assistant.persona", "value": "Пират"})),
            Err(ToolError::BadArguments(why)) if why.contains("Котёнок")
        ));
    }

    #[tokio::test]
    async fn the_adult_mode_asks_every_time_and_turns_off_freely() {
        let (here, port) = slot(FakePort::default());
        let tool = SettingsSetTool(here);
        let on = tool
            .assess(&json!({"key": "assistant.adult", "value": "вкл"}))
            .unwrap();
        assert_eq!(on.level, RiskLevel::High);
        assert_eq!(on.summary, "Включить взрослый режим");
        assert!(mika_core::policy::forever_block_for(&on).is_some());
        run(&tool, json!({"key": "assistant.adult", "value": "вкл"}))
            .await
            .unwrap();
        assert!(port.config.lock().assistant.adult);

        // From Telegram it does not turn on, but it turns off.
        let (remote, port) = slot(FakePort {
            telegram: true,
            ..Default::default()
        });
        port.config.lock().assistant.adult = true;
        let tool = SettingsSetTool(remote);
        let off = tool
            .assess(&json!({"key": "assistant.adult", "value": "выкл"}))
            .unwrap();
        assert_eq!(off.level, RiskLevel::Low);
        run(&tool, json!({"key": "assistant.adult", "value": "выкл"}))
            .await
            .unwrap();
        assert!(!port.config.lock().assistant.adult);
    }

    #[tokio::test]
    async fn the_companion_asks_every_time_needs_the_screen_and_not_from_telegram() {
        let (here, port) = slot(FakePort::default());
        let tool = SettingsSetTool(here);
        let on = json!({"key": "companion.enabled", "value": "вкл"});
        // «Экран» is off: she says what is missing and turns nothing on.
        let err = tool.assess(&on).unwrap_err();
        assert!(
            matches!(&err, ToolError::Failed(why) if why.contains("privacy.screen")),
            "{err:?}"
        );
        assert!(port.applied.lock().is_empty());
        assert!(!port.config.lock().privacy.screen_enabled);

        port.config.lock().privacy.screen_enabled = true;
        let a = tool.assess(&on).unwrap();
        assert_eq!(a.level, RiskLevel::High);
        assert_eq!(a.summary, "Включить напарника в играх");
        assert!(mika_core::policy::forever_block_for(&a).is_some());
        run(&tool, on).await.unwrap();
        assert!(port.config.lock().companion.enabled);
        // The interval asks nothing.
        let every = tool
            .assess(&json!({"key": "companion.every_s", "value": 30}))
            .unwrap();
        assert_eq!(every.level, RiskLevel::Low);

        // From Telegram it does not turn on, but it turns off; off needs no screen.
        let (remote, port) = slot(FakePort {
            telegram: true,
            ..Default::default()
        });
        port.config.lock().privacy.screen_enabled = true;
        let tool = SettingsSetTool(remote);
        let err = tool
            .assess(&json!({"key": "companion.enabled", "value": "вкл"}))
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Failed(why) if why.contains("только за компьютером")),
            "{err:?}"
        );
        port.config.lock().companion.enabled = true;
        port.config.lock().privacy.screen_enabled = false;
        let off = tool
            .assess(&json!({"key": "companion.enabled", "value": "выкл"}))
            .unwrap();
        assert_eq!(off.level, RiskLevel::Low);
        run(&tool, json!({"key": "companion.enabled", "value": "выкл"}))
            .await
            .unwrap();
        assert!(!port.config.lock().companion.enabled);
    }

    #[tokio::test]
    async fn a_voice_is_picked_from_the_list() {
        let (slot, port) = slot(FakePort::default());
        let tool = SettingsSetTool(slot);
        run(&tool, json!({"key": "voice.voice", "value": "pavel"}))
            .await
            .unwrap();
        assert_eq!(
            port.applied.lock()[0],
            ("voice.voice".into(), Value::Text("Microsoft Pavel".into()))
        );
        assert!(tool
            .assess(&json!({"key": "voice.voice", "value": "Kore"}))
            .is_err());
    }

    #[tokio::test]
    async fn a_failed_apply_is_an_error_and_no_port_says_where_it_works() {
        let (slot, _) = slot(FakePort {
            fails: true,
            ..Default::default()
        });
        let err = run(
            &SettingsSetTool(slot),
            json!({"key": "rhythm.breaks", "value": "вкл"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err, ToolError::Failed("не сохранить настройки".into()));

        let empty = SettingsSlot::default();
        let err = SettingsSetTool(empty.clone())
            .assess(&json!({"key": "voice.rate", "value": 1.2}))
            .unwrap_err();
        assert_eq!(err, ToolError::Failed(NO_PORT.into()));
        assert!(run(&SettingsGetTool(empty), json!({})).await.is_err());
    }
}
