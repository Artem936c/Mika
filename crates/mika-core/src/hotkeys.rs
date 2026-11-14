//! Global hotkeys: the list of actions, how a combination is written and which
//! combinations may be bound. Pure: the registration lives in the GUI.

use std::collections::BTreeMap;
use std::fmt;

/// One action that a global hotkey starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyDef {
    pub id: &'static str,
    /// Tried in order while the user has not chosen a combination.
    pub defaults: &'static [&'static str],
    /// The handler is also told when the key goes up (hold to talk).
    pub on_release: bool,
}

/// Every action with a global hotkey. Another epic adds an action with one line
/// here and one match arm in `crates/mika-gui/src/hotkeys.rs` `fire`.
pub const HOTKEYS: &[HotkeyDef] = &[
    HotkeyDef {
        id: "chat",
        defaults: &["Ctrl+Alt+M", "Ctrl+Shift+M"],
        on_release: false,
    },
    HotkeyDef {
        id: "listen",
        defaults: &["Ctrl+Alt+K", "Ctrl+Shift+K"],
        on_release: false,
    },
    HotkeyDef {
        id: "selection",
        defaults: &["Ctrl+Alt+X", "Ctrl+Shift+X"],
        on_release: false,
    },
    HotkeyDef {
        id: "dictation",
        defaults: &["Ctrl+Alt+D", "Ctrl+Alt+Shift+D"],
        on_release: true,
    },
    HotkeyDef {
        id: "screen_ask",
        defaults: &["Ctrl+Alt+Q", "Ctrl+Alt+Shift+Q"],
        on_release: false,
    },
];

pub fn hotkey(id: &str) -> Option<&'static HotkeyDef> {
    HOTKEYS.iter().find(|def| def.id == id)
}

/// Why a combination cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComboError {
    #[error("the combination has no key, only modifiers")]
    NoKey,
    #[error("the key `{0}` cannot be a hotkey")]
    UnknownKey(String),
    #[error("the combination needs more modifier keys")]
    Modifier,
    #[error("the combination is a usual Windows command")]
    Reserved,
}

impl ComboError {
    /// The GUI shows the locale key `hotkeys.err_<code>`.
    pub fn code(&self) -> &'static str {
        match self {
            ComboError::NoKey => "no_key",
            ComboError::UnknownKey(_) => "unknown_key",
            ComboError::Modifier => "modifier",
            ComboError::Reserved => "reserved",
        }
    }
}

/// A key with its modifiers.
///
/// `key` is a canonical name: `A`..`Z`, `0`..`9`, `F1`..`F24`, `Space`, `Up`,
/// `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Pause`,
/// `ScrollLock`, `Minus`, `Equal`, `BracketLeft`, `BracketRight`, `Backslash`,
/// `Semicolon`, `Quote`, `Comma`, `Period`, `Slash`, `Backquote`,
/// `Numpad0`..`Numpad9`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: String,
}

/// Keys with a name of their own, besides letters, digits, `F…` and `Numpad…`.
const NAMED_KEYS: &[&str] = &[
    "Space",
    "Up",
    "Down",
    "Left",
    "Right",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Pause",
    "ScrollLock",
    "Minus",
    "Equal",
    "BracketLeft",
    "BracketRight",
    "Backslash",
    "Semicolon",
    "Quote",
    "Comma",
    "Period",
    "Slash",
    "Backquote",
];

/// The number of a function key name (`F1`..`F24`), if `key` is one.
/// With Ctrl+Shift alone these are taken in every program: the arrows and
/// their neighbours select text, `Z` redoes, `V` pastes plain text, `C` copies
/// in a terminal, `T` brings a closed tab back, `N` makes a folder or a window.
const EDITING_KEYS: &[&str] = &[
    "Left", "Right", "Up", "Down", "Home", "End", "PageUp", "PageDown", "Z", "V", "C", "T", "N",
];

fn function_number(key: &str) -> Option<u32> {
    let digits = key.strip_prefix('F')?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|n| (1..=24).contains(n))
}

