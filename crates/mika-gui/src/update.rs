//! Updates: a JSON manifest at an address the user gives, a download checked by
//! SHA-256, and the installer started only after the user's click.
//!
//! Manifest: `{"version": "0.3.0", "url": "https://…/Mika_0.3.0_x64-setup.exe",
//! "sha256": "…64 hex…", "notes": "что нового"}`.

use std::cmp::Ordering;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub notes: String,
}

/// Numbers of a version and its pre-release part: `0.2.0-pre.1` → ([0, 2, 0], Some("pre.1")).
fn parts(v: &str) -> (Vec<u64>, Option<String>) {
    let v = v.trim().trim_start_matches('v');
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (v, None),
    };
    let nums = core
        .split('.')
        .map(|n| n.parse::<u64>().unwrap_or(0))
        .collect();
    (nums, pre)
}

/// Compares versions the semver way: `0.2.0-pre.1` < `0.2.0-pre.2` < `0.2.0` < `0.10.0`.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (mut na, pa) = parts(a);
    let (mut nb, pb) = parts(b);
    let len = na.len().max(nb.len());
    na.resize(len, 0);
    nb.resize(len, 0);
    na.cmp(&nb).then_with(|| match (pa, pb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            // Dot-separated parts: numbers compare as numbers, words as text.
            let xs: Vec<&str> = x.split('.').collect();
            let ys: Vec<&str> = y.split('.').collect();
            for (a, b) in xs.iter().zip(&ys) {
                let o = match (a.parse::<u64>(), b.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    _ => a.cmp(b),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
            xs.len().cmp(&ys.len())
        }
    })
}

/// Reads and checks a manifest: an https address of an .exe and a full SHA-256.
pub fn parse_manifest(body: &str) -> Result<Manifest, String> {
    let m: Manifest =
        serde_json::from_str(body).map_err(|e| format!("непонятный файл обновлений: {e}"))?;
    if m.version.trim().is_empty() {
        return Err("в файле обновлений нет версии".into());
    }
    if !m.url.starts_with("https://") || !m.url.to_lowercase().ends_with(".exe") {
        return Err("в файле обновлений адрес установщика должен быть https://….exe".into());
    }
    let sha_ok = m.sha256.len() == 64 && m.sha256.chars().all(|c| c.is_ascii_hexdigit());
    if !sha_ok {
        return Err("в файле обновлений нет контрольной суммы SHA-256".into());
    }
    Ok(m)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())
}

/// The manifest when it offers a version newer than this one.
pub async fn check(manifest_url: &str) -> Result<Option<Manifest>, String> {
    let url = manifest_url.trim();
    if !url.starts_with("https://") {
        return Err("адрес файла обновлений должен начинаться с https://".into());
    }
    let resp = client()?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("нет связи с сервером обновлений: {}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(format!(
            "сервер обновлений ответил {}",
            resp.status().as_u16()
        ));
    }
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let m = parse_manifest(&body)?;
    Ok((compare(&m.version, env!("CARGO_PKG_VERSION")) == Ordering::Greater).then_some(m))
}

/// Downloads the installer and checks its SHA-256. Returns its path.
pub async fn download(m: &Manifest) -> Result<PathBuf, String> {
    let bytes = client()?
        .get(&m.url)
        .send()
        .await
        .map_err(|e| format!("установщик не скачался: {}", e.without_url()))?
        .error_for_status()
        .map_err(|e| format!("установщик не скачался: {}", e.without_url()))?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    if !sha256_hex(&bytes).eq_ignore_ascii_case(m.sha256.trim()) {
        return Err("контрольная сумма скачанного установщика не совпала: он не запущен".into());
    }
    let dir = std::env::temp_dir().join("Mika").join("update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("Mika_{}_setup.exe", m.version.trim()));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Starts the installer quietly once this process has ended, then starts the
/// app again. The exit may take a while (a screen recording is saved first):
/// the installer must not replace the program while it still runs.
pub fn run_installer(installer: &std::path::Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let then = format!(
        "\"{}\" /S & start \"\" \"{}\"",
        installer.display(),
        exe.display()
    );
    crate::platform::run_after_exit(&then).map_err(|e| format!("установщик не запустился: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_like_semver() {
        assert_eq!(compare("0.2.0-pre.1", "0.2.0-pre.2"), Ordering::Less);
        assert_eq!(compare("0.2.0-pre.2", "0.2.0"), Ordering::Less);
        assert_eq!(compare("0.2.0", "0.10.0"), Ordering::Less);
        assert_eq!(compare("v1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare("0.2.0-pre.10", "0.2.0-pre.9"), Ordering::Greater);
        assert_eq!(compare("0.3.0-pre.1", "0.2.9"), Ordering::Greater);
    }

    #[test]
    fn manifest_needs_https_exe_and_a_checksum() {
        let good = format!(
            r#"{{"version":"0.3.0","url":"https://example.com/Mika_0.3.0_x64-setup.exe","sha256":"{}","notes":"новое"}}"#,
            "a".repeat(64)
        );
        assert_eq!(parse_manifest(&good).unwrap().version, "0.3.0");
        for bad in [
            good.replace("https://", "http://"),
            good.replace(".exe", ".zip"),
            good.replace(&"a".repeat(64), "abc"),
            good.replace("0.3.0\"", "\""),
            "<html>".to_string(),
        ] {
            assert!(parse_manifest(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn checksum_is_lower_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
