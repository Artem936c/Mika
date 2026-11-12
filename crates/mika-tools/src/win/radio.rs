//! Wi-Fi and Bluetooth switches through `Windows.Devices.Radios`.

use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind as WinKind, RadioState};
use windows::Win32::System::Com::COINIT_MULTITHREADED;

use super::with_com;
use crate::sys::{RadioKind, Radios};

pub struct WinRadios;

fn find(kind: RadioKind) -> windows::core::Result<Option<Radio>> {
    let want = match kind {
        RadioKind::Wifi => WinKind::WiFi,
        RadioKind::Bluetooth => WinKind::Bluetooth,
    };
    for radio in Radio::GetRadiosAsync()?.join()? {
        if radio.Kind()? == want {
            return Ok(Some(radio));
        }
    }
    Ok(None)
}

fn message(e: windows::core::Error) -> String {
    format!("Windows не дал доступ к радиомодулям: {}", e.message())
}

impl Radios for WinRadios {
    fn get(&self, kind: RadioKind) -> Result<Option<bool>, String> {
        with_com(COINIT_MULTITHREADED, move || {
            let Some(radio) = find(kind).map_err(message)? else {
                return Ok(None);
            };
            Ok(Some(radio.State().map_err(message)? == RadioState::On))
        })
    }

    fn set(&self, kind: RadioKind, on: bool) -> Result<(), String> {
        with_com(COINIT_MULTITHREADED, move || {
            let radio = find(kind)
                .map_err(message)?
                .ok_or("на этом компьютере нет такого радиомодуля")?;
            let access = Radio::RequestAccessAsync()
                .and_then(|a| a.join())
                .map_err(message)?;
            if access != RadioAccessStatus::Allowed {
                return Err("Windows запретил приложению переключать радиомодули: проверь «Параметры → Конфиденциальность → Радиомодули»".into());
            }
            let state = if on { RadioState::On } else { RadioState::Off };
            let result = radio
                .SetStateAsync(state)
                .and_then(|a| a.join())
                .map_err(message)?;
            if result == RadioAccessStatus::Allowed {
                Ok(())
            } else {
                Err("Windows не переключил радиомодуль".into())
            }
        })
    }
}
