//! Island canvas placement and click-through (decision 0001).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

pub const AVATAR_LABEL: &str = "avatar";
/// Desktop avatar window, logical pixels: the speech bubble on the left, the figure on the right.
pub const AVATAR_WIDTH: f64 = 560.0;
pub const AVATAR_HEIGHT: f64 = 470.0;
const AVATAR_RIGHT: f64 = 24.0;
/// Leaves room for the taskbar.
const AVATAR_BOTTOM: f64 = 64.0;

/// Top-left corner of the avatar window in physical pixels: bottom-right corner of the monitor.
pub fn avatar_position(
    monitor_x: i32,
    monitor_y: i32,
    monitor_width: u32,
    monitor_height: u32,
    scale: f64,
) -> (i32, i32) {
    let w = ((AVATAR_WIDTH + AVATAR_RIGHT) * scale).round() as i64;
    let h = ((AVATAR_HEIGHT + AVATAR_BOTTOM) * scale).round() as i64;
    let x = monitor_x as i64 + monitor_width as i64 - w;
    let y = monitor_y as i64 + monitor_height as i64 - h;
    (
        x.max(monitor_x as i64) as i32,
        y.max(monitor_y as i64) as i32,
    )
}

/// Creates and shows the desktop avatar window, or closes it.
/// The window passes every click through: it only shows the character.
pub fn set_desktop_avatar(app: &tauri::AppHandle, on: bool) -> tauri::Result<()> {
    use tauri::Manager;
    let existing = app.get_webview_window(AVATAR_LABEL);
    if !on {
        if let Some(w) = existing {
            w.close()?;
        }
        return Ok(());
    }
    let window = match existing {
        Some(w) => w,
        None => tauri::WebviewWindowBuilder::new(
            app,
            AVATAR_LABEL,
            tauri::WebviewUrl::App("index.html?view=avatar".into()),
        )
        .title("Mika avatar")
        .inner_size(AVATAR_WIDTH, AVATAR_HEIGHT)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false)
        .build()?,
    };
    if let Some(m) = window.primary_monitor()? {
        let (x, y) = avatar_position(
            m.position().x,
            m.position().y,
            m.size().width,
            m.size().height,
            m.scale_factor(),
        );
        window.set_position(PhysicalPosition::new(x, y))?;
    }
    window.set_ignore_cursor_events(true)?;
    show_without_activation(&window)
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

/// Where Mika is about to click in another program, physical pixels
/// `[x, y, w, h]`, and until when. The spot may lie under the island: the
/// picture she found it on leaves her windows out (decision 0025).
type Passing = Option<(Instant, [i32; 4])>;

static PASSING: Mutex<Passing> = parking_lot::const_mutex(None);

/// How long the island lets clicks through the spot of her click: the pause
/// before the click, a UI Automation try and the click itself fit inside.
const PASS_HOLD: Duration = Duration::from_secs(5);

/// Margin around that spot, physical pixels.
const PASS_PAD: i32 = 8;

/// Mika shows where she is about to click (`Event::UiTarget`): the island lets
/// clicks through that spot for a while, so her click lands in the window she
/// aims at, not in the island. Elsewhere the island takes clicks as before.
pub fn pass_clicks_at(x: i32, y: i32, w: i32, h: i32) {
    *PASSING.lock() = Some((Instant::now() + PASS_HOLD, [x, y, w, h]));
}

/// Whether a click at `cursor` goes through the island to the spot Mika is
/// clicking.
fn passes_through(cursor: (i32, i32), now: Instant, passing: Passing) -> bool {
    passing.is_some_and(|(until, [x, y, w, h])| {
        now < until
            && cursor.0 >= x - PASS_PAD
            && cursor.0 < x + w + PASS_PAD
            && cursor.1 >= y - PASS_PAD
            && cursor.1 < y + h + PASS_PAD
    })
}

#[derive(Debug, Clone, Copy)]
struct Placement {
    origin: (i32, i32),
    scale: f64,
}

/// Names of the connected monitors, for the settings page.
pub fn monitor_names(window: &WebviewWindow) -> Vec<String> {
    window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m.name().cloned())
        .collect()
}

