//! Terminal client for debugging the core without the GUI.
//! Reads lines from stdin, answers through the configured LLM provider.
//! Tool calls ask for confirmation in the same terminal: «да», «разреши всегда», «нет».

mod repl;
mod setup;

use std::io::{BufRead, Write};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use mika_core::commands::CommandStore;
use mika_core::memory::Memory;
use mika_core::schedule::{run_scheduler, ReminderStore};
use mika_core::secrets::SecretStore;
use mika_core::tools::format_time;
use mika_providers::live::Live;
use parking_lot::{Mutex, RwLock};
use std::time::Duration;

use mika_core::agent::{system_prompt, Agent};
use mika_core::config::Config;
use mika_core::confirm::Confirmations;
use mika_core::paths::{config_path, database_path};
use mika_core::policy::{AuditLog, PolicyGate, RuleStore};
use mika_core::secrets::KeyringSecretStore;
use mika_core::session::{Session, Submitted};
use mika_core::storage::Db;
use mika_core::tools::ToolRegistry;
use mika_core::{AssistantState, Event, EventBus, StateHandle};
use repl::{decode_line, Renderer};
use tokio::sync::mpsc;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(result) = setup::run(&args) {
        match result {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("[error] {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run());
}

async fn run() {
    let (config, err) = Config::load_or_default(&config_path());
    if let Some(e) = err {
        eprintln!("[error] {e}");
    }
    let db: Arc<Db> = match Db::open(&database_path()) {
        Ok(db) => Arc::new(db),
        Err(e) => {
            eprintln!("[error] {e}");
            return;
        }
    };
    let bus = EventBus::new(256);
    let state = StateHandle::new(bus.clone());
    let secrets: Arc<dyn SecretStore> = Arc::new(KeyringSecretStore::new());
    let provider = mika_providers::build_or_placeholder(&config.llm, secrets.as_ref());
    let confirms = Confirmations::new(bus.clone(), state.clone());
    let gate = PolicyGate::new(
        RuleStore::new(db.clone()),
        AuditLog::new(db.clone()),
        confirms.clone(),
    );

    let shared_config = Arc::new(Mutex::new(config.clone()));
    let live = Live::new(shared_config, secrets);
    let memory = Memory::new(db.clone(), Some(Arc::new(live.clone())));
    let docs = mika_tools::docs::DocIndex::new(db.clone(), Some(Arc::new(live.clone())));
    let task_board = mika_core::task::TaskBoard::new(bus.clone());
    let env = mika_tools::ToolEnv::windows(
        db.clone(),
        mika_tools::Shared {
            web_enabled: Arc::new(AtomicBool::new(config.tools.web_enabled)),
            screen_enabled: Arc::new(AtomicBool::new(config.privacy.screen_enabled)),
            news_feed: Arc::new(RwLock::new(config.tools.news_feed.clone())),
            memory: memory.clone(),
            vision: Arc::new(live.clone()),
            images: Arc::new(live),
            bus: bus.clone(),
            tasks: task_board.clone(),
            watches: Default::default(),
            focus: Default::default(),
            clips: Default::default(),
            clipboard_on: Default::default(),
            app_time_on: Arc::new(AtomicBool::new(config.tracking.app_time)),
            calendar: Default::default(),
            docs,
        },
    );
    let mut registry = ToolRegistry::new();
    mika_tools::register_all(&mut registry, &env, &config.tools.disabled_groups);
    let (mcp_tools, mcp_report) = mika_providers::mcp::connect_all(&config.mcp.servers).await;
    for (name, result) in mcp_report {
        match result {
            Ok(n) => eprintln!("[mcp] {name}: {n} tools"),
            Err(e) => eprintln!("[mcp] {name}: {e}"),
        }
    }
    for tool in mcp_tools {
        registry.register(tool);
    }
    let agent = Agent::new(
        provider,
        Arc::new(registry),
        Arc::new(gate),
        bus.clone(),
        state,
        system_prompt(&config.assistant),
    );
    agent.add_prompt_extra(Arc::new(|| {
        Some(format!("Сейчас {}.", format_time(chrono::Local::now())))
    }));
    agent.set_recall(Arc::new(memory));
    agent.set_task_board(task_board);
    let agent = Arc::new(agent);
    let hinted = Arc::downgrade(&agent);
    agent.add_prompt_extra(Arc::new(move || {
        hinted
            .upgrade()
            .and_then(|a| mika_tools::tool_hints(&a.registry()))
    }));
    let session = Session::new(agent, confirms, bus.clone());
    session.set_commands(CommandStore::new(db.clone()));
    let tasks = session.clone();
    tokio::spawn(run_scheduler(
        ReminderStore::new(db),
        bus.clone(),
        move |text| tasks.submit_task(&text),
    ));

    let mut events = bus.subscribe();
    let printer = tokio::spawn(async move {
        let mut r = Renderer::default();
        let mut out = std::io::stdout();
        while let Ok(e) = events.recv().await {
            let _ = write!(out, "{}", r.render(&e));
            let _ = out.flush();
        }
    });

    let (tx, mut lines) = mpsc::channel::<String>(16);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match input.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.blocking_send(decode_line(&buf)).is_err() {
                        break;
                    }
                }
            }
        }
    });

    while let Some(line) = lines.recv().await {
        if line.trim() == "/quit" {
            break;
        }
        let mut changes = bus.subscribe();
        if matches!(
            session.submit(&line),
            Submitted::Started | Submitted::Answered
        ) {
            // Read the next line only when she waits for the user again.
            loop {
                match changes.recv().await {
                    Ok(Event::StateChanged {
                        to: AssistantState::Idle | AssistantState::AwaitingConfirm,
                        ..
                    }) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }
    }
    session.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    printer.abort();
}
