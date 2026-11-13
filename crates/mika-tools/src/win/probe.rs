use sysinfo::{Disks, ProcessesToUpdate, System};

use crate::sys::{ProcessInfo, SystemFacts, SystemProbe};

pub struct SysinfoProbe;

const MB: u64 = 1024 * 1024;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

impl SystemProbe for SysinfoProbe {
    fn facts(&self) -> SystemFacts {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        let disks = Disks::new_with_refreshed_list()
            .list()
            .iter()
            .map(|d| {
                (
                    d.mount_point().display().to_string(),
                    d.available_space() as f64 / GB,
                    d.total_space() as f64 / GB,
                )
            })
            .collect();
        SystemFacts {
            cpu_percent: sys.global_cpu_usage(),
            memory_used_mb: sys.used_memory() / MB,
            memory_total_mb: sys.total_memory() / MB,
            uptime_s: System::uptime(),
            disks,
            os: System::long_os_version().unwrap_or_default(),
            host: System::host_name().unwrap_or_default(),
        }
    }

    fn processes(&self) -> Vec<ProcessInfo> {
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get() as f32)
            .unwrap_or(1.0);
        sys.processes()
            .iter()
            .map(|(pid, p)| ProcessInfo {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                memory_mb: p.memory() / MB,
                cpu_percent: p.cpu_usage() / cpus,
            })
            .collect()
    }

    fn battery(&self) -> Option<(u8, bool)> {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut s = SYSTEM_POWER_STATUS::default();
        // SAFETY: fills a plain struct owned here.
        unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
        // 128: no battery; 255: unknown.
        if s.BatteryFlag & 128 != 0 || s.BatteryLifePercent > 100 {
            return None;
        }
        Some((s.BatteryLifePercent, s.ACLineStatus == 1))
    }
}
