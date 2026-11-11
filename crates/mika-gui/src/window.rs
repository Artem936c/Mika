//! Island canvas placement and click-through (decision 0001).

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Deserialize;
use tauri::{PhysicalPosition, WebviewWindow, WindowEvent};

/// Logical pixels, decision 0001.
pub const CANVAS_WIDTH: f64 = 1000.0;
#[allow(dead_code)] // matches the window height in tauri.conf.json
pub const CANVAS_HEIGHT: f64 = 600.0;
/// Extra margin around the island rectangle, logical pixels.
pub const HIT_PAD: f64 = 4.0;

/// Top-left corner of the canvas in physical pixels: centred on the top edge of the monitor.
pub fn canvas_position(
    monitor_x: i32,
    monitor_y: i32,
    monitor_width: u32,
    scale: f64,
) -> (i32, i32) {
    let width = (CANVAS_WIDTH * scale).round() as i64;
    let x = monitor_x as i64 + (monitor_width as i64 - width) / 2;
    (x.max(monitor_x as i64) as i32, monitor_y)
}

/// Island rectangle in logical pixels relative to the canvas top-left corner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct IslandRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Cursor (physical screen pixels) inside the island grown by `HIT_PAD`.
/// Left and top edges are inside, right and bottom are not.
pub fn island_hit(cursor: (i32, i32), origin: (i32, i32), scale: f64, rect: IslandRect) -> bool {
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return false;
    }
    let left = origin.0 as f64 + (rect.x - HIT_PAD) * scale;
    let right = origin.0 as f64 + (rect.x + rect.w + HIT_PAD) * scale;
    let top = origin.1 as f64 + (rect.y - HIT_PAD) * scale;
    let bottom = origin.1 as f64 + (rect.y + rect.h + HIT_PAD) * scale;
    let (cx, cy) = (cursor.0 as f64, cursor.1 as f64);
    cx >= left && cx < right && cy >= top && cy < bottom
}

#[derive(Debug, Clone, Copy)]
struct Placement {
    origin: (i32, i32),
    scale: f64,
}

/// Places the canvas on the primary monitor. Returns the origin and scale it used.
pub fn place_on_primary(window: &WebviewWindow) -> tauri::Result<((i32, i32), f64)> {
    match window.primary_monitor()? {
        Some(m) => {
            let scale = m.scale_factor();
            let (x, y) = canvas_position(m.position().x, m.position().y, m.size().width, scale);
            window.set_position(PhysicalPosition::new(x, y))?;
            Ok(((x, y), scale))
        }
        None => Ok(((0, 0), 1.0)),
    }
}

/// Polls the cursor every 16 ms and toggles `set_ignore_cursor_events`.
/// Window position and scale are cached and updated from window events,
/// because reading them each tick goes through the main thread and stalls (decision 0001).
/// `origin` and `scale` come from `place_on_primary`: a hidden window reports (0, 0).
/// The cache is also refreshed once a second in case an event was missed.
pub fn spawn_click_through(
    window: WebviewWindow,
    rect: Arc<Mutex<IslandRect>>,
    origin: (i32, i32),
    scale: f64,
) {
    let placement = Arc::new(Mutex::new(Placement { origin, scale }));

    let cache = placement.clone();
    let events_window = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
            if let Ok(p) = events_window.inner_position() {
                cache.lock().origin = (p.x, p.y);
            }
        }
        WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
            cache.lock().scale = *scale_factor;
            if let Ok(p) = events_window.inner_position() {
                cache.lock().origin = (p.x, p.y);
            }
        }
        _ => {}
    });

    std::thread::Builder::new()
        .name("island-hit-test".into())
        .spawn(move || {
            let mut inside_prev: Option<bool> = None;
            let mut tick: u32 = 0;
            loop {
                tick = tick.wrapping_add(1);
                if tick.is_multiple_of(60) && window.is_visible().unwrap_or(false) {
                    if let (Ok(p), Ok(s)) = (window.inner_position(), window.scale_factor()) {
                        *placement.lock() = Placement {
                            origin: (p.x, p.y),
                            scale: s,
                        };
                    }
                }
                if let Some(cursor) = cursor_position() {
                    let p = *placement.lock();
                    let inside = island_hit(cursor, p.origin, p.scale, *rect.lock());
                    if inside_prev != Some(inside) {
                        if let Err(e) = window.set_ignore_cursor_events(!inside) {
                            log::warn!("set_ignore_cursor_events failed: {e}");
                        }
                        inside_prev = Some(inside);
                    }
                }
                std::thread::sleep(Duration::from_millis(16));
            }
        })
        .expect("spawn hit-test thread");
}

fn cursor_position() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: GetCursorPos writes into a valid POINT.
    let ok = unsafe { GetCursorPos(&mut p) } != 0;
    ok.then_some((p.x, p.y))
}

/// Shows the window without taking focus from the current foreground window.
pub fn show_without_activation(window: &WebviewWindow) -> tauri::Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOWNOACTIVATE};
    let hwnd = window.hwnd()?;
    // SAFETY: hwnd belongs to a live window owned by this process.
    unsafe {
        ShowWindow(hwnd.0 as _, SW_SHOWNOACTIVATE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centres_on_full_hd() {
        assert_eq!(canvas_position(0, 0, 1920, 1.0), (460, 0));
    }

    #[test]
    fn respects_scale_and_monitor_origin() {
        assert_eq!(canvas_position(1920, 0, 2560, 1.5), (2450, 0));
    }

    #[test]
    fn monitor_left_of_primary_has_negative_x() {
        assert_eq!(canvas_position(-1920, 120, 1920, 1.0), (-1460, 120));
    }

    #[test]
    fn monitor_narrower_than_canvas_clamps_to_left_edge() {
        assert_eq!(canvas_position(0, 0, 800, 1.0), (0, 0));
        assert_eq!(canvas_position(-800, 0, 800, 1.0), (-800, 0));
    }

    const IDLE: IslandRect = IslandRect {
        x: 382.0,
        y: 14.0,
        w: 236.0,
        h: 44.0,
    };

    #[test]
    fn hit_inside_island() {
        assert!(island_hit((960, 36), (460, 0), 1.0, IDLE));
    }

    #[test]
    fn hit_includes_pad_and_excludes_beyond_it() {
        assert!(island_hit((838, 36), (460, 0), 1.0, IDLE));
        assert!(!island_hit((837, 36), (460, 0), 1.0, IDLE));
        assert!(island_hit((960, 61), (460, 0), 1.0, IDLE));
        assert!(!island_hit((960, 62), (460, 0), 1.0, IDLE));
    }

    #[test]
    fn hit_transparent_canvas_area_is_false() {
        assert!(!island_hit((700, 300), (460, 0), 1.0, IDLE));
    }

    #[test]
    fn hit_respects_scale_and_negative_origin() {
        assert!(island_hit((-893, 30), (-1460, 0), 1.5, IDLE));
        assert!(!island_hit((-894, 30), (-1460, 0), 1.5, IDLE));
    }

    #[test]
    fn hit_with_empty_rect_is_false_outside_pad() {
        assert!(!island_hit((960, 36), (460, 0), 1.0, IslandRect::default()));
    }
}
