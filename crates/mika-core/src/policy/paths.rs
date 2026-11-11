//! Path classifier. Pure string logic; the caller expands variables and
//! detects removable drives.

use serde::{Deserialize, Serialize};

use crate::tools::RiskLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathClass {
    System,
    ProgramFiles,
    UserProfile,
    Network,
    Removable,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathAccess {
    Read,
    Write,
    Delete,
}

/// Backslashes, no `.` or `..` parts, no trailing separator. `..` never climbs above the root.
pub fn normalize_path(path: &str) -> String {
    let p = path.trim().trim_matches('"').replace('/', "\\");
    let (prefix, rest) = if let Some(r) = p.strip_prefix("\\\\") {
        ("\\\\".to_string(), r.to_string())
    } else if p.len() >= 2 && p.as_bytes()[1] == b':' {
        (p[..2].to_uppercase() + "\\", p[2..].to_string())
    } else {
        (String::new(), p.clone())
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            x => parts.push(x),
        }
    }
    let joined = parts.join("\\");
    if prefix.is_empty() {
        joined
    } else {
        format!("{prefix}{joined}")
    }
}

fn under(path: &str, dir: &str) -> bool {
    let p = path.to_lowercase();
    let d = dir.to_lowercase();
    p == d || p.starts_with(&format!("{}\\", d.trim_end_matches('\\')))
}

pub fn classify_path(path: &str, user_profile: &str, removable: bool) -> PathClass {
    let p = normalize_path(path);
    if p.starts_with("\\\\") {
        return PathClass::Network;
    }
    if removable {
        return PathClass::Removable;
    }
    // A bare drive root is treated as a system location.
    if p.len() <= 3 && p.ends_with(":\\") {
        return PathClass::System;
    }
    for sys in [
        "C:\\Windows",
        "C:\\ProgramData",
        "C:\\$Recycle.Bin",
        "C:\\System Volume Information",
        "C:\\Recovery",
        "C:\\Boot",
    ] {
        if under(&p, sys) {
            return PathClass::System;
        }
    }
    for pf in ["C:\\Program Files", "C:\\Program Files (x86)"] {
        if under(&p, pf) {
            return PathClass::ProgramFiles;
        }
    }
    let profile = normalize_path(user_profile);
    if !profile.is_empty() && under(&p, &profile) {
        // AppData holds program settings; changes there behave like system changes.
        if under(&p, &format!("{profile}\\AppData")) {
            return PathClass::ProgramFiles;
        }
        return PathClass::UserProfile;
    }
    PathClass::Other
}

pub fn path_risk(class: PathClass, access: PathAccess) -> RiskLevel {
    match (access, class) {
        (PathAccess::Delete, _) => RiskLevel::High,
        (PathAccess::Read, PathClass::System) => RiskLevel::Medium,
        (PathAccess::Read, _) => RiskLevel::Low,
        (PathAccess::Write, PathClass::System | PathClass::ProgramFiles) => RiskLevel::High,
        (PathAccess::Write, _) => RiskLevel::Medium,
    }
}

pub fn class_label(class: PathClass) -> &'static str {
    match class {
        PathClass::System => "системная папка",
        PathClass::ProgramFiles => "папка программ",
        PathClass::UserProfile => "папка пользователя",
        PathClass::Network => "сетевая папка",
        PathClass::Removable => "съёмный диск",
        PathClass::Other => "другая папка",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = "C:\\Users\\me";

    #[test]
    fn dotdot_into_windows_is_system() {
        assert_eq!(
            classify_path("C:\\Users\\me\\..\\..\\Windows\\System32", PROFILE, false),
            PathClass::System
        );
        assert_eq!(
            normalize_path("C:\\Users\\me\\..\\..\\..\\..\\Windows"),
            "C:\\Windows"
        );
    }

    #[test]
    fn program_files_x86() {
        assert_eq!(
            classify_path("c:\\program files (x86)\\App\\a.exe", PROFILE, false),
            PathClass::ProgramFiles
        );
    }

    #[test]
    fn user_profile_documents() {
        assert_eq!(
            classify_path("C:\\Users\\me\\Documents\\a.txt", PROFILE, false),
            PathClass::UserProfile
        );
        assert_eq!(
            classify_path("C:\\Users\\meme\\a.txt", PROFILE, false),
            PathClass::Other
        );
        assert_eq!(
            classify_path("C:\\Users\\me\\AppData\\Roaming\\x", PROFILE, false),
            PathClass::ProgramFiles
        );
    }

    #[test]
    fn unc_is_network() {
        assert_eq!(
            classify_path("\\\\server\\share\\a", PROFILE, false),
            PathClass::Network
        );
    }

    #[test]
    fn drive_root_is_system() {
        assert_eq!(classify_path("D:\\", PROFILE, false), PathClass::System);
        assert_eq!(classify_path("d:", PROFILE, false), PathClass::System);
    }

    #[test]
    fn removable_flag_wins_for_local_paths() {
        assert_eq!(
            classify_path("E:\\photos", PROFILE, true),
            PathClass::Removable
        );
    }

    #[test]
    fn forward_slashes_normalized() {
        assert_eq!(
            normalize_path("c:/Windows/./System32/"),
            "C:\\Windows\\System32"
        );
        assert_eq!(
            classify_path("C:/Windows/notepad.exe", PROFILE, false),
            PathClass::System
        );
    }

    #[test]
    fn delete_is_always_high() {
        for c in [
            PathClass::UserProfile,
            PathClass::Other,
            PathClass::Removable,
        ] {
            assert_eq!(path_risk(c, PathAccess::Delete), RiskLevel::High);
        }
    }

    #[test]
    fn write_to_system_is_high() {
        assert_eq!(
            path_risk(PathClass::System, PathAccess::Write),
            RiskLevel::High
        );
        assert_eq!(
            path_risk(PathClass::UserProfile, PathAccess::Write),
            RiskLevel::Medium
        );
        assert_eq!(
            path_risk(PathClass::UserProfile, PathAccess::Read),
            RiskLevel::Low
        );
    }
}
