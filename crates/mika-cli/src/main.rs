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

/// The line the terminal prints when a reached limit first holds back paid
/// work she starts by herself.
fn pause_line(pause: &mika_core::costs::BudgetPause) -> String {
    use mika_core::costs::{money_text, BudgetPeriod};
    let period = match pause.period {
        BudgetPeriod::Day => "day",
        BudgetPeriod::Month => "month",
    };
    format!(
        "[costs] {period} budget reached ({} of {}): scenarios and scheduled tasks are paused",
        money_text(pause.spent, &pause.currency),
        money_text(pause.limit, &pause.currency)
    )
}

/// A budget warning as the terminal prints it.
fn budget_line(alert: &mika_core::costs::BudgetAlert) -> String {
    use mika_core::costs::{money_text, BudgetPeriod};
    let period = match alert.period {
        BudgetPeriod::Day => "day",
        BudgetPeriod::Month => "month",
    };
    format!(
        "[costs] {period} budget at {}%: {} of {}",
        alert.level,
        money_text(alert.spent, &alert.currency),
        money_text(alert.limit, &alert.currency)
    )
}

async fn run() {
    let (config, err) = Config::load_or_default(&config_path());
    mika_core::i18n::set_lang(config.i18n.lang());
    mika_core::i18n::set_speech_language(&config.voice.stt_language);
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
    // Paid calls are counted here too (decision 0028). No price lists are
    // read. A budget warning is printed: the base is shared with the app, and
    // a threshold this program crosses is stored as told, so it must be told
    // here, or the app would never show it.
    let costs = mika_core::costs::Costs::new(db.clone());
    costs.set_settings(
        &config.costs,
        mika_core::costs::currency_code(&config.llm.effective_base_url()),
    );
    costs.on_alert(Arc::new(|alert| eprintln!("{}", budget_line(&alert))));
    // The same for the pause of her own paid work: told here, it is stored
    // as told, and the app would not say it again.
    costs.on_pause(Arc::new(|pause| eprintln!("{}", pause_line(&pause))));
    mika_core::costs::install(costs);
    let provider = mika_providers::build_or_placeholder(&config.llm, secrets.as_ref());
    let confirms = Confirmations::new(bus.clone(), state.clone());
    let gate = PolicyGate::new(
        RuleStore::new(db.clone()),
        AuditLog::new(db.clone()),
        confirms.clone(),
    );

    let shared_config = Arc::new(Mutex::new(config.clone()));
    // A recording takes the microphone of the voice input, and only while
    // the settings allow the microphone.
    let mic_switch = shared_config.clone();
    let rec = mika_tools::ffmpeg::RecordDeps {
        sound: Arc::new(mika_audio::sound_capture::SystemSound::new(Arc::new(
            mika_audio::SystemMicrophone::new(&config.voice.input_device),
        ))),
        mic_allowed: Arc::new(move || mic_switch.lock().privacy.microphone_enabled),
    };
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
            home_city: Arc::new(RwLock::new(config.island.city.clone())),
            memory: memory.clone(),
            vision: Arc::new(live.clone()),
            stt: Arc::new(live.clone()),
            text_model: Arc::new(live.clone()),
            images: Arc::new(live),
            bus: bus.clone(),
            tasks: task_board.clone(),
            watches: Default::default(),
            focus: mika_tools::focus::Focus::default()
                .with_log(mika_core::daylog::DayLog(db.clone())),
            clips: Default::default(),
            clipboard_on: Default::default(),
            app_time_on: Arc::new(AtomicBool::new(config.tracking.app_time)),
            calendar: Default::default(),
            docs,
            ffmpeg_path: Arc::new(RwLock::new(config.tools.ffmpeg_path.clone())),
            av_outputs: Default::default(),
            triggers: mika_core::triggers::TriggerStore::new(db.clone()),
            waits: Default::default(),
            jobs: mika_core::bgjobs::JobBoard::new(bus.clone()),
            rec,
            research: Arc::new(RwLock::new(config.research.clone())),
            research_runs: Default::default(),
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
    let commands = CommandStore::new(db.clone());
    session.set_commands(commands.clone());
    let tasks = session.clone();
    // Fired reminders and finished tasks go into the day log.
    tokio::spawn(mika_core::daylog::run_recorder(
        mika_core::daylog::DayLog(db.clone()),
        bus.clone(),
    ));
    tokio::spawn(run_scheduler(
        ReminderStore::new(db),
        bus.clone(),
        // With the budget reached a scheduled task is printed and not run.
        move |text| {
            if mika_core::costs::unattended_allowed() {
                tasks.submit_task(&text);
            }
        },
    ));
    // Scenarios by event and "wait and tell" (decision 0031): a scenario runs
    // as a task, what she watched for is printed as a line of her own.
    let scenarios = session.clone();
    let told = bus.clone();
    let events_config = config.events.clone();
    let windows_on = !config.tools.disabled_groups.iter().any(|g| g == "windows");
    tokio::spawn(mika_tools::observe::run(mika_tools::observe::ServiceDeps {
        sensors: env.sensors.clone(),
        windows: env.windows.clone(),
        triggers: env.shared.triggers.clone(),
        commands,
        waits: env.shared.waits.clone(),
        config: Arc::new(move || events_config.clone()),
        windows_on: Arc::new(move || windows_on),
        paid_allowed: Arc::new(mika_core::costs::unattended_allowed),
        on_task: Arc::new(move |shown: &str, full: &str| scenarios.submit_task_as(shown, full)),
        on_tell: Arc::new(move |text: &str, kind: &str| {
            told.publish(Event::Remark {
                text: text.into(),
                kind: kind.into(),
            });
        }),
    }));

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

#[cfg(test)]
mod tests {
    use super::{budget_line, pause_line};
    use mika_core::costs::{BudgetAlert, BudgetPause, BudgetPeriod};

    #[test]
    fn a_budget_pause_is_one_line_with_the_money() {
        let pause = BudgetPause {
            period: BudgetPeriod::Month,
            spent: 2000.0,
            limit: 2000.0,
            currency: "RUB".into(),
        };
        assert_eq!(
            pause_line(&pause),
            "[costs] month budget reached (2\u{a0}000\u{a0}₽ of 2\u{a0}000\u{a0}₽): scenarios and scheduled tasks are paused"
        );
    }

    #[test]
    fn a_budget_warning_is_one_line_with_the_money() {
        let alert = |period, level| BudgetAlert {
            period,
            level,
            spent: 80.4,
            limit: 100.0,
            currency: "RUB".into(),
        };
        assert_eq!(
            budget_line(&alert(BudgetPeriod::Day, 80)),
            "[costs] day budget at 80%: 80,40\u{a0}₽ of 100\u{a0}₽"
        );
        assert!(budget_line(&alert(BudgetPeriod::Month, 100))
            .starts_with("[costs] month budget at 100%: "));
    }
}
