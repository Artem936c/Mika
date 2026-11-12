#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bridge;
mod catalog;
mod platform;
mod settings;
mod strings;
mod tray;
mod update;
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
                        log::warn!("notification failed: {e}");
                    }
                }
                Ok(Event::StateChanged { to, .. }) => {
                    let name = ctx.config.lock().assistant.name.clone();
                    tray::show_state(&app, &name, to);
                }
                Ok(Event::UiTarget { x, y, w, h }) => {
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
    platform::init_logging();
    let Some(_instance) = platform::single_instance() else {
        log::info!("Mika is already running");
        return;
    };
    log::info!("Mika {} starting", env!("CARGO_PKG_VERSION"));

    let bus = EventBus::new(1024);
    let state = StateHandle::new(bus.clone());
    let ctx = match app::Ctx::build(bus.clone(), state.clone()) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            log::error!("cannot start: {e}");
            std::process::exit(1);
        }
    };
    let rect = Arc::new(Mutex::new(IslandRect::default()));
    let watch_state = state.clone();
    let emotions = mika_core::emotion::EmotionDriver::new(bus.clone());
    let name = ctx.config.lock().assistant.name.clone();
    let avatar_on_desktop = ctx.config.lock().avatar.on_desktop;
    let monitor = ctx.config.lock().window.monitor.clone();
    let start_windowed = ctx.config.lock().window.mode == mika_core::config::WindowMode::Window;
    let saved_rect = ctx.config.lock().window.rect;
    let background = ctx.clone();

    tauri::Builder::default()
        .plugin(tray::global_shortcut_plugin())
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
            bridge::set_autostart,
            bridge::open_folder,
            bridge::wipe_all_data,
            bridge::set_assistant_name,
            bridge::set_persona,
            bridge::set_avatar_placement,
            bridge::get_emotion,
            bridge::set_window_mode,
            settings::get_tool_settings,
            settings::set_tool_group,
            settings::set_screen_enabled,
            settings::set_news_feed,
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
            settings::list_mcp,
            settings::save_mcp_server,
            settings::remove_mcp_server,
            settings::get_telegram,
            settings::save_telegram,
            settings::test_telegram,
            settings::get_media_providers,
            settings::save_media_providers,
            settings::load_picture,
            settings::open_picture,
            settings::problem_report,
            settings::get_voice_settings,
            settings::set_voice,
            settings::test_voice,
            settings::save_speech_settings,
            settings::set_microphone_enabled,
            settings::voice_listen,
            settings::voice_cancel,
            settings::test_microphone,
            settings::set_wake,
            settings::finish_onboarding,
            settings::get_update_info,
            settings::save_update_settings,
            settings::check_update,
            settings::install_update,
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
            tray::register_shortcut(app.handle());
            tray::register_listen_shortcut(app.handle());
            let _ = background.app.set(app.handle().clone());
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
