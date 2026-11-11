//! Helpers shared by tools.

use std::path::PathBuf;

use mika_core::tools::ToolError;
use serde_json::{json, Value};

/// JSON Schema for an object with string/number/bool properties.
pub fn schema(props: &[(&str, &str, &str)], required: &[&str]) -> Value {
    let mut p = serde_json::Map::new();
    for (name, ty, desc) in props {
        let mut v = json!({ "type": ty, "description": desc });
        if let Some(values) = desc.strip_prefix("one of: ") {
            let options: Vec<&str> = values.split(", ").collect();
            v = json!({ "type": ty, "enum": options });
        }
        p.insert((*name).into(), v);
    }
    json!({ "type": "object", "properties": p, "required": required })
}

pub fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn arg_bool(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

pub fn failed(msg: impl Into<String>) -> ToolError {
    ToolError::Failed(msg.into())
}

/// Expands `%VAR%` and a leading `~`. Relative paths start in the user profile.
pub fn resolve_path(raw: &str, user_profile: &str) -> PathBuf {
    let mut s = raw.trim().trim_matches('"').to_string();
    while let Some(start) = s.find('%') {
        let Some(len) = s[start + 1..].find('%') else {
            break;
        };
        let name = &s[start + 1..start + 1 + len];
        let value = std::env::var(name).unwrap_or_default();
        if value.is_empty() {
            break;
        }
        s.replace_range(start..start + len + 2, &value);
    }
    if let Some(rest) = s.strip_prefix('~') {
        s = format!("{user_profile}{rest}");
    }
    let p = PathBuf::from(&s);
    let p = if p.is_absolute() || s.starts_with("\\\\") {
        p
    } else {
        PathBuf::from(user_profile).join(p)
    };
    PathBuf::from(mika_core::policy::paths::normalize_path(
        &p.to_string_lossy(),
    ))
}

pub fn truncate(text: &str, max: usize) -> String {
    mika_core::agent::truncate(text, max)
}

/// Percent-encoding for URL query values.
pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Host part of an http(s) URL, lower case.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?.split('@').next_back()?;
    let host = host.split(':').next()?.to_lowercase();
    (!host.is_empty()).then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_handles_relative_tilde_and_dotdot() {
        assert_eq!(
            resolve_path("Documents\\a.txt", "C:\\Users\\me"),
            PathBuf::from("C:\\Users\\me\\Documents\\a.txt")
        );
        assert_eq!(
            resolve_path("~/Desktop", "C:\\Users\\me"),
            PathBuf::from("C:\\Users\\me\\Desktop")
        );
        assert_eq!(
            resolve_path("C:\\Users\\me\\..\\..\\Windows", "C:\\Users\\me"),
            PathBuf::from("C:\\Windows")
        );
    }

    #[test]
    fn url_helpers() {
        assert_eq!(
            url_encode("погода Москва"),
            "%D0%BF%D0%BE%D0%B3%D0%BE%D0%B4%D0%B0+%D0%9C%D0%BE%D1%81%D0%BA%D0%B2%D0%B0"
        );
        assert_eq!(
            url_decode("https%3A%2F%2Fya.ru%2F%3Fq%3D1"),
            "https://ya.ru/?q=1"
        );
        assert_eq!(url_host("https://User@Ya.ru:443/x?y"), Some("ya.ru".into()));
        assert_eq!(url_host("ftp://x"), None);
    }
}
