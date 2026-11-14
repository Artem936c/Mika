//! Categories of records and the table "module path -> category".
//!
//! A record names its category as the target: `log::info!(target: "tool", …)`.
//! A record without one carries the path of its module; the table below maps
//! it. Records of other crates go to `lib`.

pub const APP: &str = "app";
pub const LIB: &str = "lib";

/// Every category, in the order the interface lists them.
pub const CATEGORIES: [&str; 16] = [
    "app", "turn", "llm", "tool", "policy", "voice", "telegram", "mcp", "events", "jobs", "costs",
    "backup", "update", "hotkeys", "web", "lib",
];

/// Module path -> category. A path matches an entry when it is the entry or
/// lies inside it; the longest entry wins. A module of ours that is not
/// listed goes to `app`.
const BY_MODULE: &[(&str, &str)] = &[
    ("mika_core::agent", "turn"),
    ("mika_core::session", "turn"),
    ("mika_core::task", "turn"),
    ("mika_core::llm", "llm"),
    ("mika_core::tools", "tool"),
    ("mika_core::policy", "policy"),
    ("mika_core::gate", "policy"),
    ("mika_core::confirm", "policy"),
    ("mika_core::sound", "voice"),
    ("mika_core::sound_capture", "voice"),
    ("mika_core::speech_cache", "voice"),
    ("mika_core::marks", "voice"),
    ("mika_core::triggers", "events"),
    ("mika_core::bgjobs", "jobs"),
    ("mika_core::schedule", "jobs"),
    ("mika_core::costs", "costs"),
    ("mika_core::backup", "backup"),
    ("mika_core::hotkeys", "hotkeys"),
    ("mika_providers", "llm"),
    ("mika_providers::telegram", "telegram"),
    ("mika_providers::mcp", "mcp"),
    ("mika_providers::speech", "voice"),
    ("mika_providers::metering", "costs"),
    ("mika_audio", "voice"),
    ("mika_tools", "tool"),
    ("mika_tools::research", "web"),
    ("mika_tools::web", "web"),
    ("mika_tools::search", "web"),
    ("mika_tools::win::http", "web"),
    ("mika_tools::observe", "events"),
    ("mika_tools::ffmpeg", "jobs"),
    ("mika_tools::costs", "costs"),
    ("mika_gui::hotkeys", "hotkeys"),
    ("mika_gui::hotkey_actions", "hotkeys"),
    ("mika_gui::update", "update"),
    ("mika_gui::backup", "backup"),
    ("mika_gui::costs", "costs"),
    ("mika_gui::bgjobs", "jobs"),
    ("mika_gui::events_page", "events"),
    ("mika_gui::research", "web"),
    ("mika_gui::vosk", "voice"),
];

/// The category of a record by its target: a category name as it is, a module
/// path of ours by the table, anything else `lib`.
pub fn category_of(target: &str) -> &'static str {
    if let Some(known) = CATEGORIES.iter().find(|c| **c == target) {
        return known;
    }
    if !ours(target) {
        return LIB;
    }
    BY_MODULE
        .iter()
        .filter(|(module, _)| inside(target, module))
        .max_by_key(|(module, _)| module.len())
        .map_or(APP, |(_, category)| category)
}

/// A module path of one of our crates: `mika_core`, `mika_gui::tray`.
fn ours(target: &str) -> bool {
    let krate = target.split("::").next().unwrap_or(target);
    krate.starts_with("mika_")
}

fn inside(path: &str, module: &str) -> bool {
    path.strip_prefix(module)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_category_name_is_taken_as_it_is() {
        for category in CATEGORIES {
            assert_eq!(category_of(category), category);
        }
    }

    #[test]
    fn a_module_of_ours_goes_by_the_table() {
        let cases = [
            ("mika_core::agent", "turn"),
            ("mika_core::agent::tests", "turn"),
            ("mika_core::policy::blind", "policy"),
            ("mika_core::costs::tracker", "costs"),
            ("mika_core::sound", "voice"),
            // `sound_capture` is a module of its own, not a part of `sound`.
            ("mika_core::sound_capture", "voice"),
            ("mika_core::config", "app"),
            ("mika_core::journal::sink", "app"),
            ("mika_core", "app"),
            ("mika_providers::openai", "llm"),
            ("mika_providers::telegram::voice", "telegram"),
            ("mika_providers::mcp", "mcp"),
            ("mika_providers::speech", "voice"),
            ("mika_audio::wake", "voice"),
            ("mika_tools::files", "tool"),
            ("mika_tools::research::run", "web"),
            ("mika_tools::observe::service", "events"),
            ("mika_tools::ffmpeg::record::plan", "jobs"),
            ("mika_tools::win::http", "web"),
            ("mika_tools::win::uia", "tool"),
            ("mika_gui::hotkeys", "hotkeys"),
            ("mika_gui::update", "update"),
            ("mika_gui::app", "app"),
            ("mika_gui", "app"),
            ("mika_cli", "app"),
        ];
        for (target, category) in cases {
            assert_eq!(category_of(target), category, "{target}");
        }
    }

    #[test]
    fn everything_else_is_a_library() {
        for target in [
            "reqwest::connect",
            "tauri",
            "hyper",
            "",
            "Tool",
            "mikado::x",
        ] {
            assert_eq!(category_of(target), LIB, "{target:?}");
        }
    }

    #[test]
    fn the_table_names_only_known_categories_once() {
        for (module, category) in BY_MODULE {
            assert!(CATEGORIES.contains(category), "{module} -> {category}");
            assert_eq!(
                BY_MODULE.iter().filter(|(m, _)| m == module).count(),
                1,
                "{module}"
            );
        }
    }
}
