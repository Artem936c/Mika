use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW, SetForegroundWindow,
    SetWindowPos, ShowWindow, GW_OWNER, SWP_NOACTIVATE, SWP_NOZORDER, SW_MAXIMIZE, SW_MINIMIZE,
    SW_RESTORE, WM_CLOSE,
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

fn process_name(h: HWND) -> String {
    let mut pid = 0u32;
    // SAFETY: h comes from EnumWindows; pid is a valid out pointer.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
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
        let mut handles: Vec<HWND> = Vec::new();
        // SAFETY: the callback only pushes into `handles`, which outlives the call.
        let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut handles as *mut _ as isize)) };
        handles
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
}