/// The canonical name of a key token. Accepts the canonical name in any case
/// and the browser `KeyboardEvent.code` form (`KeyD`, `Digit7`, `ArrowUp`).
fn canonical_key(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    let name = match lower.as_str() {
        "arrowup" => "up",
        "arrowdown" => "down",
        "arrowleft" => "left",
        "arrowright" => "right",
        other => other,
    };
    let single = |text: &str| {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    };
    if let Some(c) = single(name).filter(char::is_ascii_alphanumeric) {
        return Some(c.to_ascii_uppercase().to_string());
    }
    if let Some(c) = name
        .strip_prefix("key")
        .and_then(single)
        .filter(char::is_ascii_alphabetic)
    {
        return Some(c.to_ascii_uppercase().to_string());
    }
    if let Some(c) = name
        .strip_prefix("digit")
        .and_then(single)
        .filter(char::is_ascii_digit)
    {
        return Some(c.to_string());
    }
    if let Some(c) = name
        .strip_prefix("numpad")
        .and_then(single)
        .filter(char::is_ascii_digit)
    {
        return Some(format!("Numpad{c}"));
    }
    let upper = name.to_ascii_uppercase();
    if let Some(n) = function_number(&upper) {
        return Some(format!("F{n}"));
    }
    NAMED_KEYS
        .iter()
        .find(|known| known.eq_ignore_ascii_case(name))
        .map(|known| known.to_string())
}

impl Combo {
    /// Reads `Ctrl+Alt+D`. Modifiers come in any order, the key is last.
    /// Does not judge whether the combination is a sensible hotkey: see `check`.
    pub fn parse(text: &str) -> Result<Combo, ComboError> {
        if text.trim().is_empty() {
            return Err(ComboError::NoKey);
        }
        let mut combo = Combo {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            key: String::new(),
        };
        for raw in text.split('+') {
            let token = raw.trim();
            // An empty token, a second key, or anything after the key.
            if token.is_empty() || !combo.key.is_empty() {
                return Err(ComboError::UnknownKey(token.to_string()));
            }
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => combo.ctrl = true,
                "alt" => combo.alt = true,
                "shift" => combo.shift = true,
                "win" | "super" | "meta" | "cmd" => combo.win = true,
                _ => {
                    combo.key = canonical_key(token)
                        .ok_or_else(|| ComboError::UnknownKey(token.to_string()))?;
                }
            }
        }
        if combo.key.is_empty() {
            return Err(ComboError::NoKey);
        }
        Ok(combo)
    }

    /// Whether the combination may be a global hotkey: it must not get in the
    /// way of typing or of a usual Windows command.
    pub fn check(&self) -> Result<(), ComboError> {
        let strong = self.ctrl || self.alt || self.win;
        if matches!(self.key.as_str(), "Pause" | "ScrollLock") {
            return Ok(());
        }
        match function_number(&self.key) {
            Some(13..) => Ok(()),
            Some(n) => {
                if !strong {
                    return Err(ComboError::Modifier);
                }
                // Alt+F4 closes a window, Ctrl+F4 a tab.
                if n == 4 && !self.shift && !self.win && (self.ctrl != self.alt) {
                    return Err(ComboError::Reserved);
                }
                Ok(())
            }
            None => {
                let count = [self.ctrl, self.alt, self.shift, self.win]
                    .iter()
                    .filter(|held| **held)
                    .count();
                if count < 2 || !strong {
                    return Err(ComboError::Modifier);
                }
                // Ctrl+Shift with these keys is a command of nearly every
                // program: a global hotkey would take it away from all of them.
                if self.ctrl
                    && self.shift
                    && !self.alt
                    && !self.win
                    && EDITING_KEYS.contains(&self.key.as_str())
                {
                    return Err(ComboError::Reserved);
                }
                Ok(())
            }
        }
    }

    /// The form `tauri-plugin-global-shortcut` parses.
    pub fn accelerator(&self) -> String {
        self.written("Super")
    }

    fn written(&self, win: &str) -> String {
        let mut out = String::new();
        for (held, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.win, win),
        ] {
            if held {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(&self.key);
        out
    }
}

impl fmt::Display for Combo {
    /// `Ctrl+Alt+Shift+Win+D`: the form stored in the settings and shown to the user.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.written("Win"))
    }
}

