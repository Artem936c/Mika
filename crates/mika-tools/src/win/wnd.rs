use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible, PostMessageW, SetForegroundWindow, ShowWindow, GW_OWNER, SW_RESTORE,
    WM_CLOSE,
};

use crate::sys::{WindowInfo, Windows};

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
}
