//! Interface texts for the Rust side. One file with the Svelte UI: `frontend/src/locales/ru.json`.

use std::collections::HashMap;
use std::sync::OnceLock;

const RU: &str = include_str!("../../../frontend/src/locales/ru.json");

fn table() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        serde_json::from_str(RU).unwrap_or_else(|e| {
            log::error!("locale file is broken: {e}");
            HashMap::new()
        })
    })
}

/// Text for `key`. An unknown key comes back as is, so a typo is visible on screen.
pub fn tr(key: &str) -> String {
    table().get(key).cloned().unwrap_or_else(|| key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tr_returns_tray_strings() {
        assert_eq!(tr("tray.open"), "Открыть чат");
        assert_eq!(tr("tray.quit"), "Выход");
        for key in [
            "tray.hide",
            "tray.show",
            "tray.shortcut_none",
            "error.name_empty",
            "error.name_long",
        ] {
            assert_ne!(tr(key), key, "{key} is missing in ru.json");
        }
    }

    #[test]
    fn tr_unknown_key_returns_key() {
        assert_eq!(tr("no.such.key"), "no.such.key");
    }
}