/// A combination that may be bound: `parse`, then `check`.
pub fn combo(text: &str) -> Result<Combo, ComboError> {
    let parsed = Combo::parse(text)?;
    parsed.check()?;
    Ok(parsed)
}

/// What to try for an action: the user's combination alone when there is a
/// valid one, otherwise the defaults in order, without those another action
/// has as its own combination. Unknown id: nothing.
pub fn candidates(id: &str, custom: &BTreeMap<String, String>) -> Vec<Combo> {
    let Some(def) = hotkey(id) else {
        return Vec::new();
    };
    if let Some(text) = custom.get(id) {
        match combo(text) {
            Ok(own) => return vec![own],
            // Broken by hand in config.toml: the action keeps working on its defaults.
            Err(e) => log::warn!(
                "hotkey for `{id}` in the settings is not usable ({e}), using the defaults"
            ),
        }
    }
    // A default that another action got as its own combination is not tried:
    // the user's choice wins over a default, whatever the order of the list.
    let chosen: Vec<Combo> = custom
        .iter()
        .filter(|(other, _)| other.as_str() != id && hotkey(other).is_some())
        .filter_map(|(_, text)| combo(text).ok())
        .collect();
    def.defaults
        .iter()
        .filter_map(|text| combo(text).ok())
        .filter(|default| !chosen.contains(default))
        .collect()
}

