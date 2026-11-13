use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, HDC, HMONITOR,
    MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindow, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW,
    SetForegroundWindow, SetWindowPos, ShowWindow, GW_OWNER, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CLOSE,
};

use crate::sys::{Placement, WindowInfo, Windows};

fn title_of(h: HWND) -> String {
    // SAFETY: plain queries on a window handle; a stale handle gives an empty title.
    unsafe {
        let len = GetWindowTextLengthW(h);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// Half of the work area of the monitor: (x, y, width, height).
pub fn half(work: (i32, i32, i32, i32), placement: Placement) -> (i32, i32, i32, i32) {
    let (x, y, w, h) = work;
    match placement {
        Placement::Left => (x, y, w / 2, h),
        Placement::Right => (x + w / 2, y, w - w / 2, h),
        Placement::Top => (x, y, w, h / 2),
        Placement::Bottom => (x, y + h / 2, w, h - h / 2),
        _ => work,
    }
}

pub struct WinWindows;

fn hwnd(id: u64) -> HWND {
    HWND(id as usize as *mut core::ffi::c_void)
}

unsafe extern "system" fn collect(h: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut Vec passed by `list` for the duration of EnumWindows.
    let out = &mut *(lparam.0 as *mut Vec<HWND>);
    out.push(h);
    BOOL(1)
}

/// Top-level windows, from front to back.
pub(super) fn top_level() -> Vec<HWND> {
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut handles as *mut _ as isize)) };
    handles
}

/// The process of a window; 0 for a closed one.
pub(super) fn process_id(h: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: plain query on a window handle; pid is a valid out pointer.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

fn process_name(h: HWND) -> String {
    let pid = process_id(h);
    if pid == 0 {
        return String::new();
    }
    // SAFETY: handle is closed below; buffer outlives the call.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().unwrap_or("").to_lowercase()
    }
}

impl Windows for WinWindows {
    fn list(&self) -> Vec<WindowInfo> {
        top_level()
            .into_iter()
            .filter_map(|h| {
                // SAFETY: plain queries on a window handle from EnumWindows.
                unsafe {
                    if !IsWindowVisible(h).as_bool() {
                        return None;
                    }
                    if GetWindow(h, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
                        return None;
                    }
                    let len = GetWindowTextLengthW(h);
                    if len == 0 {
                        return None;
                    }
                    let mut buf = vec![0u16; len as usize + 1];
                    let n = GetWindowTextW(h, &mut buf);
                    let title = String::from_utf16_lossy(&buf[..n as usize]);
                    Some(WindowInfo {
                        id: h.0 as usize as u64,
                        title,
                        process: process_name(h),
                    })
                }
            })
            .collect()
    }

