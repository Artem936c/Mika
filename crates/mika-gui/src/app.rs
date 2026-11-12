//! Application context: config, secrets, database, session and background jobs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mika_audio::{MicControl, MicError, Microphone, SpeakerSwitch, Voice};
use mika_core::agent::{system_prompt, Agent};
use mika_core::commands::CommandStore;
use mika_core::config::{mcp_token_key, Config, TtsEngine, TELEGRAM_SECRET_KEY};
use mika_core::confirm::Confirmations;
use mika_core::gate::Gate;
use mika_core::llm::{SpeechToText, TextToSpeech};
use mika_core::memory::Memory;
use mika_core::paths::{config_path, database_path};
use mika_core::policy::{AuditLog, PolicyGate, RuleStore};
use mika_core::schedule::{run_scheduler, ReminderStore};
use mika_core::secrets::{KeyringSecretStore, SecretStore};
use mika_core::session::{Origin, Session};
use mika_core::storage::Db;
use mika_core::tools::{format_time, Tool, ToolRegistry};
use mika_core::AssistantState;
use mika_core::{EventBus, StateHandle};
use mika_providers::live::Live;
use mika_tools::{Shared, ToolEnv};
use parking_lot::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

pub const CONVERSATION: &str = "main";

/// Locale for the Windows recognizer from the recognition language: `ru` → `ru-RU`.
pub fn speech_locale(language: &str) -> String {
    let l = language.trim();
    if l.contains('-') {
        return l.to_string();
    }
    match l.to_lowercase().as_str() {
        "" | "ru" => "ru-RU".into(),
        "en" => "en-US".into(),
        "uk" => "uk-UA".into(),
        other => format!("{other}-{}", other.to_uppercase()),
    }
}

/// How one MCP server did at the last start: its tool count or the error.
pub type McpReport = Vec<(String, Result<usize, String>)>;

pub struct Ctx {
    pub config: Arc<Mutex<Config>>,
    pub secrets: Arc<dyn SecretStore>,
    pub db: Arc<Db>,
    pub rules: RuleStore,
    pub audit: AuditLog,
    pub session: Arc<Session>,
    pub memory: Memory,
    pub reminders: ReminderStore,
    pub commands: CommandStore,
    pub bus: EventBus,
    /// Mirrors `tools.web_enabled`; tools read it on every call.
    pub web_enabled: Arc<AtomicBool>,
    /// Mirrors `privacy.screen_enabled`.
    pub screen_enabled: Arc<AtomicBool>,
    /// Mirrors `tools.news_feed`.
    pub news_feed: Arc<RwLock<String>>,
    env: ToolEnv,
    mcp_tools: Mutex<Vec<Arc<dyn Tool>>>,
    pub mcp_report: Mutex<McpReport>,
    /// Stops the running Telegram bridge.
    telegram: Mutex<Option<CancellationToken>>,
    pub telegram_status: Mutex<String>,
    pub voice: Arc<dyn Voice>,
    /// Mirrors `voice.speak_replies`.
    pub speaker: SpeakerSwitch,
    pub mic: Arc<dyn Microphone>,
    /// The same microphone, for changing its device from the settings.
    system_mic: Arc<mika_audio::SystemMicrophone>,
    /// Set while a phrase is being recorded.
    recording: Mutex<Option<Arc<MicControl>>>,
    live: Live,
    /// Sound of phrases the provider already made.
    pub speech_cache: Arc<mika_core::speech_cache::SpeechCache>,
    state: StateHandle,
    /// Set in `setup`: wake phrase and shortcuts bring the windows to the front.
    pub app: std::sync::OnceLock<tauri::AppHandle>,
    wake: Mutex<Option<mika_audio::wake::WakeHandle>>,
    /// `off`, `on`, or why the wake phrase cannot listen.
    pub wake_status: Mutex<String>,
    /// Set while the user talks to her by voice: she listens again after answering.
    follow_up: AtomicBool,
    /// What she has said in the current reply, to tell her own voice from the user.
    speaking: Mutex<String>,
    /// A newer version found by the last check.
    pub update: Mutex<Option<crate::update::Manifest>>,
}

