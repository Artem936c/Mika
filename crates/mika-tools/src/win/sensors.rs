//! What the watcher of events sees on a real computer (decision 0031).
//! Everything here looks at the user's own Windows session only: on a terminal
//! server other people's processes and sessions stay out of sight.

use parking_lot::Mutex;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use windows::core::{Interface, GUID, PWSTR};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eAll, eRender, IMMDeviceEnumerator, IMMEndpoint, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSActive, WTSClientProtocolType, WTSFreeMemory,
    WTSQuerySessionInformationW, WTSSessionInfoEx, WTSINFOEXW, WTS_CURRENT_SESSION, WTS_INFO_CLASS,
    WTS_SESSIONSTATE_LOCK,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

use super::uia::UiaUi;
use super::with_com;
use crate::observe::{AudioDevice, OwnInput, Proc, Sensors, SessionState};
use crate::sys::Ui;

/// What she sent herself, for the whole process: `SendInputDevice` reports
/// here and `idle_seconds` asks.
static OWN_INPUT: Mutex<OwnInput> = Mutex::new(OwnInput::new());

/// The session's last input and the tick count now; `None` when Windows does
/// not say.
fn last_input() -> Option<(u32, u32)> {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: fills a plain struct owned here.
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return None;
    }
    // SAFETY: no arguments.
    Some((info.dwTime, unsafe { GetTickCount() }))
}

/// Held while she sends keys or clicks: what is sent meanwhile is not taken
/// for the user's input by the watcher of events (decision 0031).
pub(super) struct OwnInputGuard;

/// Call right before sending input and keep the result until it is sent.
pub(super) fn own_input() -> OwnInputGuard {
    if let Some((last, _)) = last_input() {
        OWN_INPUT.lock().begin(last);
    }
    OwnInputGuard
}

impl Drop for OwnInputGuard {
    fn drop(&mut self) {
        // SAFETY: no arguments.
        OWN_INPUT.lock().end(unsafe { GetTickCount() });
    }
}

/// `PKEY_Device_FriendlyName`: the name of a sound device as the mixer shows it.
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

/// `WTSClientProtocolType` of a session shown over RDP.
const PROTOCOL_RDP: u16 = 2;

pub struct WinSensors {
    /// Her own UI Automation: `ToolEnv.ui` keeps the element numbers of the
    /// model's last `ui_read`, and a background read must not move them.
    ui: UiaUi,
    /// Kept between looks: a refresh then only notes what changed.
    system: Mutex<System>,
    /// The Windows session this program runs in.
    session_id: Option<u32>,
}

impl WinSensors {
    pub fn new() -> Self {
        let mut id = 0u32;
        // SAFETY: writes one u32 owned here.
        let found = unsafe { ProcessIdToSessionId(std::process::id(), &mut id) }.is_ok();
        Self {
            ui: UiaUi::new(),
            system: Mutex::new(System::new()),
            session_id: found.then_some(id),
        }
    }
}

fn err(e: windows::core::Error) -> String {
    format!("ошибка Windows: {}", e.message())
}

/// One answer of `WTSQuerySessionInformationW` about the own session, read as
/// `T`. `None` when Windows gave less than a `T`.
///
/// # Safety
/// `class` must be an information class whose answer starts with a `T`.
unsafe fn session_info<T: Copy>(class: WTS_INFO_CLASS) -> Result<Option<T>, String> {
    let mut buffer = PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: both out pointers are owned here; the buffer Windows allocates
    // is read within its size and freed below.
    unsafe {
        WTSQuerySessionInformationW(None, WTS_CURRENT_SESSION, class, &mut buffer, &mut bytes)
            .map_err(err)?;
        let whole = !buffer.is_null() && bytes as usize >= std::mem::size_of::<T>();
        let value = whole.then(|| std::ptr::read_unaligned(buffer.0 as *const T));
        WTSFreeMemory(buffer.0 as *mut core::ffi::c_void);
        Ok(value)
    }
}