    fn focus(&self, id: u64) -> Result<(), String> {
        let h = hwnd(id);
        // SAFETY: plain calls on a window handle; a stale handle just fails.
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            if SetForegroundWindow(h).as_bool() {
                Ok(())
            } else {
                Err("Windows не дал переключить окно".into())
            }
        }
    }

    fn close(&self, id: u64) -> Result<(), String> {
        // SAFETY: posting WM_CLOSE to a window handle; a stale handle just fails.
        unsafe { PostMessageW(Some(hwnd(id)), WM_CLOSE, WPARAM(0), LPARAM(0)) }
            .map_err(|e| format!("не удалось закрыть окно: {}", e.message()))
    }

    fn foreground(&self) -> Option<WindowInfo> {
        // SAFETY: plain query.
        let h = unsafe { GetForegroundWindow() };
        if h.is_invalid() {
            return None;
        }
        Some(WindowInfo {
            id: h.0 as usize as u64,
            title: title_of(h),
            process: process_name(h),
        })
    }

    fn place(&self, id: u64, placement: Placement) -> Result<(), String> {
        super::physical_pixels();
        let h = hwnd(id);
        // SAFETY: plain calls on a window handle; a stale handle just fails.
        unsafe {
            match placement {
                Placement::Maximize => {
                    let _ = ShowWindow(h, SW_MAXIMIZE);
                }
                Placement::Minimize => {
                    let _ = ShowWindow(h, SW_MINIMIZE);
                }
                Placement::Restore => {
                    let _ = ShowWindow(h, SW_RESTORE);
                }
                side => {
                    let monitor = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
                    let mut info = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        ..Default::default()
                    };
                    if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                        return Err("не удалось узнать размер экрана".into());
                    }
                    let RECT {
                        left,
                        top,
                        right,
                        bottom,
                    } = info.rcWork;
                    let (x, y, w, hgt) = half((left, top, right - left, bottom - top), side);
                    // A maximized window ignores SetWindowPos until restored.
                    let _ = ShowWindow(h, SW_RESTORE);
                    SetWindowPos(h, None, x, y, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE)
                        .map_err(|e| format!("не удалось переставить окно: {}", e.message()))?;
                }
            }
        }
        Ok(())
    }

    fn monitors(&self) -> usize {
        monitor_areas().len()
    }

    fn to_monitor(&self, id: u64, n: usize) -> Result<(), String> {
        super::physical_pixels();
        let areas = monitor_areas();
        let count = areas.len();
        let target = n
            .checked_sub(1)
            .and_then(|i| areas.get(i))
            .ok_or_else(|| format!("монитора {n} нет: их {count}"))?;
        let h = hwnd(id);
        // SAFETY: plain calls on a window handle; a stale handle just fails.
        unsafe {
            let monitor = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let from = if GetMonitorInfoW(monitor, &mut info).as_bool() {
                info.rcWork
            } else {
                RECT::default()
            };
            // A maximized window keeps its old monitor until restored.
            let _ = ShowWindow(h, SW_RESTORE);
            let mut r = RECT::default();
            GetWindowRect(h, &mut r).map_err(|_| "окно закрыто или недоступно".to_string())?;
            let to = rect_tuple(*target);
            let placed = moved(rect_tuple(r), rect_tuple(from), to);
            let (x, y, w, hgt) = placed;
            SetWindowPos(h, None, x, y, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE)
                .map_err(|e| format!("не удалось перенести окно: {}", e.message()))?;
            // On a monitor with another scale the window resizes itself
            // (WM_DPICHANGED), maybe after SetWindowPos returns: fit it again.
            let to_monitor = MonitorFromRect(target, MONITOR_DEFAULTTONEAREST);
            let rescales = match (monitor_dpi(monitor), monitor_dpi(to_monitor)) {
                (Some(a), Some(b)) => a != b,
                _ => true,
            };
            if rescales {
                for _ in 0..6 {
                    let mut now = RECT::default();
                    if GetWindowRect(h, &mut now).is_err() {
                        break;
                    }
                    if let Some((x, y, w, hgt)) = refit(rect_tuple(now), to) {
                        SetWindowPos(h, None, x, y, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE)
                            .map_err(|e| format!("не удалось перенести окно: {}", e.message()))?;
                        break;
                    }
                    if rect_tuple(now) != placed {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        Ok(())
    }
}

fn rect_tuple(r: RECT) -> (i32, i32, i32, i32) {
    (r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// Effective scale of a monitor in DPI; `None` when Windows does not say.
fn monitor_dpi(m: HMONITOR) -> Option<u32> {
    let (mut x, mut y) = (0u32, 0u32);
    // SAFETY: plain query; the out pointers live through the call.
    unsafe { GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.ok()?;
    Some(x)
}

/// A new box when the window no longer fits the work area `to`: shrunk and
/// pulled in, its offset kept. `None` when it fits.
pub fn refit(
    window: (i32, i32, i32, i32),
    to: (i32, i32, i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let fits = window.0 >= to.0
        && window.1 >= to.1
        && window.0 + window.2 <= to.0 + to.2
        && window.1 + window.3 <= to.1 + to.3;
    (!fits).then(|| moved(window, to, to))
}

/// The window's box on another monitor: the same offset from the corner of
/// the work area, shrunk and pulled in to fit.
pub fn moved(
    window: (i32, i32, i32, i32),
    from: (i32, i32, i32, i32),
    to: (i32, i32, i32, i32),
) -> (i32, i32, i32, i32) {
    let w = window.2.min(to.2);
    let h = window.3.min(to.3);
    let x = (to.0 + (window.0 - from.0).max(0)).min(to.0 + to.2 - w);
    let y = (to.1 + (window.1 - from.1).max(0)).min(to.1 + to.3 - h);
    (x, y, w, h)
}

unsafe extern "system" fn collect_monitor(
    m: HMONITOR,
    _dc: HDC,
    _r: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: lparam is the &mut Vec passed by `monitor_areas` for the duration of the call.
    let out = &mut *(lparam.0 as *mut Vec<(bool, RECT)>);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(m, &mut info).as_bool() {
        out.push((info.dwFlags & MONITORINFOF_PRIMARY != 0, info.rcWork));
    }
    BOOL(1)
}

/// Work areas of the monitors: the main one first, the rest from left to right.
fn monitor_areas() -> Vec<RECT> {
    let mut found: Vec<(bool, RECT)> = Vec::new();
    // SAFETY: the callback only pushes into `found`, which outlives the call.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut found as *mut _ as isize),
        )
    };
    found.sort_by_key(|(primary, r)| (!primary, r.left, r.top));
    found.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_keeps_its_offset_and_fits_the_other_monitor() {
        let left = (0, 0, 1920, 1040);
        let right = (1920, 0, 2560, 1400);
        assert_eq!(
            moved((100, 50, 800, 600), left, right),
            (2020, 50, 800, 600)
        );
        // Too big for a small monitor: shrunk and pulled in.
        assert_eq!(
            moved((2000, 100, 2400, 1300), right, left),
            (0, 0, 1920, 1040)
        );
        assert_eq!(
            moved((1800, 10, 400, 300), left, right),
            (3720, 10, 400, 300)
        );
    }

    #[test]
    fn a_window_grown_by_the_new_scale_is_fitted_again() {
        let right = (1920, 0, 2560, 1400);
        // 1800×1000 placed at 100 % grows to 2700×1500 at 150 %.
        assert_eq!(
            refit((2020, 50, 2700, 1500), right),
            Some((1920, 0, 2560, 1400))
        );
        assert_eq!(refit((2020, 50, 1200, 700), right), None);
        assert_eq!(
            refit((4000, 900, 600, 600), right),
            Some((3880, 800, 600, 600))
        );
    }
}
