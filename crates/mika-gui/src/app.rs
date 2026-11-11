//! Application context: config, secrets, database, session.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mika_core::agent::{system_prompt, Agent};
use mika_core::config::Config;
use mika_core::confirm::Confirmations;
use mika_core::gate::Gate;
use mika_core::paths::{config_path, database_path};
use mika_core::policy::{AuditLog, PolicyGate, RuleStore};
use mika_core::secrets::{KeyringSecretStore, SecretStore};
use mika_core::session::Session;
use mika_core::storage::Db;
use mika_core::tools::ToolRegistry;
use mika_core::{EventBus, StateHandle};
use mika_tools::ToolEnv;
use parking_lot::Mutex;

pub const CONVERSATION: &str = "main";

pub struct Ctx {
    pub config: Mutex<Config>,
    pub secrets: Arc<dyn SecretStore>,
    pub db: Arc<Db>,
    pub rules: RuleStore,
    pub audit: AuditLog,
    pub session: Arc<Session>,
    /// Mirrors `tools.web_enabled`; tools read it on every call.
    pub web_enabled: Arc<AtomicBool>,
}

impl Ctx {
    pub fn build(bus: EventBus, state: StateHandle) -> Result<Ctx, String> {
        let (config, err) = Config::load_or_default(&config_path());
        if let Some(e) = err {
            log::error!("{e}");
        }
        let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore::new());
        let db = Arc::new(Db::open(&database_path()).map_err(|e| e.to_string())?);
        let history = db.load_messages(CONVERSATION).unwrap_or_else(|e| {
            log::error!("cannot load history: {e}");
            Vec::new()
        });

        let rules = RuleStore::new(db.clone());
        let audit = AuditLog::new(db.clone());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let gate: Arc<dyn Gate> = Arc::new(PolicyGate::new(
            rules.clone(),
            audit.clone(),
            confirms.clone(),
        ));
        let web_enabled = Arc::new(AtomicBool::new(config.tools.web_enabled));
        let mut registry = ToolRegistry::new();
        mika_tools::register_all(
            &mut registry,
            &ToolEnv::windows(db.clone(), web_enabled.clone()),
        );
        let provider = mika_providers::build_or_placeholder(&config.llm, secrets.as_ref());
        let agent = Agent::new(
            provider,
            Arc::new(registry),
            gate,
            bus.clone(),
            state,
            system_prompt(&config.assistant),
        )
        .with_history(history)
        .with_store(db.clone(), CONVERSATION);

        Ok(Ctx {
            config: Mutex::new(config),
            secrets,
            db,
            rules,
            audit,
            session: Session::new(Arc::new(agent), confirms, bus),
            web_enabled,
        })
    }

    /// Rebuilds the LLM provider from the current config and secrets.
    pub fn reload_provider(&self) {
        let cfg = self.config.lock().llm.clone();
        self.session
            .agent
            .set_provider(mika_providers::build_or_placeholder(
                &cfg,
                self.secrets.as_ref(),
            ));
    }

    /// Puts the current name and character into the system prompt.
    pub fn refresh_prompt(&self) {
        let prompt = system_prompt(&self.config.lock().assistant);
        self.session.agent.set_system_prompt(prompt);
    }

    pub fn save_config(&self) -> Result<(), String> {
        self.config
            .lock()
            .save(&config_path())
            .map_err(|e| e.to_string())
    }

    pub fn set_web_enabled(&self, value: bool) -> Result<(), String> {
        self.config.lock().tools.web_enabled = value;
        self.web_enabled.store(value, Ordering::Relaxed);
        self.save_config()
    }
}
