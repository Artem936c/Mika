use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL, COINIT_MULTITHREADED};

use super::with_com;
use crate::sys::Volume;

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
}