/// Places the canvas on the named monitor, or on the primary one when the name
/// is empty or that monitor is gone. Returns the origin and scale it used.
pub fn place_on(window: &WebviewWindow, name: Option<&str>) -> tauri::Result<((i32, i32), f64)> {
    let named = match name.filter(|n| !n.is_empty()) {
        Some(n) => window
            .available_monitors()?
            .into_iter()
            .find(|m| m.name().is_some_and(|x| x == n)),
        None => None,
    };
    let monitor = match named {
        Some(m) => Some(m),
        None => window.primary_monitor()?,
    };
    match monitor {
        Some(m) => {
            let scale = m.scale_factor();
            let (x, y) = canvas_position(m.position().x, m.position().y, m.size().width, scale);
            window.set_position(PhysicalPosition::new(x, y))?;
            Ok(((x, y), scale))
        }
        None => Ok(((0, 0), 1.0)),
    }
}

/// True while the main window is an ordinary window, not the island.
/// The hit-test and full-screen threads leave such a window alone.
pub static WINDOWED: AtomicBool = AtomicBool::new(false);

/// Ordinary window: first size and the smallest one, logical pixels.
pub const WINDOW_DEFAULT: (f64, f64) = (920.0, 640.0);
pub const WINDOW_MIN: (f64, f64) = (620.0, 460.0);

/// True when at least an 80 px corner of the title bar lies on some monitor,
/// so the window can be grabbed. Monitors are (x, y, width, height).
pub fn rect_is_reachable(rect: [i32; 4], monitors: &[(i32, i32, u32, u32)]) -> bool {
    let [x, y, w, h] = rect;
    if w < 200 || h < 150 {
        return false;
    }
    monitors.iter().any(|(mx, my, mw, mh)| {
        let (right, bottom) = (mx + *mw as i32, my + *mh as i32);
        x + w > mx + 80 && x < right - 80 && y >= *my - 10 && y < bottom - 60
    })
}

/// Turns the island into an ordinary window: free size, a place in the taskbar,
/// not on top. It keeps no Windows frame: the page draws its own header and edges.
/// `saved` is where it was last time.
pub fn enter_window_mode(window: &WebviewWindow, saved: Option<[i32; 4]>) -> tauri::Result<()> {
    WINDOWED.store(true, Ordering::Relaxed);
    window.set_ignore_cursor_events(false)?;
    window.set_always_on_top(false)?;
    window.set_skip_taskbar(false)?;
    window.set_resizable(true)?;
    window.set_min_size(Some(tauri::LogicalSize::new(WINDOW_MIN.0, WINDOW_MIN.1)))?;
    let monitors: Vec<(i32, i32, u32, u32)> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            (
                m.position().x,
                m.position().y,
                m.size().width,
                m.size().height,
            )
        })
        .collect();
    match saved.filter(|r| rect_is_reachable(*r, &monitors)) {
        Some([x, y, w, h]) => {
            window.set_size(tauri::PhysicalSize::new(w as u32, h as u32))?;
            window.set_position(PhysicalPosition::new(x, y))?;
        }
        None => {
            window.set_size(tauri::LogicalSize::new(WINDOW_DEFAULT.0, WINDOW_DEFAULT.1))?;
            window.center()?;
        }
    }
    window.show()?;
    window.set_focus()
}

/// Turns the ordinary window back into the island at the top of the monitor.
pub fn enter_island_mode(window: &WebviewWindow, monitor: Option<&str>) -> tauri::Result<()> {
    if window.is_maximized().unwrap_or(false) {
        window.unmaximize()?;
    }
    window.set_min_size(None::<tauri::LogicalSize<f64>>)?;
    window.set_size(tauri::LogicalSize::new(CANVAS_WIDTH, CANVAS_HEIGHT))?;
    window.set_resizable(false)?;
    window.set_always_on_top(true)?;
    window.set_skip_taskbar(true)?;
    place_on(window, monitor)?;
    WINDOWED.store(false, Ordering::Relaxed);
    Ok(())
}

/// Position and size of the window in physical pixels, for the config.
pub fn window_rect(window: &WebviewWindow) -> Option<[i32; 4]> {
    if window.is_maximized().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
        return None;
    }
    let p = window.outer_position().ok()?;
    let s = window.inner_size().ok()?;
    Some([p.x, p.y, s.width as i32, s.height as i32])
}