impl Sensors for WinSensors {
    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String> {
        with_com(COINIT_MULTITHREADED, || {
            // SAFETY: COM is initialised by `with_com` on this thread; every
            // interface is used and released on it.
            unsafe {
                let en: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(err)?;
                let list = en
                    .EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE)
                    .map_err(err)?;
                let mut out = Vec::new();
                for i in 0..list.GetCount().map_err(err)? {
                    let Ok(device) = list.Item(i) else { continue };
                    let Ok(raw) = device.GetId() else { continue };
                    let id = raw.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
                    // A device whose name cannot be read is still a device.
                    let name = device
                        .OpenPropertyStore(STGM_READ)
                        .and_then(|store| store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME))
                        .map(|value| value.to_string())
                        .unwrap_or_default();
                    let output = device
                        .cast::<IMMEndpoint>()
                        .and_then(|e| e.GetDataFlow())
                        .map_or(true, |flow| flow == eRender);
                    out.push(AudioDevice { id, name, output });
                }
                Ok(out)
            }
        })
    }

    fn processes(&self) -> Result<Vec<Proc>, String> {
        let own = self
            .session_id
            .ok_or("не удалось узнать свой сеанс Windows")?;
        let mut system = self.system.lock();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        Ok(system
            .processes()
            .iter()
            .filter(|(_, p)| p.session_id().map(|s| s.as_u32()) == Some(own))
            .map(|(pid, p)| Proc {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().to_lowercase(),
                started: p.start_time(),
            })
            .collect())
    }

    fn session(&self) -> Result<SessionState, String> {
        // SAFETY: WTSSessionInfoEx answers with a WTSINFOEXW and
        // WTSClientProtocolType with a USHORT.
        let (info, protocol) = unsafe {
            (
                session_info::<WTSINFOEXW>(WTSSessionInfoEx)?,
                session_info::<u16>(WTSClientProtocolType)?,
            )
        };
        let info = info
            .filter(|i| i.Level == 1)
            .ok_or("Windows не сообщила состояние сеанса")?;
        // SAFETY: level 1 means the union holds WTSINFOEX_LEVEL1_W.
        let level = unsafe { info.Data.WTSInfoExLevel1 };
        Ok(SessionState {
            connected: level.SessionState == WTSActive,
            remote: protocol == Some(PROTOCOL_RDP),
            locked: level.SessionFlags as u32 == WTS_SESSIONSTATE_LOCK,
        })
    }

    fn idle_seconds(&self) -> u64 {
        let Some((last, now)) = last_input() else {
            return 0;
        };
        // Her own keys and clicks moved the session's last input; the user's is asked for.
        let user = OWN_INPUT.lock().user_input(last);
        u64::from(now.wrapping_sub(user) / 1000)
    }

    fn window_text(&self, window: u64) -> Result<String, String> {
        let nodes = self.ui.tree(window)?;
        let lines: Vec<String> = nodes
            .into_iter()
            .flat_map(|n| [n.name, n.value])
            .filter(|part| !part.trim().is_empty())
            .collect();
        Ok(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prints what the sensors see. The names of sound devices are printed:
    /// run it by hand only.
    #[test]
    #[ignore = "reads the real sound devices, processes and session"]
    fn live_sensors() {
        let s = WinSensors::new();
        match s.audio_devices() {
            Ok(list) => {
                println!("sound devices: {}", list.len());
                for d in list {
                    let kind = if d.output { "output" } else { "input" };
                    println!("  {kind}: {}", d.name);
                }
            }
            Err(e) => println!("sound devices: {e}"),
        }
        let asked = std::time::Instant::now();
        let first = s.processes().map(|p| p.len());
        let first_ms = asked.elapsed().as_millis();
        let asked = std::time::Instant::now();
        let second = s.processes().map(|p| p.len());
        println!(
            "processes of the own session: {first:?} in {first_ms} ms, again {second:?} in {} ms",
            asked.elapsed().as_millis()
        );
        println!("session: {:?}", s.session());
        println!("idle: {} s", s.idle_seconds());
    }
}
