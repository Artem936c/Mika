use std::path::PathBuf;

use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

use crate::sys::RdpSource;

pub struct WinRdp {
    pub user_profile: String,
}

impl RdpSource for WinRdp {
    fn saved_hosts(&self) -> Vec<String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Terminal Server Client\\Servers")
            .map(|k| k.enum_keys().filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    fn rdp_files(&self) -> Vec<PathBuf> {
        let base = PathBuf::from(&self.user_profile);
        ["Documents", "Desktop"]
            .iter()
            .filter_map(|d| std::fs::read_dir(base.join(d)).ok())
            .flat_map(|rd| rd.filter_map(Result::ok))
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rdp")))
            .collect()
    }
}
