//! How settings from a backup land on this PC.
//!
//! Everything that listens, looks, tracks, starts programs or takes commands
//! from outside arrives switched off: the user turns it on here, on this PC.
//! What reaches the network or spends money by itself is never turned on by
//! a file. What belongs to the machine (the window, the path to FFmpeg, the
//! update address) stays.

use super::MAX_LANDED_NAME;
use crate::config::Config;

/// Something that was on in the backup and is off after the restore.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "id", rename_all = "snake_case")]
pub enum SwitchedOff {
    Microphone,
    Screen,
    Notifications,
    AppTime,
    Clipboard,
    /// Dictation into any window: it listens to the microphone.
    Dictation,
    /// The question about a picture of the window in front: it looks at the
    /// screen and sends the picture to the provider.
    ScreenAsk,
    Telegram,
    /// Voice messages of the Telegram bridge, in or out: paid recognition
    /// and synthesis nobody at this PC asked for, and a sound sent away.
    TelegramVoice,
    /// Tools that reach the network: on in the backup, off on this PC.
    Web,
    /// The calendar link is fetched by itself: on in the backup, off here.
    Calendar,
    /// The morning briefing is a paid request made by itself: on in the
    /// backup, off here.
    Briefing,
    /// The evening summary of the day: the same, a paid request made by
    /// itself. On in the backup, off here.
    Evening,
    /// The main switch of the scenarios by event: on in the backup, paused
    /// on this PC.
    Events,
    /// MCP servers that were enabled.
    Mcp {
        count: usize,
    },
    /// Document folders that were listed.
    Docs {
        count: usize,
    },
    /// Scenarios by event that came from the backup: each one lands switched
    /// off. Listed by a restore, not by `land_config`: they are rows of the
    /// database, not settings.
    Scenarios {
        count: usize,
    },
}

/// Settings ready for this PC and what was changed in them.
#[derive(Debug, Clone, PartialEq)]
pub struct Landing {
    pub config: Config,
    pub switched_off: Vec<SwitchedOff>,
    /// The document folders of the backup: the list itself lands empty.
    pub docs_folders: Vec<String>,
}

/// A name from the file, or the one of this PC when the file's is unusable.
fn landed_name(incoming: &str, here: &str) -> String {
    let name = incoming.trim();
    if name.is_empty() || name.chars().count() > MAX_LANDED_NAME {
        here.to_string()
    } else {
        name.to_string()
    }
}