/// The action, other than `except`, that already holds the combination. An
/// action with its own combination holds only that one; an action without one
/// holds all its defaults.
pub fn owner(
    combo: &Combo,
    custom: &BTreeMap<String, String>,
    except: &str,
) -> Option<&'static str> {
    HOTKEYS
        .iter()
        .filter(|def| def.id != except)
        .find(|def| candidates(def.id, custom).contains(combo))
        .map(|def| def.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(text: &str) -> Combo {
        Combo::parse(text).unwrap()
    }

    fn custom(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, combo)| (id.to_string(), combo.to_string()))
            .collect()
    }

    #[test]
    fn parse_reads_modifiers_in_any_order_and_case() {
        let want = Combo {
            ctrl: true,
            alt: true,
            shift: false,
            win: false,
            key: "D".into(),
        };
        assert_eq!(c("Ctrl+Alt+D"), want);
        assert_eq!(c("alt+CTRL+d"), want);
        assert_eq!(c("  Alt + Control +  D "), want);
        assert_eq!(c("Ctrl+Alt+KeyD"), want);
        let all = c("win+shift+alt+ctrl+f9");
        assert!(all.ctrl && all.alt && all.shift && all.win);
        assert_eq!(all.key, "F9");
    }

    #[test]
    fn parse_knows_the_aliases_of_win() {
        for alias in ["Win", "Super", "Meta", "Cmd", "super", "META"] {
            let got = c(&format!("{alias}+Shift+S"));
            assert!(got.win && got.shift && !got.ctrl && !got.alt, "{alias}");
            assert_eq!(got.key, "S");
        }
    }

    #[test]
    fn parse_accepts_browser_codes_and_canonical_names() {
        assert_eq!(c("Ctrl+Alt+Digit7").key, "7");
        assert_eq!(c("Ctrl+Alt+7").key, "7");
        assert_eq!(c("Ctrl+Alt+ArrowUp").key, "Up");
        assert_eq!(c("Ctrl+Alt+ArrowDown").key, "Down");
        assert_eq!(c("Ctrl+Alt+ArrowLeft").key, "Left");
        assert_eq!(c("Ctrl+Alt+arrowright").key, "Right");
        assert_eq!(c("Ctrl+Alt+pageup").key, "PageUp");
        assert_eq!(c("Ctrl+Alt+SPACE").key, "Space");
        assert_eq!(c("Ctrl+Alt+bracketleft").key, "BracketLeft");
        assert_eq!(c("Ctrl+Alt+numpad5").key, "Numpad5");
        assert_eq!(c("f24").key, "F24");
        assert_eq!(c("Pause").key, "Pause");
        assert_eq!(c("scrolllock").key, "ScrollLock");
        // The letter F is a key of its own, not a function key.
        assert_eq!(c("Ctrl+Alt+F").key, "F");
        assert_eq!(c("Ctrl+Alt+KeyF").key, "F");
    }

    #[test]
    fn parse_without_a_key_is_no_key() {
        for text in ["", "   ", "Ctrl+Alt", "shift", "Win+Ctrl+Alt+Shift"] {
            assert_eq!(Combo::parse(text), Err(ComboError::NoKey), "{text:?}");
        }
    }

    #[test]
    fn parse_refuses_keys_outside_the_list() {
        let unknown = |text: &str, token: &str| {
            assert_eq!(
                Combo::parse(text),
                Err(ComboError::UnknownKey(token.into())),
                "{text:?}"
            );
        };
        unknown("Ctrl+Alt+Escape", "Escape");
        unknown("Ctrl+Alt+Tab", "Tab");
        unknown("Ctrl+Alt+Enter", "Enter");
        unknown("Ctrl+Alt+Delete", "Delete");
        unknown("Ctrl+Alt+Backspace", "Backspace");
        unknown("Ctrl+Alt+Insert", "Insert");
        unknown("Ctrl+Alt+PrintScreen", "PrintScreen");
        unknown("Ctrl+Alt+CapsLock", "CapsLock");
        unknown("Ctrl+Alt+NumLock", "NumLock");
        unknown("Ctrl+Alt+MediaPlayPause", "MediaPlayPause");
        unknown("Ctrl+Alt+F25", "F25");
        unknown("Ctrl+Alt+F0", "F0");
        unknown("Ctrl+Alt+F01", "F01");
        unknown("Ctrl+Alt+Key", "Key");
        unknown("Ctrl+Alt+KeyDD", "KeyDD");
        unknown("Ctrl+Alt+Digit", "Digit");
        unknown("Ctrl+Alt+Numpad10", "Numpad10");
        unknown("Ctrl+Alt+Ж", "Ж");
        // Two keys, a modifier after the key, an empty token.
        unknown("Ctrl+A+B", "B");
        unknown("Ctrl+D+Alt", "Alt");
        unknown("Ctrl++D", "");
        unknown("Ctrl+Alt+", "");
    }

    #[test]
    fn display_has_a_fixed_order_and_round_trips() {
        assert_eq!(
            c("win+shift+alt+ctrl+d").to_string(),
            "Ctrl+Alt+Shift+Win+D"
        );
        assert_eq!(c("shift+ctrl+KeyM").to_string(), "Ctrl+Shift+M");
        assert_eq!(c("f13").to_string(), "F13");

        let mut keys: Vec<String> = NAMED_KEYS.iter().map(|k| k.to_string()).collect();
        keys.extend(('A'..='Z').map(|ch| ch.to_string()));
        keys.extend(('0'..='9').map(|ch| ch.to_string()));
        keys.extend((1..=24).map(|n| format!("F{n}")));
        keys.extend((0..=9).map(|n| format!("Numpad{n}")));
        for key in keys {
            for bits in 0..16u8 {
                let combo = Combo {
                    ctrl: bits & 1 != 0,
                    alt: bits & 2 != 0,
                    shift: bits & 4 != 0,
                    win: bits & 8 != 0,
                    key: key.clone(),
                };
                assert_eq!(Combo::parse(&combo.to_string()), Ok(combo.clone()));
                // The accelerator differs only in the name of Win.
                assert_eq!(Combo::parse(&combo.accelerator()), Ok(combo));
            }
        }
    }

    #[test]
    fn accelerator_writes_super_for_win() {
        assert_eq!(c("Win+Shift+S").accelerator(), "Shift+Super+S");
        assert_eq!(
            c("win+shift+alt+ctrl+PageDown").accelerator(),
            "Ctrl+Alt+Shift+Super+PageDown"
        );
        assert_eq!(c("Ctrl+Alt+D").accelerator(), "Ctrl+Alt+D");
        assert_eq!(c("F13").accelerator(), "F13");
    }

    #[test]
    fn check_follows_the_rules() {
        let ok = |text: &str| assert_eq!(c(text).check(), Ok(()), "{text}");
        let bad = |text: &str, err: ComboError| assert_eq!(c(text).check(), Err(err), "{text}");

        bad("D", ComboError::Modifier);
        bad("Ctrl+D", ComboError::Modifier);
        bad("Shift+D", ComboError::Modifier);
        bad("Alt+D", ComboError::Modifier);
        bad("Win+D", ComboError::Modifier);
        ok("Ctrl+Alt+D");
        ok("Ctrl+Shift+D");
        ok("Alt+Shift+D");
        ok("Win+Shift+D");
        ok("Ctrl+Alt+Shift+Win+D");

        bad("F9", ComboError::Modifier);
        bad("Shift+F9", ComboError::Modifier);
        ok("Ctrl+F9");
        ok("Alt+F9");
        ok("Win+F9");
        bad("Alt+F4", ComboError::Reserved);
        bad("Ctrl+F4", ComboError::Reserved);
        ok("Ctrl+Alt+F4");
        ok("Alt+Shift+F4");
        ok("Ctrl+Shift+F4");
        ok("Win+F4");
        ok("Alt+F12");

        ok("F13");
        ok("F24");
        ok("Pause");
        ok("ScrollLock");
        ok("Shift+F13");

        bad("Ctrl+Space", ComboError::Modifier);
        ok("Ctrl+Alt+Space");
        bad("Shift+Up", ComboError::Modifier);
        bad("Ctrl+7", ComboError::Modifier);
        ok("Ctrl+Shift+7");
        bad("Alt+Numpad1", ComboError::Modifier);
        ok("Ctrl+Alt+Numpad1");
        bad("Ctrl+Home", ComboError::Modifier);
        ok("Ctrl+Win+Home");
        bad("Alt+Slash", ComboError::Modifier);

        // Ctrl+Shift with the keys every program uses for selecting and editing.
        for key in [
            "Left", "Right", "Up", "Down", "Home", "End", "PageUp", "PageDown", "Z", "V", "C", "T",
            "N",
        ] {
            bad(&format!("Ctrl+Shift+{key}"), ComboError::Reserved);
            // A third modifier, or Alt in place of Shift, is nobody's command.
            ok(&format!("Ctrl+Alt+Shift+{key}"));
            ok(&format!("Ctrl+Alt+{key}"));
        }
    }

    #[test]
    fn combo_parses_then_checks() {
        assert_eq!(combo("ctrl+alt+KeyJ"), Ok(c("Ctrl+Alt+J")));
        assert_eq!(combo("Ctrl+J"), Err(ComboError::Modifier));
        assert_eq!(combo("Alt+F4"), Err(ComboError::Reserved));
        assert_eq!(combo("Ctrl+Alt"), Err(ComboError::NoKey));
        assert_eq!(
            combo("Ctrl+Alt+Tab"),
            Err(ComboError::UnknownKey("Tab".into()))
        );
    }

    #[test]
    fn error_codes_name_the_locale_keys() {
        assert_eq!(ComboError::NoKey.code(), "no_key");
        assert_eq!(ComboError::UnknownKey("Tab".into()).code(), "unknown_key");
        assert_eq!(ComboError::Modifier.code(), "modifier");
        assert_eq!(ComboError::Reserved.code(), "reserved");
        assert!(ComboError::UnknownKey("Tab".into())
            .to_string()
            .contains("Tab"));
    }

    #[test]
    fn the_list_is_sound() {
        let ids: Vec<&str> = HOTKEYS.iter().map(|def| def.id).collect();
        assert_eq!(
            ids,
            ["chat", "listen", "selection", "dictation", "screen_ask"]
        );
        let mut seen_ids = std::collections::HashSet::new();
        let mut seen_combos = std::collections::HashSet::new();
        for def in HOTKEYS {
            assert!(seen_ids.insert(def.id), "{} twice", def.id);
            assert!(!def.defaults.is_empty(), "{} has no default", def.id);
            for text in def.defaults {
                let parsed = combo(text).unwrap_or_else(|e| panic!("{text}: {e}"));
                // Defaults are written in the canonical form.
                assert_eq!(parsed.to_string(), *text);
                assert!(seen_combos.insert(parsed), "{text} twice");
            }
        }
        assert!(hotkey("dictation").unwrap().on_release);
        assert!(HOTKEYS
            .iter()
            .all(|def| def.on_release == (def.id == "dictation")));
        assert_eq!(hotkey("chat").unwrap().defaults[0], "Ctrl+Alt+M");
        assert!(hotkey("nope").is_none());
    }

    #[test]
    fn candidates_are_the_own_combination_or_the_defaults() {
        let none = BTreeMap::new();
        assert_eq!(
            candidates("dictation", &none),
            [c("Ctrl+Alt+D"), c("Ctrl+Alt+Shift+D")]
        );
        let own = custom(&[("dictation", "ctrl+alt+KeyJ")]);
        assert_eq!(candidates("dictation", &own), [c("Ctrl+Alt+J")]);
        // The choice of one action does not change another.
        assert_eq!(
            candidates("chat", &own),
            [c("Ctrl+Alt+M"), c("Ctrl+Shift+M")]
        );
        for broken in ["Ctrl+J", "Ctrl+Alt+Tab", "", "Alt+F4"] {
            let map = custom(&[("dictation", broken)]);
            assert_eq!(
                candidates("dictation", &map),
                [c("Ctrl+Alt+D"), c("Ctrl+Alt+Shift+D")],
                "{broken:?}"
            );
        }
        assert!(candidates("nope", &none).is_empty());
        assert!(candidates("nope", &custom(&[("nope", "Ctrl+Alt+J")])).is_empty());
    }

    #[test]
    fn a_default_another_action_chose_is_not_tried() {
        // Chat moved away and dictation took its first default; then chat was
        // put back to its defaults (only dictation's entry is left).
        let map = custom(&[("dictation", "Ctrl+Alt+M")]);
        assert_eq!(candidates("dictation", &map), [c("Ctrl+Alt+M")]);
        assert_eq!(candidates("chat", &map), [c("Ctrl+Shift+M")]);
        assert_eq!(owner(&c("Ctrl+Alt+M"), &map, "chat"), Some("dictation"));
        assert_eq!(owner(&c("Ctrl+Alt+M"), &map, "dictation"), None);
        // Both defaults chosen by others: nothing is left to try.
        let map = custom(&[("dictation", "Ctrl+Alt+M"), ("listen", "Ctrl+Shift+M")]);
        assert!(candidates("chat", &map).is_empty());
        // A broken entry and an entry of an unknown action take nothing away.
        let map = custom(&[("dictation", "Ctrl+M"), ("nope", "Ctrl+Alt+M")]);
        assert_eq!(
            candidates("chat", &map),
            [c("Ctrl+Alt+M"), c("Ctrl+Shift+M")]
        );
    }

    #[test]
    fn owner_finds_who_holds_a_combination() {
        let none = BTreeMap::new();
        // A default of another action, the main one and the spare one.
        assert_eq!(owner(&c("Ctrl+Alt+M"), &none, "dictation"), Some("chat"));
        assert_eq!(
            owner(&c("Ctrl+Shift+K"), &none, "dictation"),
            Some("listen")
        );
        // Its own combination does not stand in its way.
        assert_eq!(owner(&c("Ctrl+Alt+D"), &none, "dictation"), None);
        assert_eq!(owner(&c("Ctrl+Alt+D"), &none, "chat"), Some("dictation"));
        assert_eq!(owner(&c("Ctrl+Alt+J"), &none, "dictation"), None);

        // Another action's own combination.
        let own = custom(&[("chat", "Ctrl+Alt+J")]);
        assert_eq!(owner(&c("Ctrl+Alt+J"), &own, "dictation"), Some("chat"));
        assert_eq!(owner(&c("Ctrl+Alt+J"), &own, "chat"), None);
        // An action with its own combination no longer holds its defaults.
        assert_eq!(owner(&c("Ctrl+Alt+M"), &own, "dictation"), None);
        assert_eq!(owner(&c("Ctrl+Shift+M"), &own, "dictation"), None);
        // A broken entry leaves the defaults held.
        let broken = custom(&[("chat", "Ctrl+M")]);
        assert_eq!(owner(&c("Ctrl+Alt+M"), &broken, "dictation"), Some("chat"));
    }
}
