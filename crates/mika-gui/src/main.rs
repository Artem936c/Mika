#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod avatar_model;
mod backup;
mod bgjobs;
mod bridge;
mod catalog;
mod costs;
mod dayend;
mod events_page;
mod everyday;
mod ffmpeg_get;
mod file_dialog;
mod hotkey_actions;
mod hotkeys;
mod journal;
mod lang;
mod platform;
mod research;
mod rhythm;
mod selection;
mod settings;
mod strings;
mod talk_settings;
mod tray;
mod update;
mod vosk;
mod window;

use std::sync::Arc;

use mika_core::{EventBus, StateHandle};
use parking_lot::Mutex;
use tauri::Manager;

use window::IslandRect;

/// Windows notifications for fired reminders, the frame around a click target and
/// the state on the tray icon.
fn spawn_notifier(app: tauri::AppHandle, bus: EventBus, ctx: Arc<app::Ctx>) {
    use mika_core::Event;
    use tauri_plugin_notification::NotificationExt;
    use tokio::sync::broadcast::error::RecvError;
    let mut events = bus.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(Event::ReminderFired { text, .. }) => {
                    let title = ctx.config.lock().assistant.name.clone();
                    if let Err(e) = app.notification().builder().title(title).body(text).show() {
                        log::warn!("the Windows notification of a reminder was not shown: {e}");
                    }
                }
                // What she was asked to watch for: the user may be in another program by now.
                Ok(Event::Remark { text, kind }) if kind == mika_core::triggers::REMARK_WATCH => {
                    let title = ctx.config.lock().assistant.name.clone();
                    if let Err(e) = app.notification().builder().title(title).body(text).show() {
                        log::warn!("the Windows notification of a watch was not shown: {e}");
                    }
                }
                Ok(Event::StateChanged { to, .. }) => {
                    let name = ctx.config.lock().assistant.name.clone();
                    tray::show_state(&app, &name, to);
                }
                Ok(Event::BgJobUpdated { job }) if job.live => {
                    // The board, not the event, says whether a recording runs: an event can be late.
                    let on = bgjobs::recording(&ctx.jobs).is_some();
                    let name = ctx.config.lock().assistant.name.clone();
                    tray::set_recording(&app, &name, ctx.state(), on);
                }
                Ok(Event::UiTarget { x, y, w, h }) => {
                    // First, before the frame waits for the main thread.
                    window::pass_clicks_at(x, y, w, h);
                    if let Err(e) = window::flash_target(&app, x, y, w, h) {
                        log::warn!("target frame failed: {e}");
                    }
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Remembers where the ordinary window is, and makes its close button bring
/// the island back instead of quitting.
fn track_window(app: tauri::AppHandle, window: &tauri::WebviewWindow, ctx: Arc<app::Ctx>) {
    use std::sync::atomic::Ordering;
    use tauri::WindowEvent;
    let tracked = window.clone();
    window.on_window_event(move |event| {
        if !window::WINDOWED.load(Ordering::Relaxed) {
            return;
        }
        match event {
            WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
                if let Some(rect) = window::window_rect(&tracked) {
                    ctx.config.lock().window.rect = Some(rect);
                }
            }
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                if let Err(e) = bridge::apply_window_mode(&app, &ctx, false) {
                    log::warn!("cannot return to the island: {e}");
                }
            }
            _ => {}
        }
    });
}