/// One monitor in physical pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorBox {
    /// `None` for the primary monitor: that is how the config stores it.
    pub name: Option<String>,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub scale: f64,
}

impl MonitorBox {
    fn contains(&self, p: (i32, i32)) -> bool {
        p.0 >= self.x
            && p.0 < self.x + self.w as i32
            && p.1 >= self.y
            && p.1 < self.y + self.h as i32
    }
}

/// What happens when the user lets go of the window.
#[derive(Debug, Clone, PartialEq)]
pub enum DropAction {
    /// The island sits at the top of this monitor.
    Island { monitor: Option<String> },
    /// An ordinary window at this place: x, y, width, height.
    Window { rect: [i32; 4] },
    /// The ordinary window stays where it was dropped.
    Stay,
}

/// An island dropped this close to the top edge stays an island, logical pixels.
pub const ISLAND_SNAP: f64 = 110.0;
/// An ordinary window becomes the island when the cursor lets go this close
/// to the top edge, logical pixels.
pub const WINDOW_SNAP: f64 = 12.0;

/// Decides what a drop means. `cursor` and `island_top` are physical screen
/// pixels; `size` is the size an ordinary window would get, logical pixels.
pub fn decide_drop(
    windowed: bool,
    cursor: (i32, i32),
    island_top: i32,
    monitors: &[MonitorBox],
    size: (f64, f64),
) -> DropAction {
    let Some(m) = monitors
        .iter()
        .find(|m| m.contains(cursor))
        .or_else(|| monitors.iter().find(|m| m.name.is_none()))
        .or_else(|| monitors.first())
    else {
        return DropAction::Stay;
    };
    if windowed {
        return if cursor.1 - m.y <= (WINDOW_SNAP * m.scale) as i32 {
            DropAction::Island {
                monitor: m.name.clone(),
            }
        } else {
            DropAction::Stay
        };
    }
    if island_top - m.y <= (ISLAND_SNAP * m.scale) as i32 {
        return DropAction::Island {
            monitor: m.name.clone(),
        };
    }
    // The window appears under the cursor, which ends up on its header.
    let w = ((size.0 * m.scale) as i32).min(m.w as i32);
    let h = ((size.1 * m.scale) as i32).min(m.h as i32);
    let x = (cursor.0 - w / 2).clamp(m.x, m.x + m.w as i32 - w);
    let y = (cursor.1 - (24.0 * m.scale) as i32).clamp(m.y, m.y + m.h as i32 - h);
    DropAction::Window { rect: [x, y, w, h] }
}

/// The connected monitors, the primary one without a name.
pub fn monitor_boxes(window: &WebviewWindow) -> Vec<MonitorBox> {
    let primary = window
        .primary_monitor()
        .ok()
        .flatten()
        .and_then(|m| m.name().cloned());
    window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| MonitorBox {
            name: m.name().filter(|n| Some(*n) != primary.as_ref()).cloned(),
            x: m.position().x,
            y: m.position().y,
            w: m.size().width,
            h: m.size().height,
            scale: m.scale_factor(),
        })
        .collect()
}

/// True while the user drags the window with the mouse.
static DRAGGING: AtomicBool = AtomicBool::new(false);
/// True when the current gesture changed the size: it was a resize, not a move.
static RESIZING: AtomicBool = AtomicBool::new(false);

fn left_button_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const VK_LBUTTON: i32 = 0x01;
    // SAFETY: plain query of a key state.
    (unsafe { GetAsyncKeyState(VK_LBUTTON) } as u16) & 0x8000 != 0
}

/// Call on every "moved" event: a move while the left button is down is a drag.
/// Moves made by the program itself come with the button up.
pub fn note_moved() {
    if left_button_down() {
        DRAGGING.store(true, Ordering::Relaxed);
    }
}

/// Call on every "resized" event: a resize with the left button down is a resize gesture.
pub fn note_resized() {
    if left_button_down() {
        RESIZING.store(true, Ordering::Relaxed);
    }
}