/// Turns the settings of a backup into settings for this PC.
///
/// Three rules. What listens, looks, tracks, starts programs or takes
/// commands from outside lands off whatever this PC had. What reaches the
/// network or spends money by itself stays on only when it is on in both: a
/// file never turns it on. What names a program or a place of this machine
/// is not taken from a file at all.
pub fn land_config(incoming: Config, here: &Config) -> Landing {
    let mut config = incoming;
    let mut switched_off = Vec::new();

    if config.privacy.microphone_enabled {
        switched_off.push(SwitchedOff::Microphone);
    }
    if config.privacy.screen_enabled {
        switched_off.push(SwitchedOff::Screen);
    }
    if config.notifications.enabled {
        switched_off.push(SwitchedOff::Notifications);
    }
    if config.tracking.app_time {
        switched_off.push(SwitchedOff::AppTime);
    }
    if config.tracking.clipboard {
        switched_off.push(SwitchedOff::Clipboard);
    }
    if config.dictation.enabled {
        switched_off.push(SwitchedOff::Dictation);
    }
    if config.screen_ask.enabled {
        switched_off.push(SwitchedOff::ScreenAsk);
    }
    if config.telegram.enabled {
        switched_off.push(SwitchedOff::Telegram);
    }
    if config.telegram.voice_in || config.telegram.voice_reply {
        switched_off.push(SwitchedOff::TelegramVoice);
    }
    if config.tools.web_enabled && !here.tools.web_enabled {
        switched_off.push(SwitchedOff::Web);
    }
    if config.calendar.enabled && !here.calendar.enabled {
        switched_off.push(SwitchedOff::Calendar);
    }
    if config.briefing.enabled && !here.briefing.enabled {
        switched_off.push(SwitchedOff::Briefing);
    }
    if config.briefing.evening_enabled && !here.briefing.evening_enabled {
        switched_off.push(SwitchedOff::Evening);
    }
    if config.events.enabled && !here.events.enabled {
        switched_off.push(SwitchedOff::Events);
    }
    let servers = config.mcp.servers.iter().filter(|s| s.enabled).count();
    if servers > 0 {
        switched_off.push(SwitchedOff::Mcp { count: servers });
    }
    let docs_folders = std::mem::take(&mut config.docs.folders);
    if !docs_folders.is_empty() {
        switched_off.push(SwitchedOff::Docs {
            count: docs_folders.len(),
        });
    }

    config.privacy.microphone_enabled = false;
    config.privacy.screen_enabled = false;
    config.notifications.enabled = false;
    config.tracking.app_time = false;
    config.tracking.clipboard = false;
    // Dictation listens and the screen question looks: each has a switch of
    // its own besides the microphone and the screen, and both land off.
    config.dictation.enabled = false;
    config.screen_ask.enabled = false;
    // The owner id is kept: the bridge stays off until the user turns it on.
    config.telegram.enabled = false;
    // So do its voice messages: turning the bridge on again must not start
    // paid recognition and voice replies by itself.
    config.telegram.voice_in = false;
    config.telegram.voice_reply = false;
    for server in &mut config.mcp.servers {
        server.enabled = false;
    }

    // A file never turns these on: the user did that on this PC or did not.
    config.tools.web_enabled &= here.tools.web_enabled;
    config.calendar.enabled &= here.calendar.enabled;
    config.briefing.enabled &= here.briefing.enabled;
    config.briefing.evening_enabled &= here.briefing.evening_enabled;
    // Scenarios paused on this PC stay paused: every run is a paid request.
    config.events.enabled &= here.events.enabled;
    // And a file never lifts the pause that a reached budget puts on the
    // paid work she starts by herself (decision 0028). The switch alone is
    // not the pause: a file with counting off, with no limit or a higher
    // one, or with lower prices of its own would lift it just the same. So
    // while a budget holds that work on this PC, the whole budget stays as
    // it is here: counting, limits, currency and prices.
    if here.costs.holds_unattended() {
        config.costs = here.costs.clone();
    }
    config.costs.pause_unattended |= here.costs.pause_unattended;
    // A tool group switched off on this PC stays off.
    for group in &here.tools.disabled_groups {
        if !config.tools.disabled_groups.contains(group) {
            config.tools.disabled_groups.push(group.clone());
        }
    }

    // A path to a program never comes from a file: the app starts what
    // stands there (decision 0034).
    config.tools.ffmpeg_path = here.tools.ffmpeg_path.clone();
    // Nor does a place on this machine that she writes files to: the folder
    // of the deep search reports could name somebody's network share.
    config.research.folder = here.research.folder.clone();
    // The window is the one that is open now: its mode and its place.
    config.window = here.window.clone();
    // An update address from a file could point at somebody's installer.
    config.updates = here.updates.clone();
    config.assistant.onboarded = true;
    config.assistant.name = landed_name(&config.assistant.name, &here.assistant.name);
    config.assistant.wake_phrase =
        landed_name(&config.assistant.wake_phrase, &here.assistant.wake_phrase);

    Landing {
        config,
        switched_off,
        docs_folders,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{McpServerConfig, WindowMode};

    fn server(name: &str, enabled: bool) -> McpServerConfig {
        McpServerConfig {
            name: name.into(),
            command: "npx".into(),
            args: vec!["-y".into(), "server".into()],
            enabled,
            url: String::new(),
        }
    }

    /// Settings with everything on that must land off.
    fn loud() -> Config {
        let mut c = Config::default();
        c.privacy.microphone_enabled = true;
        c.privacy.screen_enabled = true;
        c.notifications.enabled = true;
        c.tracking.app_time = true;
        c.tracking.clipboard = true;
        c.dictation.enabled = true;
        c.screen_ask.enabled = true;
        c.telegram.enabled = true;
        c.telegram.user_id = 123_456_789;
        c.telegram.voice_in = true;
        c.telegram.voice_reply = true;
        c.tools.web_enabled = true;
        c.calendar.enabled = true;
        c.briefing.enabled = true;
        c.briefing.evening_enabled = true;
        c.mcp.servers = vec![
            server("files", true),
            server("off", false),
            server("git", true),
        ];
        c.docs.folders = vec!["C:\\Docs".into(), "D:\\Work".into()];
        c
    }

    #[test]
    fn everything_that_listens_or_acts_lands_off_and_is_listed() {
        // Scenarios are paused on this PC; everything else is as installed.
        let mut here = Config::default();
        here.events.enabled = false;
        let landing = land_config(loud(), &here);
        let c = &landing.config;
        assert!(!c.privacy.microphone_enabled && !c.privacy.screen_enabled);
        assert!(!c.notifications.enabled);
        assert!(!c.tracking.app_time && !c.tracking.clipboard);
        assert!(!c.dictation.enabled && !c.screen_ask.enabled);
        assert!(!c.telegram.enabled);
        assert!(!c.telegram.voice_in && !c.telegram.voice_reply);
        assert_eq!(c.telegram.user_id, 123_456_789);
        assert!(!c.tools.web_enabled && !c.calendar.enabled && !c.briefing.enabled);
        assert!(!c.briefing.evening_enabled && !c.events.enabled);
        assert!(c.mcp.servers.iter().all(|s| !s.enabled));
        // The servers themselves stay, with their commands.
        assert_eq!(c.mcp.servers.len(), 3);
        assert_eq!(c.mcp.servers[0].command, "npx");
        assert!(c.docs.folders.is_empty());
        assert_eq!(landing.docs_folders, ["C:\\Docs", "D:\\Work"]);
        assert_eq!(
            landing.switched_off,
            vec![
                SwitchedOff::Microphone,
                SwitchedOff::Screen,
                SwitchedOff::Notifications,
                SwitchedOff::AppTime,
                SwitchedOff::Clipboard,
                SwitchedOff::Dictation,
                SwitchedOff::ScreenAsk,
                SwitchedOff::Telegram,
                SwitchedOff::TelegramVoice,
                SwitchedOff::Web,
                SwitchedOff::Calendar,
                SwitchedOff::Briefing,
                SwitchedOff::Evening,
                SwitchedOff::Events,
                SwitchedOff::Mcp { count: 2 },
                SwitchedOff::Docs { count: 2 },
            ]
        );
    }

    /// Every switch of `Config` that turns on a sensor, the network or a
    /// paid action of her own, by its path. The list is kept by hand: a new
    /// section with such a switch is added here and to `land_config`.
    fn switches(c: &Config) -> Vec<(&'static str, bool)> {
        vec![
            ("privacy.microphone_enabled", c.privacy.microphone_enabled),
            ("privacy.screen_enabled", c.privacy.screen_enabled),
            ("notifications.enabled", c.notifications.enabled),
            ("tracking.app_time", c.tracking.app_time),
            ("tracking.clipboard", c.tracking.clipboard),
            ("dictation.enabled", c.dictation.enabled),
            ("screen_ask.enabled", c.screen_ask.enabled),
            ("telegram.enabled", c.telegram.enabled),
            ("telegram.voice_in", c.telegram.voice_in),
            ("telegram.voice_reply", c.telegram.voice_reply),
            ("tools.web_enabled", c.tools.web_enabled),
            ("calendar.enabled", c.calendar.enabled),
            ("briefing.enabled", c.briefing.enabled),
            ("briefing.evening_enabled", c.briefing.evening_enabled),
        ]
    }

    #[test]
    fn nothing_that_listens_looks_or_pays_by_itself_is_on_after_a_restore_into_a_new_pc() {
        let mut file = loud();
        file.events.enabled = true;
        for (name, on) in switches(&file) {
            assert!(on, "the test file must have {name} on");
        }
        // A PC as it is right after the installation.
        let landed = land_config(file, &Config::default()).config;
        for (name, on) in switches(&landed) {
            assert!(!on, "{name} must land off");
        }
        assert!(landed.mcp.servers.iter().all(|s| !s.enabled));
        assert!(landed.docs.folders.is_empty());
    }

    #[test]
    fn the_new_switches_are_listed_one_at_a_time() {
        let one = |turn_on: fn(&mut Config)| {
            let mut c = Config::default();
            turn_on(&mut c);
            land_config(c, &Config::default()).switched_off
        };
        assert_eq!(
            one(|c| c.dictation.enabled = true),
            vec![SwitchedOff::Dictation]
        );
        assert_eq!(
            one(|c| c.screen_ask.enabled = true),
            vec![SwitchedOff::ScreenAsk]
        );
        // Either voice switch of the bridge gives the one line.
        assert_eq!(
            one(|c| c.telegram.voice_in = true),
            vec![SwitchedOff::TelegramVoice]
        );
        assert_eq!(
            one(|c| c.telegram.voice_reply = true),
            vec![SwitchedOff::TelegramVoice]
        );
        assert_eq!(
            one(|c| c.briefing.evening_enabled = true),
            vec![SwitchedOff::Evening]
        );
        // Scenarios are on by default on both sides: nothing to list.
        assert!(one(|c| c.events.enabled = true).is_empty());
    }

    #[test]
    fn a_file_does_not_start_the_evening_summary_or_paused_scenarios() {
        let mut file = Config::default();
        file.briefing.evening_enabled = true;
        file.briefing.evening_time = "22:30".into();
        file.events.enabled = true;
        file.events.max_per_hour = 30;

        // On here too: it stays, and is not listed.
        let mut here = Config::default();
        here.briefing.evening_enabled = true;
        let landing = land_config(file.clone(), &here);
        assert!(landing.config.briefing.evening_enabled);
        assert!(landing.config.events.enabled);
        assert!(landing.switched_off.is_empty());
        // The plain settings beside the switches travel.
        assert_eq!(landing.config.briefing.evening_time, "22:30");
        assert_eq!(landing.config.events.max_per_hour, 30);

        // Scenarios paused here stay paused, and the list says so.
        let mut here = Config::default();
        here.events.enabled = false;
        let landing = land_config(file.clone(), &here);
        assert!(!landing.config.events.enabled);
        assert!(!landing.config.briefing.evening_enabled);
        assert_eq!(
            landing.switched_off,
            vec![SwitchedOff::Evening, SwitchedOff::Events]
        );

        // Off in the file, on here: the file's choice, nothing to list.
        let mut here = Config::default();
        here.briefing.evening_enabled = true;
        let mut quiet = Config::default();
        quiet.events.enabled = false;
        let landing = land_config(quiet, &here);
        assert!(!landing.config.briefing.evening_enabled);
        assert!(!landing.config.events.enabled);
        assert!(landing.switched_off.is_empty());
    }

    #[test]
    fn a_file_never_lifts_the_budget_pause_and_never_names_the_report_folder() {
        let mut file = Config::default();
        file.costs.pause_unattended = false;
        file.costs.day_limit = 50.0;
        file.research.folder = "\\\\host\\share\\reports".into();
        file.research.max_pages = 7;
        let mut here = Config::default();
        here.research.folder = "D:\\Reports".into();

        let landing = land_config(file.clone(), &here);
        // On here: on after the restore, whatever the file says.
        assert!(landing.config.costs.pause_unattended);
        // The limits themselves are the user's settings and travel.
        assert_eq!(landing.config.costs.day_limit, 50.0);
        assert_eq!(landing.config.research.folder, "D:\\Reports");
        assert_eq!(landing.config.research.max_pages, 7);
        assert!(landing.switched_off.is_empty());

        // Off on both sides: the user's own choice on this PC stays.
        here.costs.pause_unattended = false;
        assert!(!land_config(file, &here).config.costs.pause_unattended);
        // On in the file, off here: on.
        assert!(
            land_config(Config::default(), &here)
                .config
                .costs
                .pause_unattended
        );
        // No folder set here: none comes from the file either.
        let mut file = Config::default();
        file.research.folder = "C:\\Elsewhere".into();
        assert_eq!(
            land_config(file, &Config::default()).config.research.folder,
            ""
        );
    }

    #[test]
    fn a_file_never_lifts_a_budget_that_holds_her_own_paid_work_on_this_pc() {
        use crate::costs::ManualPrice;
        // This PC: counting on, the pause on, 100 a day.
        let mut here = Config::default();
        here.costs.day_limit = 100.0;
        here.costs.currency = "RUB".into();
        here.costs.prices = vec![ManualPrice {
            provider: "api.example".into(),
            model: "big".into(),
            currency: "RUB".into(),
            input_mtok: 300.0,
            ..ManualPrice::default()
        }];
        assert!(here.costs.holds_unattended());

        // Every way a file could lift it: counting off, no limit, a higher
        // limit, the same number in a dearer currency, a price near zero.
        let lifts: [fn(&mut Config); 5] = [
            |c| c.costs.enabled = false,
            |c| c.costs.day_limit = 0.0,
            |c| c.costs.day_limit = 100_000.0,
            |c| c.costs.currency = "USD".into(),
            |c| c.costs.prices[0].input_mtok = 0.01,
        ];
        for lift in lifts {
            let mut file = here.clone();
            lift(&mut file);
            assert_ne!(file.costs, here.costs);
            let landed = land_config(file, &here);
            assert_eq!(landed.config.costs, here.costs);
            assert!(landed.config.costs.holds_unattended());
        }
        // A month limit holds it like a day limit.
        let mut monthly = Config::default();
        monthly.costs.month_limit = 900.0;
        let mut file = Config::default();
        file.costs.enabled = false;
        assert_eq!(land_config(file, &monthly).config.costs, monthly.costs);

        // No budget holds anything here (no limit; the pause off; counting
        // off): the budget of the file is an ordinary setting and travels.
        let mut file = Config::default();
        file.costs.enabled = false;
        file.costs.day_limit = 50.0;
        file.costs.currency = "EUR".into();
        let frees: [fn(&mut Config); 3] = [
            |c| c.costs.day_limit = 0.0,
            |c| c.costs.pause_unattended = false,
            |c| c.costs.enabled = false,
        ];
        for free in frees {
            let mut here = here.clone();
            free(&mut here);
            assert!(!here.costs.holds_unattended());
            let landed = land_config(file.clone(), &here).config.costs;
            assert!(!landed.enabled);
            assert_eq!((landed.day_limit, landed.currency.as_str()), (50.0, "EUR"));
            // The switch of the pause is still never turned off by a file.
            assert_eq!(
                landed.pause_unattended,
                file.costs.pause_unattended || here.costs.pause_unattended
            );
        }
    }

    #[test]
    fn a_file_never_turns_on_what_reaches_the_network_by_itself() {
        // On in the file, off here: off, and listed.
        let landing = land_config(loud(), &Config::default());
        assert!(!landing.config.tools.web_enabled);
        assert!(!landing.config.calendar.enabled);
        assert!(!landing.config.briefing.enabled);

        // On in both: the user turned it on here, it stays and is not listed.
        let mut here = Config::default();
        here.tools.web_enabled = true;
        here.calendar.enabled = true;
        here.briefing.enabled = true;
        let landing = land_config(loud(), &here);
        assert!(landing.config.tools.web_enabled);
        assert!(landing.config.calendar.enabled);
        assert!(landing.config.briefing.enabled);
        for gone in [
            SwitchedOff::Web,
            SwitchedOff::Calendar,
            SwitchedOff::Briefing,
        ] {
            assert!(!landing.switched_off.contains(&gone), "{gone:?}");
        }
        // The sensors land off even then.
        assert!(landing.switched_off.contains(&SwitchedOff::Microphone));

        // Off in the file, on here: the file's choice, nothing to list.
        let landing = land_config(Config::default(), &here);
        assert!(!landing.config.tools.web_enabled);
        assert!(!landing.config.calendar.enabled);
        assert!(!landing.config.briefing.enabled);
        assert!(landing.switched_off.is_empty());
    }

    #[test]
    fn a_tool_group_switched_off_here_stays_off() {
        let mut incoming = Config::default();
        incoming.tools.disabled_groups = vec!["rdp".into()];
        let mut here = Config::default();
        here.tools.disabled_groups = vec!["shell".into(), "rdp".into(), "files".into()];
        let c = land_config(incoming, &here).config;
        assert_eq!(c.tools.disabled_groups, ["rdp", "shell", "files"]);
        // A file with no group off does not bring back what was switched off here.
        let c = land_config(Config::default(), &here).config;
        assert_eq!(c.tools.disabled_groups, ["shell", "rdp", "files"]);
    }

    #[test]
    fn a_path_to_a_program_never_comes_from_a_file() {
        let mut incoming = Config::default();
        incoming.tools.ffmpeg_path = "\\\\host\\share".into();
        let mut here = Config::default();
        here.tools.ffmpeg_path = "D:\\Tools\\ffmpeg".into();
        assert_eq!(
            land_config(incoming.clone(), &here)
                .config
                .tools
                .ffmpeg_path,
            "D:\\Tools\\ffmpeg"
        );
        // Nothing set here: nothing comes from the file either.
        assert_eq!(
            land_config(incoming, &Config::default())
                .config
                .tools
                .ffmpeg_path,
            ""
        );
    }

    #[test]
    fn what_was_off_is_not_listed() {
        let mut quiet = Config::default();
        quiet.mcp.servers = vec![server("off", false)];
        quiet.telegram.user_id = 5;
        let landing = land_config(quiet, &Config::default());
        assert!(landing.switched_off.is_empty());
        assert!(landing.docs_folders.is_empty());
        // One switch at a time.
        let mut one = Config::default();
        one.tracking.clipboard = true;
        assert_eq!(
            land_config(one, &Config::default()).switched_off,
            vec![SwitchedOff::Clipboard]
        );
        let mut one = Config::default();
        one.privacy.screen_enabled = true;
        assert_eq!(
            land_config(one, &Config::default()).switched_off,
            vec![SwitchedOff::Screen]
        );
    }

    #[test]
    fn the_list_has_the_shape_the_interface_reads() {
        let json = serde_json::to_value(vec![
            SwitchedOff::Microphone,
            SwitchedOff::AppTime,
            SwitchedOff::Dictation,
            SwitchedOff::ScreenAsk,
            SwitchedOff::TelegramVoice,
            SwitchedOff::Web,
            SwitchedOff::Calendar,
            SwitchedOff::Briefing,
            SwitchedOff::Evening,
            SwitchedOff::Events,
            SwitchedOff::Mcp { count: 2 },
            SwitchedOff::Docs { count: 1 },
            SwitchedOff::Scenarios { count: 3 },
        ])
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                { "id": "microphone" },
                { "id": "app_time" },
                { "id": "dictation" },
                { "id": "screen_ask" },
                { "id": "telegram_voice" },
                { "id": "web" },
                { "id": "calendar" },
                { "id": "briefing" },
                { "id": "evening" },
                { "id": "events" },
                { "id": "mcp", "count": 2 },
                { "id": "docs", "count": 1 },
                { "id": "scenarios", "count": 3 },
            ])
        );
    }

    #[test]
    fn the_window_and_the_update_address_stay_from_this_pc() {
        let mut incoming = loud();
        incoming.window.mode = WindowMode::Window;
        incoming.window.monitor = Some("\\\\.\\DISPLAY7".into());
        incoming.window.rect = Some([1, 2, 3, 4]);
        incoming.updates.manifest_url = "https://evil.example/update.json".into();
        incoming.updates.check_at_start = true;
        let mut here = Config::default();
        here.window.mode = WindowMode::Island;
        here.window.monitor = Some("\\\\.\\DISPLAY1".into());
        here.window.rect = Some([10, 20, 800, 600]);
        here.updates.manifest_url = "https://mine.example/update.json".into();
        here.updates.check_at_start = false;

        let c = land_config(incoming, &here).config;
        // The mode too: the page draws the window that is open now.
        assert_eq!(c.window.mode, WindowMode::Island);
        assert_eq!(c.window.monitor.as_deref(), Some("\\\\.\\DISPLAY1"));
        assert_eq!(c.window.rect, Some([10, 20, 800, 600]));
        assert_eq!(c.updates, here.updates);

        // Nothing of the kind on this PC: nothing comes from the file either.
        let mut incoming = Config::default();
        incoming.window.monitor = Some("X".into());
        incoming.window.rect = Some([1, 2, 3, 4]);
        let c = land_config(incoming, &Config::default()).config;
        assert_eq!(c.window.monitor, None);
        assert_eq!(c.window.rect, None);
    }

    #[test]
    fn the_first_run_screen_is_not_shown_again() {
        let incoming = Config::default();
        assert!(!incoming.assistant.onboarded);
        assert!(
            land_config(incoming, &Config::default())
                .config
                .assistant
                .onboarded
        );
    }

    #[test]
    fn an_unusable_name_falls_back_to_the_one_here() {
        let mut here = Config::default();
        here.assistant.name = "Айри".into();
        here.assistant.wake_phrase = "Эй, Айри".into();
        let land = |name: &str, wake: &str| {
            let mut incoming = Config::default();
            incoming.assistant.name = name.into();
            incoming.assistant.wake_phrase = wake.into();
            let c = land_config(incoming, &here).config;
            (c.assistant.name, c.assistant.wake_phrase)
        };
        assert_eq!(
            land("  Юки \n", " Юки "),
            ("Юки".to_string(), "Юки".to_string())
        );
        assert_eq!(
            land("", "   "),
            ("Айри".to_string(), "Эй, Айри".to_string())
        );
        let long = "я".repeat(MAX_LANDED_NAME + 1);
        assert_eq!(
            land(&long, &long),
            ("Айри".to_string(), "Эй, Айри".to_string())
        );
        let longest = "я".repeat(MAX_LANDED_NAME);
        assert_eq!(land(&longest, "Юки"), (longest.clone(), "Юки".to_string()));
    }

    #[test]
    fn the_other_settings_travel_as_they_are() {
        let mut incoming = loud();
        incoming.tools.news_feed = "https://news.example/rss".into();
        incoming.tools.disabled_groups = vec!["rdp".into()];
        incoming.voice.input_device = "USB Mic".into();
        incoming.voice.voice = "Microsoft Irina".into();
        incoming.voice.wake_enabled = true;
        incoming.calendar.remind_min = 25;
        incoming.briefing.time = "07:15".into();
        // What the later parts of the app added and what is no switch of a
        // sensor, of the network or of a paid action: it travels too.
        incoming.briefing.evening_time = "22:10".into();
        incoming.costs.month_limit = 1500.0;
        incoming.costs.currency = "EUR".into();
        incoming
            .hotkeys
            .custom
            .insert("dictation".into(), "Ctrl+Alt+D".into());
        incoming.events.max_per_hour = 5;
        incoming.research.max_searches = 3;
        incoming.research.model = "small-model".into();
        incoming.llm.base_url = "https://routerai.ru/api/v1".into();
        incoming.island.city = "Пермь".into();
        let before = incoming.clone();
        let c = land_config(incoming, &Config::default()).config;
        assert_eq!(c.tools.news_feed, before.tools.news_feed);
        assert_eq!(c.tools.disabled_groups, before.tools.disabled_groups);
        assert_eq!(c.voice, before.voice);
        assert_eq!(c.calendar.remind_min, 25);
        assert_eq!(c.briefing.time, "07:15");
        assert_eq!(c.briefing.evening_time, "22:10");
        assert_eq!(c.costs, before.costs);
        assert_eq!(c.hotkeys, before.hotkeys);
        assert_eq!(c.events.max_per_hour, 5);
        assert_eq!(c.research, before.research);
        assert_eq!(c.i18n, before.i18n);
        assert_eq!(c.llm, before.llm);
        assert_eq!(c.island, before.island);
        assert_eq!(c.assistant.persona, before.assistant.persona);
    }
}