fn main() {
    // Before the journal: the copy that is already running owns its files.
    let Some(_instance) = platform::single_instance() else {
        eprintln!("Mika is already running");
        return;
    };
    platform::init_logging();
    log::info!(
        target: "app",
        "Mika {} starting on {}",
        env!("CARGO_PKG_VERSION"),
        platform::windows_build()
    );

    let bus = EventBus::new(1024);
    let state = StateHandle::new(bus.clone());
    let ctx = match app::Ctx::build(bus.clone(), state.clone()) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            log::error!(target: "app", "cannot start: {e}");
            // Said and written down: the next start has nothing to warn about.
            platform::end_logging();
            std::process::exit(1);
        }
    };
    {
        // Names of what is on, not what the settings hold.
        let config = ctx.config.lock();
        // What the hotkey and voice features hang on is named too: without
        // a recognition model dictation, the voice question and Telegram
        // voice all refuse, and that is the first thing to look at.
        let set = |value: &str| {
            if value.trim().is_empty() {
                "none"
            } else {
                "set"
            }
        };
        log::info!(
            target: "app",
            "started: language={} provider={} tools={} recognition_model={} vision_model={} level={} switches_on={}",
            mika_core::i18n::lang().code(),
            config.llm.kind.as_str(),
            ctx.session.agent.registry().len(),
            set(&config.voice.stt_model),
            set(&config.llm.vision_model),
            config.log.level.name(),
            config.switches_on().join(",")
        );
    }
    let rect = Arc::new(Mutex::new(IslandRect::default()));
    let watch_state = state.clone();
    let emotions = mika_core::emotion::EmotionDriver::new(bus.clone());
    let emotion_now = emotions.handle();
    let name = ctx.config.lock().assistant.name.clone();
    let avatar_on_desktop = ctx.config.lock().avatar.on_desktop;
    let monitor = ctx.config.lock().window.monitor.clone();
    let start_windowed = ctx.config.lock().window.mode == mika_core::config::WindowMode::Window;
    let saved_rect = ctx.config.lock().window.rect;
    let background = ctx.clone();

    tauri::Builder::default()
        .plugin(hotkeys::plugin())
        .plugin(tauri_plugin_notification::init())
        .manage(state)
        .manage(ctx)
        .manage(rect.clone())
        .manage(emotions.handle())
        .invoke_handler(tauri::generate_handler![
            bridge::get_state,
            bridge::debug_set_state,
            bridge::set_island_rect,
            bridge::send_message,
            bridge::stop,
            bridge::get_paused,
            bridge::poke_avatar,
            catalog::list_models,
            catalog::list_voices,
            bridge::get_history,
            bridge::clear_history,
            bridge::get_llm_settings,
            bridge::save_llm_settings,
            bridge::test_provider,
            bridge::get_ui_settings,
            bridge::set_speak_replies,
            bridge::answer_confirm,
            bridge::get_pending_confirm,
            bridge::list_rules,
            bridge::remove_rule,
            bridge::recent_audit,
            bridge::set_web_enabled,
            bridge::focus_island,
            bridge::get_general_settings,
            lang::get_language,
            lang::set_language,
            bridge::set_autostart,
            bridge::open_folder,
            bridge::wipe_all_data,
            bridge::set_assistant_name,
            bridge::set_persona,
            bridge::set_avatar_placement,
            avatar_model::avatar_motions,
            avatar_model::avatar_motion_add,
            avatar_model::avatar_motion_rename,
            avatar_model::avatar_motion_delete,
            avatar_model::avatar_motion_bytes,
            avatar_model::avatar_motion_play,
            avatar_model::avatar_hover,
            avatar_model::avatar_dragging,
            avatar_model::avatar_cursor_wanted,
            avatar_model::avatar_typing,
            avatar_model::avatar_look,
            avatar_model::avatar_set_look,
            avatar_model::avatar_scale_step,
            avatar_model::avatar_models,
            avatar_model::avatar_model_add,
            avatar_model::avatar_model_select,
            avatar_model::avatar_model_delete,
            avatar_model::avatar_model_profile,
            avatar_model::avatar_model_bytes,
            avatar_model::avatar_model_set_slot,
            avatar_model::avatar_model_set_lip,
            avatar_model::avatar_model_save_stills,
            avatar_model::avatar_model_stills,
            avatar_model::avatar_model_failed,
            bridge::get_emotion,
            bridge::set_window_mode,
            settings::get_tool_settings,
            settings::set_tool_group,
            settings::set_screen_enabled,
            settings::set_news_feed,
            settings::get_ffmpeg_status,
            settings::set_ffmpeg_path,
            settings::install_ffmpeg,
            settings::remove_ffmpeg,
            settings::open_av_file,
            settings::get_island_settings,
            settings::set_island_settings,
            settings::set_island_monitor,
            settings::idle_info,
            settings::get_memory,
            settings::add_fact,
            settings::remove_fact,
            settings::clear_facts,
            settings::clear_archive,
            settings::save_embedding_settings,
            settings::test_embedding,
            settings::list_reminders,
            settings::cancel_reminder,
            settings::list_commands,
            settings::save_command,
            settings::delete_command,
            settings::list_skills,
            settings::delete_skill,
            settings::set_skill_offers,
            settings::skill_offer_accept,
            settings::list_mcp,
            settings::save_mcp_server,
            settings::remove_mcp_server,
            settings::get_telegram,
            settings::save_telegram,
            settings::test_telegram,
            settings::set_telegram_voice,
            settings::get_media_providers,
            settings::save_media_providers,
            settings::load_picture,
            settings::open_picture,
            settings::problem_report,
            journal::journal_view,
            journal::journal_read,
            journal::journal_tail,
            journal::set_journal,
            journal::journal_export,
            journal::journal_clear,
            journal::journal_crash_seen,
            settings::get_voice_settings,
            settings::set_voice,
            settings::test_voice,
            settings::set_tts_cache,
            settings::set_mood_directions,
            settings::clear_speech_cache,
            settings::install_vosk,
            settings::set_follow_up,
            settings::set_duck_media,
            settings::set_notifications,
            settings::set_briefing,
            settings::get_rhythm_settings,
            settings::set_rhythm,
            settings::set_tracking,
            settings::clear_app_time,
            settings::clear_clipboard_history,
            settings::start_focus,
            settings::stop_focus,
            settings::save_calendar,
            settings::set_docs_folders,
            settings::reindex_docs,
            settings::set_voice_style,
            settings::set_brief_speech,
            settings::set_expressive_speech,
            settings::remove_vosk,
            settings::save_speech_settings,
            settings::set_microphone_enabled,
            settings::voice_listen,
            settings::voice_cancel,
            settings::test_microphone,
            settings::set_wake,
            settings::finish_onboarding,
            settings::get_update_info,
            research::get_research_settings,
            research::set_research_settings,
            settings::save_update_settings,
            bgjobs::bg_jobs_list,
            bgjobs::bg_job_stop,
            settings::check_update,
            backup::backup_export,
            backup::backup_reveal,
            backup::backup_import_pick,
            backup::backup_import_preview,
            backup::backup_import_apply,
            backup::backup_import_cancel,
            backup::backup_restart,
            settings::install_update,
            events_page::get_events,
            events_page::set_events_enabled,
            events_page::set_trigger_enabled,
            events_page::delete_trigger,
            events_page::stop_wait,
            selection::selection_action,
            selection::selection_paste,
            hotkeys::hotkeys_get,
            hotkeys::hotkeys_set,
            hotkeys::hotkeys_reset,
            hotkeys::hotkeys_pause,
            hotkey_actions::dictation_set_enabled,
            hotkey_actions::screen_ask_set_enabled,
            hotkey_actions::screen_ask_preview,
            hotkey_actions::screen_ask_send,
            hotkey_actions::screen_ask_listen,
            hotkey_actions::screen_ask_cancel,
            selection::selection_clear,
            costs::get_costs,
            costs::set_costs_enabled,
            costs::set_cost_budget,
            costs::set_cost_pause,
            costs::set_cost_price,
            costs::remove_cost_price,
            costs::refresh_cost_prices,
            costs::clear_costs,
            dayend::get_evening_summary,
            dayend::set_evening_summary,
            dayend::evening_summary_now,
        ])
        .setup(move |app| {
            let island = app
                .get_webview_window("island")
                .expect("island window in tauri.conf.json");
            let (origin, scale) = window::place_on(&island, monitor.as_deref())?;
            island.set_ignore_cursor_events(true)?;
            let drop_app = app.handle().clone();
            let drop_ctx = background.clone();
            window::spawn_click_through(island.clone(), rect.clone(), origin, scale, move || {
                // Off the hit-test thread: placing a window talks to the main thread.
                let (app, ctx) = (drop_app.clone(), drop_ctx.clone());
                tauri::async_runtime::spawn(async move { bridge::handle_drop(&app, &ctx) });
            });
            bridge::spawn_bridge(app.handle().clone(), bus.clone());
            tauri::async_runtime::spawn(emotions.run());
            if start_windowed {
                window::enter_window_mode(&island, saved_rect)?;
            } else {
                window::show_without_activation(&island)?;
            }
            track_window(app.handle().clone(), &island, background.clone());
            if avatar_on_desktop {
                window::set_desktop_avatar(app.handle(), true)?;
            }
            tray::install(app.handle(), &name)?;
            hotkeys::register_all(app.handle(), &background);
            let _ = background.app.set(app.handle().clone());
            // Her settings change by conversation the way the pages change them (decision 0046).
            background
                .tool_env()
                .shared
                .settings
                .set(Arc::new(talk_settings::TalkSettings::new(
                    app.handle().clone(),
                )));
            let _ = background.emotion.set(emotion_now.clone());
            background.start_background();
            spawn_notifier(app.handle().clone(), bus.clone(), background.clone());
            let watched = background.clone();
            let busy_state = watch_state.clone();
            let asking_state = watch_state.clone();
            window::spawn_game_watch(
                app.handle().clone(),
                move || watched.config.lock().island.hide_in_games,
                move || busy_state.get() != mika_core::AssistantState::Idle,
                move || asking_state.get() == mika_core::AssistantState::AwaitingConfirm,
            );
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // From here on the run counts as ended cleanly. The end of a
                // Windows session comes this way too, and Windows may stop the
                // process while the jobs below are still being saved.
                log::info!(target: "app", "exit");
                platform::end_logging();
                // Background jobs are stopped, a recording is saved and half-written
                // files are removed before the process ends. This waits for them,
                // so from here on nothing asks this thread to redraw the tray.
                tray::closing();
                app.state::<Arc<crate::app::Ctx>>().shutdown_media();
                // Programs quietened for her voice get their sound back.
                app.state::<Arc<crate::app::Ctx>>().restore_media();
                // What the shutdown itself wrote.
                mika_core::journal::flush();
            }
        });
}