/// True once, right after the user lets go of a dragged window.
/// A gesture that changed the size was a resize and does not count.
pub fn drag_ended() -> bool {
    if left_button_down() || !(DRAGGING.load(Ordering::Relaxed) || RESIZING.load(Ordering::Relaxed))
    {
        return false;
    }
    let moved = DRAGGING.swap(false, Ordering::Relaxed);
    let resized = RESIZING.swap(false, Ordering::Relaxed);
    moved && !resized
}

/// What the drop of the main window means now.
pub fn drop_action(window: &WebviewWindow, saved_size: Option<(f64, f64)>) -> DropAction {
    let Some(cursor) = cursor_position() else {
        return DropAction::Stay;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    let top = window
        .outer_position()
        .map(|p| p.y + (ISLAND_TOP * scale) as i32)
        .unwrap_or(0);
    decide_drop(
        WINDOWED.load(Ordering::Relaxed),
        cursor,
        top,
        &monitor_boxes(window),
        saved_size.unwrap_or(WINDOW_DEFAULT),
    )
}

/// Gap between the top of the canvas and the island, logical pixels (as in the page).
pub const ISLAND_TOP: f64 = 14.0;

/// Moves the island to another monitor. The hit-test cache follows through the
/// window "moved" event.
pub fn move_island(app: &tauri::AppHandle, name: &str) -> tauri::Result<()> {
    use tauri::Manager;
    if let Some(island) = app.get_webview_window("island") {
        place_on(&island, Some(name))?;
    }
    Ok(())
}

pub const TARGET_LABEL: &str = "target";
/// Frame thickness around the element about to be clicked, physical pixels.
const TARGET_PAD: i32 = 6;

/// Shows a frame around a screen rectangle for a moment: the user sees where
/// the assistant is about to click.
pub fn flash_target(app: &tauri::AppHandle, x: i32, y: i32, w: i32, h: i32) -> tauri::Result<()> {
    use tauri::Manager;
    let window = match app.get_webview_window(TARGET_LABEL) {
        Some(win) => win,
        None => tauri::WebviewWindowBuilder::new(
            app,
            TARGET_LABEL,
            tauri::WebviewUrl::App("index.html?view=target".into()),
        )
        .title("Mika target")
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false)
        .build()?,
    };
    window.set_ignore_cursor_events(true)?;
    window.set_position(PhysicalPosition::new(x - TARGET_PAD, y - TARGET_PAD))?;
    window.set_size(tauri::PhysicalSize::new(
        (w + 2 * TARGET_PAD).max(1) as u32,
        (h + 2 * TARGET_PAD).max(1) as u32,
    ))?;
    show_without_activation(&window)?;
    let hide = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(900)).await;
        let _ = hide.hide();
    });
    Ok(())
}

/// True while a full-screen program (a game, a presentation) is in front.
pub fn fullscreen_app_running() -> bool {
    use windows_sys::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_BUSY, QUNS_PRESENTATION_MODE,
        QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    let mut state = 0;
    // SAFETY: writes the state into a valid integer.
    let hr = unsafe { SHQueryUserNotificationState(&mut state) };
    hr >= 0
        && matches!(
            state,
            QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE
        )
}

/// Window classes of the desktop and the taskbar: they are never "a program on the whole screen".
const SHELL_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