/// How an attempt to hear a phrase ended.
pub enum Heard {
    /// A recording was already running; this call told it to finish.
    Finished,
    Text(String),
}

/// Whether speech would be heard: system sound on and above zero. Read at most
/// once a second; an unreadable mixer counts as audible.
fn audible(volume: Arc<dyn mika_tools::sys::Volume>) -> mika_audio::speech::Audible {
    let last: Arc<Mutex<Option<(std::time::Instant, bool)>>> = Arc::new(Mutex::new(None));
    Arc::new(move || {
        if let Some((at, heard)) = *last.lock() {
            if at.elapsed() < std::time::Duration::from_secs(1) {
                return heard;
            }
        }
        let heard = match volume.get() {
            Ok((level, muted)) => !muted && level > 0,
            Err(_) => true,
        };
        *last.lock() = Some((std::time::Instant::now(), heard));
        heard
    })
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
        let screen_enabled = Arc::new(AtomicBool::new(config.privacy.screen_enabled));
        let news_feed = Arc::new(RwLock::new(config.tools.news_feed.clone()));
        let disabled = config.tools.disabled_groups.clone();
        let voice: Arc<dyn Voice> = Arc::new(mika_audio::win::WindowsVoice::new());
        voice.configure(&config.voice.voice, config.voice.rate);
        let speaker = SpeakerSwitch::new(config.voice.speak_replies);
        let mut config = config;
        // A profile from an earlier version that already has a key needs no first-run screen.
        if !config.assistant.onboarded
            && matches!(secrets.get(&config.llm.kind.secret_key()), Ok(Some(k)) if !k.is_empty())
        {
            config.assistant.onboarded = true;
            if let Err(e) = config.save(&config_path()) {
                log::warn!("cannot save config: {e}");
            }
        }
        let system_mic = Arc::new(mika_audio::SystemMicrophone::new(
            &config.voice.input_device,
        ));
        let assistant = config.assistant.clone();
        let llm = config.llm.clone();
        let config = Arc::new(Mutex::new(config));

        let speech_cache = Arc::new(mika_core::speech_cache::SpeechCache::new(
            mika_core::paths::speech_cache_dir(),
        ));
        let live =
            Live::new(config.clone(), secrets.clone()).with_speech_cache(speech_cache.clone());
        let speech = live.clone();
        let voice_state = state.clone();
        let memory = Memory::new(db.clone(), Some(Arc::new(live.clone())));
        let task_board = mika_core::task::TaskBoard::new(bus.clone());
        let env = ToolEnv::windows(
            db.clone(),
            Shared {
                web_enabled: web_enabled.clone(),
                screen_enabled: screen_enabled.clone(),
                news_feed: news_feed.clone(),
                memory: memory.clone(),
                vision: Arc::new(live.clone()),
                images: Arc::new(live),
                bus: bus.clone(),
                tasks: task_board.clone(),
            },
        );
        let speaker = speaker.with_audible(audible(env.volume.clone()));
        let mut registry = ToolRegistry::new();
        mika_tools::register_all(&mut registry, &env, &disabled);

        let provider = mika_providers::build_or_placeholder(&llm, secrets.as_ref());
        let agent = Agent::new(
            provider,
            Arc::new(registry),
            gate,
            bus.clone(),
            state,
            system_prompt(&assistant),
        )
        .with_history(history)
        .with_store(db.clone(), CONVERSATION);
        agent.add_prompt_extra(Arc::new(|| {
            Some(format!("Сейчас {}.", format_time(chrono::Local::now())))
        }));
        agent.set_recall(Arc::new(memory.clone()));
        agent.set_task_board(task_board);

        let agent = Arc::new(agent);
        let hinted = Arc::downgrade(&agent);
        agent.add_prompt_extra(Arc::new(move || {
            hinted
                .upgrade()
                .and_then(|a| mika_tools::tool_hints(&a.registry()))
        }));

        let commands = CommandStore::new(db.clone());
        let session = Session::new(agent, confirms, bus.clone());
        session.set_commands(commands.clone());

        Ok(Ctx {
            config,
            secrets,
            reminders: ReminderStore::new(db.clone()),
            db,
            rules,
            audit,
            session,
            memory,
            commands,
            bus,
            web_enabled,
            screen_enabled,
            news_feed,
            env,
            mcp_tools: Mutex::new(Vec::new()),
            mcp_report: Mutex::new(Vec::new()),
            telegram: Mutex::new(None),
            telegram_status: Mutex::new(String::new()),
            voice,
            speaker,
            mic: system_mic.clone(),
            system_mic,
            recording: Mutex::new(None),
            live: speech,
            speech_cache,
            state: voice_state,
            app: std::sync::OnceLock::new(),
            wake: Mutex::new(None),
            wake_status: Mutex::new("off".into()),
            follow_up: AtomicBool::new(false),
            speaking: Mutex::new(String::new()),
            update: Mutex::new(None),
        })
    }

    /// Starts the scheduler, the MCP servers and the Telegram bridge.
    /// Must run inside the async runtime.
    pub fn start_background(self: &Arc<Self>) {
        self.watch_voice_dialog();
        let session = self.session.clone();
        let notes = self.session.clone();
        tauri::async_runtime::spawn(run_scheduler(
            self.reminders.clone(),
            self.bus.clone(),
            move |text| session.submit_task(&text),
        ));
        // A fired reminder joins the conversation, so "snooze it" has something to refer to.
        let mut events = self.bus.subscribe();
        tauri::async_runtime::spawn(async move {
            use mika_core::schedule::ReminderKind;
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match events.recv().await {
                    Ok(mika_core::Event::ReminderFired {
                        text,
                        kind: ReminderKind::Remind,
                        ..
                    }) => notes.agent.append_note(&format!("⏰ Напоминание: {text}")),
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => break,
                }
            }
        });
        tauri::async_runtime::spawn(mika_audio::run_speaker(
            self.bus.clone(),
            self.voice.clone(),
            self.speaker.clone(),
        ));
        self.apply_voice_engine();
        self.restart_wake();
        // Her current reply, to recognise her own name in the speakers.
        let mut said = self.bus.subscribe();
        let echo = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                let event = match said.recv().await {
                    Ok(e) => e,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                let Some(ctx) = echo.upgrade() else { break };
                match event {
                    mika_core::Event::UserText { .. } => ctx.speaking.lock().clear(),
                    mika_core::Event::AssistantDelta { text } => {
                        let mut s = ctx.speaking.lock();
                        s.push_str(&text);
                        if s.chars().count() > 600 {
                            *s = s
                                .chars()
                                .rev()
                                .take(400)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect();
                        }
                    }
                    _ => {}
                }
            }
        });
        if self.config.lock().updates.check_at_start {
            let this = self.clone();
            tauri::async_runtime::spawn(async move {
                match this.check_update().await {
                    Ok(Some(m)) => log::info!("version {} is available", m.version),
                    Ok(None) => {}
                    Err(e) => log::warn!("update check failed: {e}"),
                }
            });
        }
        let timings = self.bus.clone();
        self.voice.set_observer(Some(Arc::new(move |ms| {
            timings.publish(mika_core::Event::Timing {
                stage: "tts".into(),
                ms,
            });
        })));
        let this = self.clone();
        tauri::async_runtime::spawn(async move { this.reconnect_mcp().await });
        self.restart_telegram();
    }

    /// Rebuilds the tool set from the current settings and connected MCP servers.
    pub fn rebuild_registry(&self) {
        let disabled = self.config.lock().tools.disabled_groups.clone();
        let mut registry = ToolRegistry::new();
        mika_tools::register_all(&mut registry, &self.env, &disabled);
        for tool in self.mcp_tools.lock().iter() {
            registry.register(tool.clone());
        }
        self.session.agent.set_registry(Arc::new(registry));
    }

    /// Stops the old MCP servers and starts the enabled ones.
    pub async fn reconnect_mcp(&self) {
        let servers = self.config.lock().mcp.servers.clone();
        // Dropping the old tools drops their clients, which kills the old processes.
        self.mcp_tools.lock().clear();
        self.rebuild_registry();
        let secrets = self.secrets.clone();
        let token_of = move |name: &str| {
            secrets
                .get(&mcp_token_key(name))
                .ok()
                .flatten()
                .filter(|t| !t.is_empty())
        };
        let (tools, report) = mika_providers::mcp::connect_all_with(&servers, &token_of).await;
        *self.mcp_tools.lock() = tools;
        *self.mcp_report.lock() = report;
        self.rebuild_registry();
    }

    /// Stops the Telegram bridge and starts it again if it is on and has a token and an owner.
    pub fn restart_telegram(self: &Arc<Self>) {
        if let Some(old) = self.telegram.lock().take() {
            old.cancel();
        }
        let cfg = self.config.lock().telegram.clone();
        let token = self
            .secrets
            .get(TELEGRAM_SECRET_KEY)
            .ok()
            .flatten()
            .filter(|t| !t.is_empty());
        let status = match (cfg.enabled, token, cfg.user_id) {
            (false, _, _) => "off",
            (true, None, _) => "no_token",
            (true, Some(_), 0) => "no_owner",
            (true, Some(token), owner) => {
                let cancel = CancellationToken::new();
                *self.telegram.lock() = Some(cancel.clone());
                let api = Arc::new(mika_providers::telegram::HttpTelegram::new(token));
                tauri::async_runtime::spawn(mika_providers::telegram::run_bridge(
                    api,
                    self.session.clone(),
                    self.bus.clone(),
                    owner,
                    cancel,
                ));
                "on"
            }
        };
        *self.telegram_status.lock() = status.to_string();
    }

    /// Points the voice at the provider or back at Windows, as the settings say.
    pub fn apply_voice_engine(&self) {
        let v = self.config.lock().voice.clone();
        if v.tts == TtsEngine::Provider && !v.tts_model.trim().is_empty() {
            let live = self.live.clone();
            // Runs on the voice's own thread, so blocking on the request is fine.
            self.voice.set_remote(Some(Arc::new(move |text: &str| {
                tauri::async_runtime::block_on(live.synthesize(text))
            })));
        } else {
            self.voice.set_remote(None);
        }
    }

    pub fn set_input_device(&self, name: &str) {
        self.system_mic.set_device(name);
    }

    pub fn media(&self) -> Arc<dyn mika_tools::sys::Media> {
        self.env.media.clone()
    }

    pub fn is_recording(&self) -> bool {
        self.recording.lock().is_some()
    }

    /// Starts or stops the wake phrase as the settings say: it listens only while
    /// the microphone is allowed and the wake phrase is on.
    /// Stops the wake phrase and waits until its thread let go of Vosk.
    pub fn stop_wake(&self) {
        if let Some(handle) = self.wake.lock().take() {
            handle.stop_and_wait();
        }
    }

    pub fn restart_wake(self: &Arc<Self>) {
        // Dropping the old handle stops its listener.
        self.wake.lock().take();
        let (on, name, language) = {
            let c = self.config.lock();
            (
                c.privacy.microphone_enabled && c.voice.wake_enabled,
                c.assistant.wake_phrase.clone(),
                c.voice.stt_language.clone(),
            )
        };
        let phrases = mika_audio::wake::wake_phrases(&name);
        if !on || phrases.is_empty() {
            *self.wake_status.lock() = "off".into();
            return;
        }
        *self.wake_status.lock() = "starting".into();
        let me = Arc::downgrade(self);
        let status = Arc::downgrade(self);
        let engine = crate::vosk::engine().unwrap_or(mika_audio::wake::WakeEngine::Windows {
            language: speech_locale(&language),
        });
        let local = matches!(engine, mika_audio::wake::WakeEngine::Vosk { .. });
        let handle = mika_audio::wake::start_wake(mika_audio::wake::WakeSetup {
            engine,
            name,
            phrases,
            mic: self.system_mic.clone(),
            on_wake: Arc::new(move |heard| {
                if let Some(ctx) = me.upgrade() {
                    ctx.on_wake(&heard);
                }
            }),
            on_status: Arc::new(move |result| {
                if let Some(ctx) = status.upgrade() {
                    *ctx.wake_status.lock() = match result {
                        Ok(()) => "on".into(),
                        Err(why) => why,
                    };
                }
            }),
        });
        *self.wake.lock() = Some(handle);
        // Vosk reports `on` itself once the model is loaded, which takes seconds.
        if !local {
            *self.wake_status.lock() = "on".into();
        }
    }

    /// The wake phrase was heard: stop talking, come forward, listen.
    fn on_wake(self: &Arc<Self>, heard: &str) {
        if self.is_recording() {
            return;
        }
        let name = self.config.lock().assistant.wake_phrase.clone();
        if self.state.get() == AssistantState::Speaking
            && mika_audio::wake::is_echo(&name, &self.speaking.lock())
        {
            log::info!("wake phrase ignored: she said her own name");
            return;
        }
        log::info!("wake phrase heard: {heard}");
        if let Some(app) = self.app.get() {
            crate::window::summon(app);
        }
        let ctx = self.clone();
        tauri::async_runtime::spawn(async move { ctx.listen_and_submit().await });
    }

    /// Looks for a newer version if an update address is set. Never downloads.
    pub async fn check_update(&self) -> Result<Option<crate::update::Manifest>, String> {
        let url = self.config.lock().updates.manifest_url.clone();
        if url.trim().is_empty() {
            *self.update.lock() = None;
            return Ok(None);
        }
        let found = crate::update::check(&url).await?;
        *self.update.lock() = found.clone();
        Ok(found)
    }

    /// Stops a recording and throws it away.
    pub fn cancel_listening(&self) {
        if let Some(c) = self.recording.lock().as_ref() {
            c.cancel();
        }
    }

    /// Records one phrase and recognises it. A second call while recording
    /// finishes the phrase early. The microphone opens only when the user allowed it.
    pub async fn hear(&self) -> Result<Heard, String> {
        let (allowed, has_model) = {
            let c = self.config.lock();
            (
                c.privacy.microphone_enabled,
                !c.voice.stt_model.trim().is_empty(),
            )
        };
        if !allowed {
            return Err(crate::strings::tr("error.mic_off"));
        }
        if !has_model {
            return Err(crate::strings::tr("error.stt_off"));
        }
        let control = {
            let mut slot = self.recording.lock();
            if let Some(running) = slot.as_ref() {
                running.finish();
                return Ok(Heard::Finished);
            }
            let control = Arc::new(MicControl::default());
            *slot = Some(control.clone());
            control
        };
        // Her own speech must not reach the microphone.
        self.voice.stop();
        let before = self.state.get();
        let rest = if before == AssistantState::AwaitingConfirm {
            AssistantState::AwaitingConfirm
        } else {
            AssistantState::Idle
        };
        self.state.set(AssistantState::Listening);
        let mic = self.mic.clone();
        let recorded = tauri::async_runtime::spawn_blocking(move || mic.record(&control)).await;
        *self.recording.lock() = None;
        let wav = match recorded {
            Ok(Ok(wav)) => wav,
            Ok(Err(e)) => {
                self.state.set(rest);
                return Err(match e {
                    // The user cancelled: nothing to report.
                    MicError::Cancelled => String::new(),
                    other => other.to_string(),
                });
            }
            Err(e) => {
                self.state.set(rest);
                return Err(e.to_string());
            }
        };
        if rest == AssistantState::Idle {
            self.state.set(AssistantState::Thinking);
        } else {
            self.state.set(rest);
        }
        let asked = std::time::Instant::now();
        let text = self.live.transcribe(&wav).await;
        if text.is_ok() {
            let ms = asked.elapsed().as_millis() as u64;
            log::info!("speech recognised in {ms} ms");
            self.bus.publish(mika_core::Event::Timing {
                stage: "stt".into(),
                ms,
            });
        }
        if self.state.get() == AssistantState::Thinking {
            self.state.set(AssistantState::Idle);
        }
        let text = text?;
        if text.trim().is_empty() {
            return Err(MicError::NoSpeech.to_string());
        }
        Ok(Heard::Text(text))
    }

    /// Records a phrase and hands it to the assistant as a voice message.
    /// Problems go to the chat as an error line.
    pub async fn listen_and_submit(&self) {
        self.listen_once(false).await;
    }

    /// Records one phrase and sends it. After a spoken question she listens
    /// again once her answer is said; silence then ends the conversation quietly.
    async fn listen_once(&self, follow_up: bool) {
        match self.hear().await {
            Ok(Heard::Text(text)) => {
                self.follow_up.store(true, Ordering::Relaxed);
                self.session.submit_from(&text, Origin::Voice);
            }
            Ok(Heard::Finished) => {}
            Err(message) if message.is_empty() => {}
            Err(_) if follow_up => {
                self.follow_up.store(false, Ordering::Relaxed);
            }
            Err(message) => {
                self.bus.publish(mika_core::Event::Error { message });
            }
        }
    }

    /// The conversation by voice is over: typed text, «Стоп», an error.
    pub fn end_voice_dialog(&self) {
        self.follow_up.store(false, Ordering::Relaxed);
    }

    /// After an answer to a spoken question: waits until she has said it all,
    /// then listens for the next question, as smart speakers do.
    async fn listen_after_answer(self: Arc<Self>, at: AssistantState) {
        // The last sentences reach the voice a moment after the turn ends.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        while self.voice.busy() && std::time::Instant::now() < deadline {
            if self.state.get() != at || !self.follow_up.load(Ordering::Relaxed) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // The tail of her voice must not reach the microphone.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let wanted = {
            let c = self.config.lock();
            c.voice.follow_up
                && c.privacy.microphone_enabled
                && !c.voice.stt_model.trim().is_empty()
        };
        if !wanted {
            self.end_voice_dialog();
            return;
        }
        if !self.follow_up.load(Ordering::Relaxed) || self.state.get() != at || self.is_recording()
        {
            return;
        }
        self.listen_once(true).await;
    }

    /// Starts listening after each answer to a spoken question.
    fn watch_voice_dialog(self: &Arc<Self>) {
        use mika_core::Event;
        use tokio::sync::broadcast::error::RecvError;
        let me = Arc::downgrade(self);
        let mut events = self.bus.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(e) => e,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                };
                let Some(ctx) = me.upgrade() else { break };
                match event {
                    Event::Error { .. } => ctx.end_voice_dialog(),
                    Event::StateChanged { to, .. }
                        if matches!(to, AssistantState::Idle | AssistantState::AwaitingConfirm)
                            && ctx.follow_up.load(Ordering::Relaxed) =>
                    {
                        tauri::async_runtime::spawn(ctx.listen_after_answer(to));
                    }
                    _ => {}
                }
            }
        });
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

    pub fn set_screen_enabled(&self, value: bool) -> Result<(), String> {
        self.config.lock().privacy.screen_enabled = value;
        self.screen_enabled.store(value, Ordering::Relaxed);
        self.save_config()
    }

    pub fn http(&self) -> Arc<dyn mika_tools::Http> {
        self.env.http.clone()
    }

    pub fn user_profile(&self) -> &str {
        &self.env.user_profile
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn recognizer_locale_from_language() {
        assert_eq!(super::speech_locale("ru"), "ru-RU");
        assert_eq!(super::speech_locale(""), "ru-RU");
        assert_eq!(super::speech_locale("en"), "en-US");
        assert_eq!(super::speech_locale("de"), "de-DE");
        assert_eq!(super::speech_locale("en-GB"), "en-GB");
    }
}
