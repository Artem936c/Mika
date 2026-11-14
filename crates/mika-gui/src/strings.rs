//! Interface texts for the Rust side: `mika_core::i18n` reads `frontend/src/locales/*.json`.

/// Text for `key` in the interface language. An unknown key comes back as is, so a typo is visible on screen.
pub fn tr(key: &str) -> String {
    mika_core::i18n::tr(key)
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
            "hotkeys.err_taken",
            "dictation.err_blind",
            "screen_ask.err_blind",
            "screen_ask.err_gone",
            "error.name_empty",
            "error.name_long",
            "tray.recording",
            "tray.rec_stop",
            "error.bgjob_gone",
        ] {
            assert_ne!(tr(key), key, "{key} is missing in ru.json");
        }
    }

    #[test]
    fn tr_unknown_key_returns_key() {
        assert_eq!(tr("no.such.key"), "no.such.key");
    }
}