/// True when the window in front is another program that is maximized or
/// covers the whole monitor the island is on. `own` are our window handles.
pub fn front_app_fills_screen(island: isize, own: &[isize]) -> bool {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowRect, IsIconic, IsWindowVisible, IsZoomed,
    };
    // SAFETY: plain queries on window handles; a stale handle gives zeros and false.
    unsafe {
        let front = GetForegroundWindow();
        if front.is_null() || own.contains(&(front as isize)) {
            return false;
        }
        if IsWindowVisible(front) == 0 || IsIconic(front) != 0 {
            return false;
        }
        let mut class = [0u16; 64];
        let n = GetClassNameW(front, class.as_mut_ptr(), class.len() as i32);
        let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
        if SHELL_CLASSES.contains(&class.as_str()) {
            return false;
        }
        let its_monitor = MonitorFromWindow(front, MONITOR_DEFAULTTONEAREST);
        let our_monitor = MonitorFromWindow(island as _, MONITOR_DEFAULTTONEAREST);
        if its_monitor != our_monitor {
            return false;
        }
        if IsZoomed(front) != 0 {
            return true;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..std::mem::zeroed()
        };
        let mut r: RECT = std::mem::zeroed();
        if GetMonitorInfoW(its_monitor, &mut info) == 0 || GetWindowRect(front, &mut r) == 0 {
            return false;
        }
        let m = info.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

/// When the user last called the assistant with a shortcut or by voice.
static SUMMONED: Mutex<Option<std::time::Instant>> = parking_lot::const_mutex(None);
/// A call keeps her in front at least this long.
const SUMMON_HOLD: Duration = Duration::from_secs(10);

/// Puts the island and the avatar above everything, full-screen programs included.
fn bring_front(app: &tauri::AppHandle) {
    use tauri::Manager;
    for label in ["island", AVATAR_LABEL] {
        let Some(w) = app.get_webview_window(label) else {
            continue;
        };
        // An ordinary window is not kept on top; it comes forward with the focus.
        if label == "island" && WINDOWED.load(Ordering::Relaxed) {
            let _ = w.unminimize();
            continue;
        }
        let _ = w.set_always_on_top(true);
        if w.is_visible().unwrap_or(false) {
            let _ = show_without_activation(&w);
            // An avatar model is drawn again.
            crate::avatar_model::window_shown(app, label, true);
        }
    }
}

/// Lets a full-screen program cover the island and the avatar: they stay open
/// but go to the bottom of the window order.
fn send_back(app: &tauri::AppHandle) {
    use tauri::Manager;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_BOTTOM, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    for label in ["island", AVATAR_LABEL] {
        let Some(w) = app.get_webview_window(label) else {
            continue;
        };
        if label == "island" && WINDOWED.load(Ordering::Relaxed) {
            continue;
        }
        let _ = w.set_always_on_top(false);
        if let Ok(hwnd) = w.hwnd() {
            // SAFETY: plain call on our own window handle.
            unsafe {
                SetWindowPos(
                    hwnd.0 as windows_sys::Win32::Foundation::HWND,
                    HWND_BOTTOM,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
        // Behind a full-screen program nobody sees her: an avatar model is not drawn.
        crate::avatar_model::window_shown(app, label, false);
    }
}

/// The user called the assistant with a shortcut or by voice: she comes to the
/// front at once, even over a full-screen program.
pub fn summon(app: &tauri::AppHandle) {
    *SUMMONED.lock() = Some(std::time::Instant::now());
    bring_front(app);
}

/// Whether she should be in front. `held` is the answer of the previous check:
/// once called, she stays in front while she is busy or her window has the focus.
pub fn stays_in_front(
    fullscreen: bool,
    summoned_recently: bool,
    held: bool,
    busy: bool,
    focused: bool,
    asking: bool,
) -> (bool, bool) {
    let held = summoned_recently || (held && (busy || focused));
    // A question about an action must be seen, called or not.
    (!fullscreen || held || asking, held)
}

/// While a full-screen program is in front, the island and the desktop avatar go
/// behind it; a call by shortcut or voice brings them back on top. `enabled`,
/// `busy` and `asking` are read on every check.
pub fn spawn_game_watch(
    app: tauri::AppHandle,
    enabled: impl Fn() -> bool + Send + 'static,
    busy: impl Fn() -> bool + Send + 'static,
    asking: impl Fn() -> bool + Send + 'static,
) {
    use tauri::Manager;
    std::thread::Builder::new()
        .name("fullscreen-watch".into())
        .spawn(move || {
            let mut in_front = true;
            let mut held = false;
            loop {
                std::thread::sleep(Duration::from_millis(700));
                let handle = |label: &str| {
                    app.get_webview_window(label)
                        .and_then(|w| w.hwnd().ok())
                        .map(|h| h.0 as isize)
                };
                let island = handle("island").unwrap_or(0);
                let own: Vec<isize> = ["island", AVATAR_LABEL, TARGET_LABEL]
                    .iter()
                    .filter_map(|l| handle(l))
                    .collect();
                let fullscreen =
                    enabled() && (fullscreen_app_running() || front_app_fills_screen(island, &own));
                let summoned = SUMMONED.lock().is_some_and(|at| at.elapsed() < SUMMON_HOLD);
                let focused = app
                    .get_webview_window("island")
                    .is_some_and(|w| w.is_focused().unwrap_or(false));
                let (front, still_held) =
                    stays_in_front(fullscreen, summoned, held, busy(), focused, asking());
                held = still_held;
                if front != in_front {
                    if front {
                        bring_front(&app);
                    } else {
                        send_back(&app);
                    }
                    in_front = front;
                }
            }
        })
        .expect("spawn fullscreen watch thread");
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
    // Runs once each time the user lets go of a dragged window.
    on_drop: impl Fn() + Send + 'static,
) {
    let placement = Arc::new(Mutex::new(Placement { origin, scale }));

    let cache = placement.clone();
    let events_window = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
            match event {
                WindowEvent::Moved(_) => note_moved(),
                _ => note_resized(),
            }
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
                if drag_ended() {
                    on_drop();
                }
                if WINDOWED.load(Ordering::Relaxed) {
                    // An ordinary window takes every click; start over when it is the island again.
                    inside_prev = None;
                    std::thread::sleep(Duration::from_millis(40));
                    continue;
                }
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
                    let inside = island_hit(cursor, p.origin, p.scale, *rect.lock())
                        && !passes_through(cursor, Instant::now(), *PASSING.lock());
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

/// Shows the window and hands the keyboard focus back to the window that had it.
///
/// Must go through `WebviewWindow::show`: tao keeps its own visibility flag, and
/// every later style change (each click-through toggle) runs `ShowWindow(SW_HIDE)`
/// when that flag says hidden. A raw `ShowWindow` left the flag false, so the
/// island vanished and half-ignored clicks.
pub fn show_without_activation(window: &WebviewWindow) -> tauri::Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};
    // SAFETY: plain query of the current foreground window.
    let previous = unsafe { GetForegroundWindow() };
    window.show()?;
    let own = window.hwnd()?.0 as windows_sys::Win32::Foundation::HWND;
    if !previous.is_null() && previous != own {
        // SAFETY: `previous` was a valid window handle a moment ago; a stale one makes the call fail.
        unsafe {
            SetForegroundWindow(previous);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_monitors() -> Vec<MonitorBox> {
        vec![
            MonitorBox {
                name: None,
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
                scale: 1.0,
            },
            MonitorBox {
                name: Some("DISPLAY2".into()),
                x: 1920,
                y: 0,
                w: 2560,
                h: 1440,
                scale: 1.5,
            },
        ]
    }

    #[test]
    fn island_dropped_near_the_top_stays_an_island_on_that_monitor() {
        let m = two_monitors();
        assert_eq!(
            decide_drop(false, (900, 40), 60, &m, WINDOW_DEFAULT),
            DropAction::Island { monitor: None }
        );
        // Carried to the second monitor: 110 logical px is 165 physical there.
        assert_eq!(
            decide_drop(false, (3000, 150), 150, &m, WINDOW_DEFAULT),
            DropAction::Island {
                monitor: Some("DISPLAY2".into())
            }
        );
    }

    #[test]
    fn island_dropped_lower_becomes_a_window_under_the_cursor() {
        let m = two_monitors();
        assert_eq!(
            decide_drop(false, (960, 500), 480, &m, (920.0, 640.0)),
            DropAction::Window {
                rect: [500, 440, 920, 640]
            }
        );
        // Near the right bottom corner the window is pushed back onto the monitor.
        assert_eq!(
            decide_drop(false, (1900, 1000), 980, &m, (920.0, 640.0)),
            DropAction::Window {
                rect: [1000, 440, 920, 640]
            }
        );
        // On the second monitor the size follows its scale.
        assert_eq!(
            decide_drop(false, (3200, 700), 680, &m, (920.0, 640.0)),
            DropAction::Window {
                rect: [2510, 480, 1380, 960]
            }
        );
    }

    #[test]
    fn window_becomes_the_island_only_at_the_very_top_edge() {
        let m = two_monitors();
        assert_eq!(
            decide_drop(true, (700, 5), 0, &m, WINDOW_DEFAULT),
            DropAction::Island { monitor: None }
        );
        assert_eq!(
            decide_drop(true, (700, 60), 0, &m, WINDOW_DEFAULT),
            DropAction::Stay
        );
        assert_eq!(
            decide_drop(true, (2500, 10), 0, &m, WINDOW_DEFAULT),
            DropAction::Island {
                monitor: Some("DISPLAY2".into())
            }
        );
        // The cursor left every monitor: the primary one decides.
        assert_eq!(
            decide_drop(true, (-50, 5000), 0, &m, WINDOW_DEFAULT),
            DropAction::Stay
        );
        assert_eq!(
            decide_drop(true, (5, 5), 0, &[], WINDOW_DEFAULT),
            DropAction::Stay
        );
    }

    #[test]
    fn full_screen_program_sends_her_back_until_she_is_called() {
        // No full-screen program: always in front.
        assert_eq!(
            stays_in_front(false, false, false, false, false, false),
            (true, false)
        );
        // A game is in front and nobody called her.
        assert_eq!(
            stays_in_front(true, false, false, false, false, false),
            (false, false)
        );
        // Called by shortcut: in front, and held.
        assert_eq!(
            stays_in_front(true, true, false, false, false, false),
            (true, true)
        );
        // The call is over but she still answers, or her window has the focus.
        assert_eq!(
            stays_in_front(true, false, true, true, false, false),
            (true, true)
        );
        assert_eq!(
            stays_in_front(true, false, true, false, true, false),
            (true, true)
        );
        // Done and the focus is back in the game: behind again.
        assert_eq!(
            stays_in_front(true, false, true, false, false, false),
            (false, false)
        );
        // A reply she started herself does not pull her over the game...
        assert_eq!(
            stays_in_front(true, false, false, true, false, false),
            (false, false)
        );
        // ...but a question about an action does.
        assert_eq!(
            stays_in_front(true, false, false, false, false, true),
            (true, false)
        );
    }

    #[test]
    fn saved_window_place_must_be_on_a_monitor() {
        let one = [(0, 0, 1920, 1080)];
        assert!(rect_is_reachable([100, 100, 920, 640], &one));
        // Left on a monitor that is gone.
        assert!(!rect_is_reachable([2500, 100, 920, 640], &one));
        assert!(rect_is_reachable(
            [2500, 100, 920, 640],
            &[(0, 0, 1920, 1080), (1920, 0, 1920, 1080)]
        ));
        // Title bar above the screen or below it: nothing to grab.
        assert!(!rect_is_reachable([100, -300, 920, 640], &one));
        assert!(!rect_is_reachable([100, 1070, 920, 640], &one));
        // Only a sliver sticks in from the side.
        assert!(!rect_is_reachable([-900, 100, 920, 640], &one));
        assert!(!rect_is_reachable([100, 100, 50, 40], &one));
    }

    #[test]
    fn centres_on_full_hd() {
        assert_eq!(canvas_position(0, 0, 1920, 1.0), (460, 0));
    }

    #[test]
    fn respects_scale_and_monitor_origin() {
        assert_eq!(canvas_position(1920, 0, 2560, 1.5), (2450, 0));
    }

    #[test]
    fn avatar_window_sits_in_bottom_right_corner() {
        assert_eq!(avatar_position(0, 0, 1920, 1080, 1.0), (1336, 546));
        assert_eq!(avatar_position(0, 0, 2560, 1440, 1.25), (1830, 772));
        // A monitor smaller than the window: stay on it.
        assert_eq!(avatar_position(100, 50, 400, 300, 1.0), (100, 50));
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

    #[test]
    fn mikas_click_goes_through_the_island_at_its_spot_only() {
        let now = Instant::now();
        // She clicks a 24×24 spot under the island.
        let passing = Some((now + PASS_HOLD, [948, 24, 24, 24]));
        assert!(passes_through((960, 36), now, passing));
        assert!(passes_through((940, 24), now, passing));
        // The user's click elsewhere on the island is the island's.
        assert!(!passes_through((900, 36), now, passing));
        assert!(!passes_through((960, 57), now, passing));
        // Not for ever, and not before she shows a spot.
        assert!(!passes_through((960, 36), now + PASS_HOLD, passing));
        assert!(!passes_through((960, 36), now, None));
    }
}
