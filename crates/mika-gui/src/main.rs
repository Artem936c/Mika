#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bridge;
mod platform;
mod strings;
mod tray;
mod window;

use std::sync::Arc;

use mika_core::{EventBus, StateHandle};
use parking_lot::Mutex;
use tauri::Manager;

use window::IslandRect;

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
    let emotions = mika_core::emotion::EmotionDriver::new(bus.clone());
    let name = ctx.config.lock().assistant.name.clone();
    let avatar_on_desktop = ctx.config.lock().avatar.on_desktop;

    tauri::Builder::default()
        .plugin(tray::global_shortcut_plugin())
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
        ])
        .setup(move |app| {
            let island = app
                .get_webview_window("island")
                .expect("island window in tauri.conf.json");
            let (origin, scale) = window::place_on_primary(&island)?;
            island.set_ignore_cursor_events(true)?;
            window::spawn_click_through(island.clone(), rect.clone(), origin, scale);
            bridge::spawn_bridge(app.handle().clone(), bus.clone());
            tauri::async_runtime::spawn(emotions.run());
            window::show_without_activation(&island)?;
            if avatar_on_desktop {
                window::set_desktop_avatar(app.handle(), true)?;
            }
            tray::install(app.handle(), &name)?;
            tray::register_shortcut(app.handle());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
