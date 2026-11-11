//! Spike 0.1: transparent always-on-top island window with click-through.
//!
//! Mode `canvas` (default): fixed 900x540 transparent window, cursor events ignored
//! except while the global cursor is over the island rectangle reported by the page.
//! Mode `resize` (SPIKE_MODE=resize): the OS window itself is resized every frame.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;
use tauri::{LogicalSize, Manager, PhysicalPosition, WebviewWindow};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

const CANVAS_W: f64 = 900.0;
const CANVAS_H: f64 = 540.0;
const HIT_PAD: f64 = 4.0;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

struct Shared {
    mode: String,
    rect: Mutex<Rect>,
    started: Instant,
}

fn ts(shared: &Shared) -> String {
    format!("{:>8.3}", shared.started.elapsed().as_secs_f64())
}

#[tauri::command]
fn get_mode(shared: tauri::State<'_, Arc<Shared>>) -> String {
    shared.mode.clone()
}

#[tauri::command]
fn set_island_rect(shared: tauri::State<'_, Arc<Shared>>, rect: Rect) {
    *shared.rect.lock() = rect;
}

#[tauri::command]
fn report(shared: tauri::State<'_, Arc<Shared>>, msg: String) {
    println!("[{}] [page] {msg}", ts(&shared));
}

/// Resize mode: page drives the OS window size every animation frame.
#[tauri::command]
fn resize_window(window: WebviewWindow, w: f64, h: f64) -> Result<(), String> {
    let monitor = window
        .primary_monitor()
        .map_err(|e| e.to_string())?
        .ok_or("no monitor")?;
    let scale = monitor.scale_factor();
    let mpos = monitor.position();
    let msize = monitor.size();
    let pw = (w * scale).round() as i32;
    let x = mpos.x + (msize.width as i32 - pw) / 2;
    let y = mpos.y + (14.0 * scale).round() as i32;
    window
        .set_size(LogicalSize::new(w, h))
        .map_err(|e| e.to_string())?;
    window
        .set_position(PhysicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn place_canvas(window: &WebviewWindow) -> tauri::Result<()> {
    if let Some(m) = window.primary_monitor()? {
        let scale = m.scale_factor();
        let pw = (CANVAS_W * scale).round() as i32;
        let x = (m.position().x + (m.size().width as i32 - pw) / 2).max(m.position().x);
        window.set_position(PhysicalPosition::new(x, m.position().y))?;
        println!(
            "[setup] monitor {}x{} at ({},{}) scale {scale} -> canvas x={x}",
            m.size().width,
            m.size().height,
            m.position().x,
            m.position().y
        );
    }
    Ok(())
}

fn spawn_hit_test(window: WebviewWindow, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let mut inside_prev: Option<bool> = None;
        let mut toggles = 0u64;
        let mut worst_tick = Duration::ZERO;
        let mut last_stat = Instant::now();
        loop {
            let tick = Instant::now();
            let mut p = POINT { x: 0, y: 0 };
            // SAFETY: GetCursorPos writes into a valid POINT.
            let ok = unsafe { GetCursorPos(&mut p) } != 0;
            if ok {
                if let (Ok(origin), Ok(scale)) = (window.inner_position(), window.scale_factor()) {
                    let r = *shared.rect.lock();
                    let left = origin.x as f64 + (r.x - HIT_PAD) * scale;
                    let top = origin.y as f64 + (r.y - HIT_PAD) * scale;
                    let right = origin.x as f64 + (r.x + r.w + HIT_PAD) * scale;
                    let bottom = origin.y as f64 + (r.y + r.h + HIT_PAD) * scale;
                    let (cx, cy) = (p.x as f64, p.y as f64);
                    let inside = cx >= left && cx < right && cy >= top && cy < bottom;
                    if inside_prev != Some(inside) {
                        if let Err(e) = window.set_ignore_cursor_events(!inside) {
                            println!("[{}] [hit] set_ignore_cursor_events failed: {e}", ts(&shared));
                        }
                        toggles += 1;
                        println!(
                            "[{}] [hit] inside={inside} cursor=({},{}) rect=({left:.0},{top:.0})-({right:.0},{bottom:.0})",
                            ts(&shared),
                            p.x,
                            p.y
                        );
                        inside_prev = Some(inside);
                    }
                }
            }
            worst_tick = worst_tick.max(tick.elapsed());
            if last_stat.elapsed() >= Duration::from_secs(10) {
                println!(
                    "[{}] [hit] stats toggles={toggles} worst_tick_us={}",
                    ts(&shared),
                    worst_tick.as_micros()
                );
                worst_tick = Duration::ZERO;
                last_stat = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    });
}

fn main() {
    let mode = std::env::var("SPIKE_MODE").unwrap_or_else(|_| "canvas".into());
    let shared = Arc::new(Shared {
        mode: mode.clone(),
        rect: Mutex::new(Rect::default()),
        started: Instant::now(),
    });

    tauri::Builder::default()
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            get_mode,
            set_island_rect,
            report,
            resize_window
        ])
        .setup(move |app| {
            let window = app.get_webview_window("island").expect("island window");
            if mode == "canvas" {
                place_canvas(&window)?;
                window.set_ignore_cursor_events(true)?;
                spawn_hit_test(window.clone(), shared.clone());
            }
            window.show()?;
            println!("[{}] [ready] mode={mode} pid={}", ts(&shared), std::process::id());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
