use windows::core::Interface;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL, COINIT_MULTITHREADED};

use super::with_com;
use crate::sys::{AppVolume, Volume};

pub struct CoreAudioVolume;

fn endpoint() -> Result<IAudioEndpointVolume, String> {
    // SAFETY: COM is initialised by `with_com` on this thread.
    unsafe {
        let en: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(err)?;
        let dev = en
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|_| "нет устройства вывода звука".to_string())?;
        dev.Activate(CLSCTX_ALL, None).map_err(err)
    }
}

fn err(e: windows::core::Error) -> String {
    format!("ошибка звука: {}", e.message())
}

impl Volume for CoreAudioVolume {
    fn get(&self) -> Result<(u8, bool), String> {
        with_com(COINIT_MULTITHREADED, || {
            let v = endpoint()?;
            // SAFETY: v is a live COM interface on this thread.
            unsafe {
                let level = v.GetMasterVolumeLevelScalar().map_err(err)?;
                let mute = v.GetMute().map_err(err)?.as_bool();
                Ok(((level * 100.0).round() as u8, mute))
            }
        })
    }

    fn set(&self, level: u8) -> Result<(), String> {
        with_com(COINIT_MULTITHREADED, move || {
            let v = endpoint()?;
            // SAFETY: v is a live COM interface on this thread.
            unsafe {
                v.SetMasterVolumeLevelScalar(level.min(100) as f32 / 100.0, std::ptr::null())
                    .map_err(err)
            }
        })
    }

    fn set_mute(&self, mute: bool) -> Result<(), String> {
        with_com(COINIT_MULTITHREADED, move || {
            let v = endpoint()?;
            // SAFETY: v is a live COM interface on this thread.
            unsafe { v.SetMute(mute, std::ptr::null()).map_err(err) }
        })
    }

    fn apps(&self) -> Result<Vec<AppVolume>, String> {
        with_com(COINIT_MULTITHREADED, || {
            let mut out: Vec<AppVolume> = Vec::new();
            for (pid, v) in sessions()? {
                // SAFETY: v is a live COM interface on this thread.
                let (level, muted) = unsafe {
                    (
                        v.GetMasterVolume().map_err(err)?,
                        v.GetMute().map_err(err)?.as_bool(),
                    )
                };
                // One program may have several sounds: the first one speaks for it.
                if out.iter().any(|a| a.pid == pid) {
                    continue;
                }
                out.push(AppVolume {
                    pid,
                    process: process_name(pid),
                    level: (level * 100.0).round() as u8,
                    muted,
                });
            }
            Ok(out)
        })
    }

    fn set_app(&self, pid: u32, level: Option<u8>, mute: Option<bool>) -> Result<(), String> {
        with_com(COINIT_MULTITHREADED, move || {
            let mut found = false;
            for (p, v) in sessions()? {
                if p != pid {
                    continue;
                }
                found = true;
                // SAFETY: v is a live COM interface on this thread.
                unsafe {
                    if let Some(l) = level {
                        v.SetMasterVolume(l.min(100) as f32 / 100.0, std::ptr::null())
                            .map_err(err)?;
                    }
                    if let Some(m) = mute {
                        v.SetMute(m, std::ptr::null()).map_err(err)?;
                    }
                }
            }
            if found {
                Ok(())
            } else {
                Err("у этой программы сейчас нет звука".into())
            }
        })
    }
}

/// Sounds of programs on the default output device, with their process ids.
/// The system sounds (process 0) are left out.
fn sessions() -> Result<Vec<(u32, ISimpleAudioVolume)>, String> {
    // SAFETY: COM is initialised by `with_com` on this thread.
    unsafe {
        let en: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(err)?;
        let dev = en
            .GetDefaultAudioEndpoint(eRender, eMultimedia)
            .map_err(|_| "нет устройства вывода звука".to_string())?;
        let manager: IAudioSessionManager2 = dev.Activate(CLSCTX_ALL, None).map_err(err)?;
        let list = manager.GetSessionEnumerator().map_err(err)?;
        let mut out = Vec::new();
        for i in 0..list.GetCount().map_err(err)? {
            let Ok(control) = list.GetSession(i) else {
                continue;
            };
            let Ok(control) = control.cast::<IAudioSessionControl2>() else {
                continue;
            };
            let pid = control.GetProcessId().unwrap_or(0);
            if pid == 0 {
                continue;
            }
            if let Ok(v) = control.cast::<ISimpleAudioVolume>() {
                out.push((pid, v));
            }
        }
        Ok(out)
    }
}

/// Executable name of a process, lower case, or an empty string.
fn process_name(pid: u32) -> String {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: the handle is closed below; the buffer outlives the call.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(h);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().unwrap_or("").to_lowercase()
    }
}
