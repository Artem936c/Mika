//! Global hotkeys at run time: what is registered with Windows now, what each
//! key starts, and the commands of the settings page. The list of actions and
//! the rules for a combination live in `mika_core::hotkeys`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use mika_core::config::Config;
use mika_core::hotkeys::{candidates, hotkey, owner, Combo, HOTKEYS};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::app::Ctx;
use crate::strings::tr;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// The hotkeys come back by themselves this long after the settings page took
/// them away, should the page never give them back.
const PAUSE_LIMIT: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Registry {
    /// What is registered with Windows now.
    bound: HashMap<&'static str, Combo>,
    /// The settings page waits for a new combination: nothing is registered,
    /// so the keys reach the page.
    paused: bool,
    /// Bumped on every pause: the timer of an older pause gives up.
    pause_turn: u64,
    /// Whether each action was switched on when its hotkey was last applied:
    /// `follow` registers anew only what changed since.
    applied: HashMap<&'static str, bool>,
}

impl Registry {
    /// Tells the tools which combinations are registered now: the keys she
    /// presses herself must never be one of them (decision 0030).
    fn publish(&self) {
        let own: Vec<Combo> = self.bound.values().cloned().collect();
        mika_tools::direct::set_own_hotkeys(&own);
    }
}

/// Never held across a call into the plugin: the plugin works on the main
/// thread, and a command running there may be waiting for this lock.
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// The plugin that listens to Windows. Every registration carries its own handler.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new().build()
}

/// Whether the hotkey of an action is asked for from Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gate {
    On,
    /// The action itself is switched off.
    Off,
    /// The action is on, but «Микрофон» or «Экран» it needs is off: the
    /// combination is not taken from other programs for nothing.
    Needs,
}

fn gate(config: &Config, id: &str) -> Gate {
    let (action, master) = match id {
        "dictation" => (config.dictation.enabled, config.privacy.microphone_enabled),
        "screen_ask" => (config.screen_ask.enabled, config.privacy.screen_enabled),
        _ => (true, true),
    };
    match (action, master) {
        (false, _) => Gate::Off,
        (true, false) => Gate::Needs,
        (true, true) => Gate::On,
    }
}

/// Whether the action behind the hotkey is switched on, with what it needs.
fn enabled_in(config: &Config, id: &str) -> bool {
    gate(config, id) == Gate::On
}

fn enabled(ctx: &Ctx, id: &str) -> bool {
    enabled_in(&ctx.config.lock(), id)
}

/// What to try to register for an action, in order: nothing while the action
/// is switched off, the user's combination alone, or the defaults.
fn plan(config: &Config, id: &str) -> Vec<Combo> {
    if !enabled_in(config, id) {
        return Vec::new();
    }
    candidates(id, &config.hotkeys.custom)
}

fn register(app: &AppHandle, id: &'static str, combo: &Combo) -> Result<(), String> {
    app.global_shortcut()
        .on_shortcut(combo.accelerator().as_str(), move |app, _, event| {
            fire(app, id, event.state() == ShortcutState::Pressed)
        })
        .map_err(|e| e.to_string())
}

/// Takes the combination of `id` away from Windows, if one is registered.
fn unbind(app: &AppHandle, id: &str) {
    let old = registry().lock().bound.get(id).cloned();
    if let Some(combo) = old {
        if let Err(e) = app
            .global_shortcut()
            .unregister(combo.accelerator().as_str())
        {
            log::warn!("hotkey `{id}`: cannot release {combo}: {e}");
        }
        // Only now: while Windows still listens, the tools must not press it.
        let mut registry = registry().lock();
        registry.bound.remove(id);
        registry.publish();
    }
}

/// Remembers what Windows now listens to for `id`.
fn bind(id: &'static str, combo: Combo) {
    let mut registry = registry().lock();
    registry.bound.insert(id, combo);
    registry.publish();
}

/// Whether Windows would give us the combination now. Leaves nothing registered.
fn free(app: &AppHandle, combo: &Combo) -> bool {
    let accelerator = combo.accelerator();
    let shortcuts = app.global_shortcut();
    if shortcuts.register(accelerator.as_str()).is_err() {
        return false;
    }
    let _ = shortcuts.unregister(accelerator.as_str());
    true
}

/// Registers the hotkey of one action anew from the settings: the first
/// combination of its plan that Windows gives. While the settings page holds
/// the hotkeys it only releases; the resume registers everything.
pub fn apply(app: &AppHandle, ctx: &Ctx, id: &'static str) {
    unbind(app, id);
    if registry().lock().paused {
        return;
    }
    // Read first: the settings lock is not held across the plugin either.
    let (on, tries) = {
        let config = ctx.config.lock();
        (enabled_in(&config, id), plan(&config, id))
    };
    registry().lock().applied.insert(id, on);
    if !on {
        return;
    }
    for combo in tries {
        match register(app, id, &combo) {
            Ok(()) => {
                log::info!("hotkey `{id}`: {combo} registered");
                bind(id, combo);
                return;
            }
            Err(e) => log::warn!("hotkey `{id}`: {combo} is taken: {e}"),
        }
    }
    log::warn!("hotkey `{id}`: no free combination, the action has no hotkey");
}

/// Registers the hotkey of every action, in the order of the list.
pub fn register_all(app: &AppHandle, ctx: &Ctx) {
    for def in HOTKEYS {
        apply(app, ctx, def.id);
    }
}

/// Gives the hotkeys back after a pause. Returns whether there was one.
fn resume(app: &AppHandle, ctx: &Ctx) -> bool {
    let was = std::mem::replace(&mut registry().lock().paused, false);
    if was {
        register_all(app, ctx);
    }
    was
}

/// The actions whose hotkey is switched on and off by the settings.
const GATED: [&str; 2] = ["dictation", "screen_ask"];

/// The gated actions whose switches changed since their hotkey was applied.
fn stale(ctx: &Ctx) -> Vec<&'static str> {
    let wanted = {
        let config = ctx.config.lock();
        GATED.map(|id| (id, enabled_in(&config, id)))
    };
    let registry = registry().lock();
    wanted
        .into_iter()
        .filter(|(id, on)| registry.applied.get(id).is_some_and(|was| was != on))
        .map(|(id, _)| id)
        .collect()
}

/// The settings were saved: the hotkeys of dictation and of the screen
/// question follow their own switches and «Микрофон» and «Экран», whoever
/// changed them. Callable from any thread; the registrations themselves change
/// on the main thread, like everywhere else, and never under a lock.
pub fn follow(app: &AppHandle) {
    let Some(ctx) = app.try_state::<Arc<Ctx>>().map(|s| s.inner().clone()) else {
        return;
    };
    if stale(&ctx).is_empty() {
        return;
    }
    let on_main = app.clone();
    let _ = app.run_on_main_thread(move || {
        // Looked at again: a command may have applied the change meanwhile.
        for id in stale(&ctx) {
            apply(&on_main, &ctx, id);
        }
    });
}

/// Whether a hotkey event starts something. A release is told only to actions
/// that asked for it. A press that comes with the assistant's own key presses
/// is not the user's: what a hotkey does passes no gate, so her own input (a
/// tool pressing keys while the user holds Ctrl or Alt) must not start it.
fn takes(pressed: bool, on_release: bool, own_keys_echo: bool) -> bool {
    if pressed {
        !own_keys_echo
    } else {
        on_release
    }
}

/// A hotkey went down or up.
fn fire(app: &AppHandle, id: &'static str, pressed: bool) {
    let Some(def) = hotkey(id) else {
        return;
    };
    let echo = pressed && mika_tools::direct::keys_pressed_recently();
    if !takes(pressed, def.on_release, echo) {
        if echo {
            log::info!("hotkey `{id}` ignored: it came with her own key presses");
        }
        return;
    }
    let ctx = || app.state::<Arc<Ctx>>().inner().clone();
    match id {
        "chat" => crate::tray::open_chat(app),
        "listen" => {
            crate::window::summon(app);
            let ctx = ctx();
            tauri::async_runtime::spawn(async move { ctx.listen_and_submit().await });
        }
        "selection" => crate::selection::on_shortcut(app, ctx()),
        "dictation" => crate::hotkey_actions::on_dictation(app, &ctx(), pressed),
        "screen_ask" => crate::hotkey_actions::on_screen_ask(app, ctx()),
        other => log::warn!("hotkey `{other}` has no action"),
    }
}

/// The combination registered for an action, or a note that none was free.
pub fn label(id: &str) -> String {
    registry()
        .lock()
        .bound
        .get(id)
        .map(Combo::to_string)
        .unwrap_or_else(|| tr("tray.shortcut_none"))
}

/// One row of the settings page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HotkeyView {
    id: &'static str,
    /// The registered combination, else the first one that would be tried.
    combo: String,
    /// The user chose the combination.
    custom: bool,
    /// `on`, `off`, `needs` or `taken`: see `status`.
    status: &'static str,
}

/// The settings page: every hotkey, the switches of the two actions and what they need.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HotkeysView {
    hotkeys: Vec<HotkeyView>,
    /// The switch of dictation.
    dictation: bool,
    /// The switch of the question about the screen.
    screen_ask: bool,
    /// `privacy.microphone_enabled`.
    microphone: bool,
    /// `privacy.screen_enabled`.
    screen: bool,
    /// A speech recognition model is set.
    stt_model: bool,
}

/// `off`: the action is switched off. `needs`: it is on, but «Микрофон» or
/// «Экран» is off, so its hotkey is not registered. `on`: its hotkey is
/// registered. `taken`: the action is on, but Windows gave none of its
/// combinations. During a pause nothing is registered on purpose, so nothing
/// counts as taken.
fn status(gate: Gate, bound: bool, paused: bool) -> &'static str {
    match gate {
        Gate::Off => "off",
        Gate::Needs => "needs",
        Gate::On if bound || paused => "on",
        Gate::On => "taken",
    }
}

fn rows(config: &Config, bound: &HashMap<&'static str, Combo>, paused: bool) -> Vec<HotkeyView> {
    let custom: &BTreeMap<String, String> = &config.hotkeys.custom;
    HOTKEYS
        .iter()
        .map(|def| {
            let registered = bound.get(def.id);
            HotkeyView {
                id: def.id,
                combo: registered
                    .cloned()
                    .or_else(|| candidates(def.id, custom).into_iter().next())
                    .map(|combo| combo.to_string())
                    .unwrap_or_default(),
                custom: custom.contains_key(def.id),
                status: status(gate(config, def.id), registered.is_some(), paused),
            }
        })
        .collect()
}

fn view_of(config: &Config, bound: &HashMap<&'static str, Combo>, paused: bool) -> HotkeysView {
    HotkeysView {
        hotkeys: rows(config, bound, paused),
        dictation: config.dictation.enabled,
        screen_ask: config.screen_ask.enabled,
        microphone: config.privacy.microphone_enabled,
        screen: config.privacy.screen_enabled,
        stt_model: !config.voice.stt_model.trim().is_empty(),
    }
}

pub fn view(ctx: &Ctx) -> HotkeysView {
    let (bound, paused) = {
        let registry = registry().lock();
        (registry.bound.clone(), registry.paused)
    };
    view_of(&ctx.config.lock(), &bound, paused)
}

#[tauri::command]
pub fn hotkeys_get(ctx: CtxState<'_>) -> HotkeysView {
    view(&ctx)
}

/// The user pressed a new combination for an action. The settings are changed
/// only after Windows gave the combination.
#[tauri::command]
pub fn hotkeys_set(
    app: AppHandle,
    ctx: CtxState<'_>,
    action: String,
    combo: String,
) -> Result<HotkeysView, String> {
    let id = hotkey(&action).ok_or("unknown hotkey action")?.id;
    let new =
        mika_core::hotkeys::combo(&combo).map_err(|e| tr(&format!("hotkeys.err_{}", e.code())))?;
    let custom = ctx.config.lock().hotkeys.custom.clone();
    if let Some(other) = owner(&new, &custom, id) {
        return Err(
            tr("hotkeys.err_same").replace("{action}", &tr(&format!("hotkeys.action.{other}")))
        );
    }
    // The page paused the hotkeys to hear the keys: back to what the settings say.
    resume(&app, &ctx);
    let old = registry().lock().bound.get(id).cloned();
    if old.as_ref() != Some(&new) {
        let taken = || tr("hotkeys.err_taken").replace("{combo}", &new.to_string());
        if enabled(&ctx, id) {
            unbind(&app, id);
            if let Err(e) = register(&app, id, &new) {
                log::warn!("hotkey `{id}`: {new} is taken: {e}");
                match old {
                    Some(old) if register(&app, id, &old).is_ok() => bind(id, old),
                    // The settings are unchanged: whatever they give.
                    _ => apply(&app, &ctx, id),
                }
                return Err(taken());
            }
            log::info!("hotkey `{id}`: {new} registered");
            bind(id, new.clone());
        } else if !free(&app, &new) {
            return Err(taken());
        }
    }
    ctx.config
        .lock()
        .hotkeys
        .custom
        .insert(id.to_string(), new.to_string());
    ctx.save_config()?;
    Ok(view(&ctx))
}

/// Back to the default combinations of an action.
#[tauri::command]
pub fn hotkeys_reset(
    app: AppHandle,
    ctx: CtxState<'_>,
    action: String,
) -> Result<HotkeysView, String> {
    let id = hotkey(&action).ok_or("unknown hotkey action")?.id;
    ctx.config.lock().hotkeys.custom.remove(id);
    let saved = ctx.save_config();
    // A resume registers every action from the settings, this one too.
    if !resume(&app, &ctx) {
        apply(&app, &ctx, id);
    }
    saved?;
    Ok(view(&ctx))
}

/// The settings page takes the hotkeys away while the user presses a new
/// combination, so that the keys reach the page, and gives them back after.
#[tauri::command]
pub fn hotkeys_pause(app: AppHandle, ctx: CtxState<'_>, paused: bool) {
    if !paused {
        resume(&app, &ctx);
        return;
    }
    let turn = {
        let mut registry = registry().lock();
        registry.paused = true;
        registry.pause_turn += 1;
        registry.pause_turn
    };
    for def in HOTKEYS {
        unbind(&app, def.id);
    }
    // A page that went away without a word must not leave her without hotkeys.
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(PAUSE_LIMIT).await;
        let on_main = app.clone();
        // On the main thread, where the commands change the registrations too.
        let _ = app.run_on_main_thread(move || {
            // Its own statement: the lock must be free again before `resume`.
            let same_pause = registry().lock().pause_turn == turn;
            if same_pause && resume(&on_main, &ctx) {
                log::info!("hotkeys are back: the settings page held them too long");
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(text: &str) -> Combo {
        mika_core::hotkeys::combo(text).unwrap()
    }

    fn texts(plan: &[Combo]) -> Vec<String> {
        plan.iter().map(Combo::to_string).collect()
    }

    #[test]
    fn status_tells_off_on_and_taken() {
        assert_eq!(status(Gate::Off, false, false), "off");
        // A switched-off action is off even if something is still registered.
        assert_eq!(status(Gate::Off, true, false), "off");
        assert_eq!(status(Gate::On, true, false), "on");
        assert_eq!(status(Gate::On, false, false), "taken");
        // The page itself took the hotkeys away: none of them is taken.
        assert_eq!(status(Gate::On, false, true), "on");
        assert_eq!(status(Gate::Off, false, true), "off");
        // Waiting for «Микрофон» or «Экран» is neither off nor taken.
        assert_eq!(status(Gate::Needs, false, false), "needs");
        assert_eq!(status(Gate::Needs, false, true), "needs");
    }

    /// The action switches on, with the master switches as given.
    fn config_with(microphone: bool, screen: bool) -> Config {
        let mut config = Config::default();
        config.dictation.enabled = true;
        config.screen_ask.enabled = true;
        config.privacy.microphone_enabled = microphone;
        config.privacy.screen_enabled = screen;
        config
    }

    #[test]
    fn a_hotkey_is_asked_for_only_with_both_switches_on() {
        let off = Config::default();
        assert_eq!(gate(&off, "dictation"), Gate::Off);
        assert_eq!(gate(&off, "screen_ask"), Gate::Off);
        // The other actions have no switch.
        for id in ["chat", "listen", "selection"] {
            assert_eq!(gate(&off, id), Gate::On);
        }

        // The action is on, «Микрофон» and «Экран» are not: nothing is taken
        // from other programs, and the page says what the hotkey waits for.
        let waits = config_with(false, false);
        assert_eq!(gate(&waits, "dictation"), Gate::Needs);
        assert_eq!(gate(&waits, "screen_ask"), Gate::Needs);
        assert!(plan(&waits, "dictation").is_empty());
        assert!(plan(&waits, "screen_ask").is_empty());
        let rows = rows(&waits, &HashMap::new(), false);
        let status_of = |id: &str| rows.iter().find(|r| r.id == id).unwrap().status;
        assert_eq!(status_of("dictation"), "needs");
        assert_eq!(status_of("screen_ask"), "needs");

        // Each master switch opens its own action only.
        let mic = config_with(true, false);
        assert_eq!(gate(&mic, "dictation"), Gate::On);
        assert_eq!(gate(&mic, "screen_ask"), Gate::Needs);
        assert!(!plan(&mic, "dictation").is_empty());
        let screen = config_with(false, true);
        assert_eq!(gate(&screen, "dictation"), Gate::Needs);
        assert_eq!(gate(&screen, "screen_ask"), Gate::On);

        // The master switch alone, with the action off, asks for nothing.
        let mut only_master = Config::default();
        only_master.privacy.microphone_enabled = true;
        only_master.privacy.screen_enabled = true;
        assert_eq!(gate(&only_master, "dictation"), Gate::Off);
        assert_eq!(gate(&only_master, "screen_ask"), Gate::Off);
    }

    #[test]
    fn a_press_that_comes_with_her_own_keys_starts_nothing() {
        // The user's press and the release an action asked for.
        assert!(takes(true, false, false));
        assert!(takes(true, true, false));
        assert!(takes(false, true, false));
        assert!(!takes(false, false, false));
        // A press right after keys she pressed herself, for every action.
        assert!(!takes(true, false, true));
        assert!(!takes(true, true, true));
        // A release can only end a dictation: it is never held back.
        assert!(takes(false, true, true));
    }

    #[test]
    fn plan_is_empty_while_the_action_is_switched_off() {
        let config = Config::default();
        assert!(plan(&config, "dictation").is_empty());
        assert!(plan(&config, "screen_ask").is_empty());
        assert_eq!(
            texts(&plan(&config, "chat")),
            ["Ctrl+Alt+M", "Ctrl+Shift+M"]
        );
        assert_eq!(
            texts(&plan(&config, "listen")),
            ["Ctrl+Alt+K", "Ctrl+Shift+K"]
        );
        assert_eq!(
            texts(&plan(&config, "selection")),
            ["Ctrl+Alt+X", "Ctrl+Shift+X"]
        );
        assert!(plan(&config, "no_such_action").is_empty());
    }

    #[test]
    fn plan_follows_the_switches_and_the_users_choice() {
        let mut config = config_with(true, true);
        assert_eq!(
            texts(&plan(&config, "dictation")),
            ["Ctrl+Alt+D", "Ctrl+Alt+Shift+D"]
        );
        assert_eq!(
            texts(&plan(&config, "screen_ask")),
            ["Ctrl+Alt+Q", "Ctrl+Alt+Shift+Q"]
        );
        // The user's combination alone: no default is tried behind their back.
        config
            .hotkeys
            .custom
            .insert("dictation".into(), "Win+Shift+J".into());
        assert_eq!(texts(&plan(&config, "dictation")), ["Shift+Win+J"]);
        // An entry broken by hand falls back to the defaults.
        config
            .hotkeys
            .custom
            .insert("chat".into(), "Ctrl+Escape".into());
        assert_eq!(
            texts(&plan(&config, "chat")),
            ["Ctrl+Alt+M", "Ctrl+Shift+M"]
        );
        // The choice is kept while the action is off, and waits for the switch.
        config.dictation.enabled = false;
        assert!(plan(&config, "dictation").is_empty());
    }

    #[test]
    fn rows_show_what_is_registered_and_what_would_be_tried() {
        let mut config = Config::default();
        config.screen_ask.enabled = true;
        config.privacy.screen_enabled = true;
        config
            .hotkeys
            .custom
            .insert("listen".into(), "Ctrl+Alt+L".into());
        let mut bound = HashMap::new();
        // The first default of `chat` was taken: the second one is registered.
        bound.insert("chat", combo("Ctrl+Shift+M"));
        bound.insert("listen", combo("Ctrl+Alt+L"));
        let rows = rows(&config, &bound, false);
        let row = |id: &str| rows.iter().find(|r| r.id == id).unwrap().clone();

        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            ["chat", "listen", "selection", "dictation", "screen_ask"]
        );
        assert_eq!(
            row("chat"),
            HotkeyView {
                id: "chat",
                combo: "Ctrl+Shift+M".into(),
                custom: false,
                status: "on"
            }
        );
        assert_eq!(
            row("listen"),
            HotkeyView {
                id: "listen",
                combo: "Ctrl+Alt+L".into(),
                custom: true,
                status: "on"
            }
        );
        // On, but Windows gave nothing: the first candidate is shown as taken.
        assert_eq!(
            row("selection"),
            HotkeyView {
                id: "selection",
                combo: "Ctrl+Alt+X".into(),
                custom: false,
                status: "taken"
            }
        );
        assert_eq!(
            row("dictation"),
            HotkeyView {
                id: "dictation",
                combo: "Ctrl+Alt+D".into(),
                custom: false,
                status: "off"
            }
        );
        assert_eq!(row("screen_ask").status, "taken");
        assert_eq!(row("screen_ask").combo, "Ctrl+Alt+Q");
    }

    #[test]
    fn rows_during_a_pause_call_nothing_taken() {
        let mut config = Config::default();
        config.dictation.enabled = true;
        config.privacy.microphone_enabled = true;
        let rows = rows(&config, &HashMap::new(), true);
        let statuses: Vec<_> = rows.iter().map(|r| (r.id, r.status)).collect();
        assert_eq!(
            statuses,
            [
                ("chat", "on"),
                ("listen", "on"),
                ("selection", "on"),
                ("dictation", "on"),
                ("screen_ask", "off")
            ]
        );
    }

    #[test]
    fn view_carries_the_switches_and_what_they_need() {
        let mut config = Config::default();
        let view = view_of(&config, &HashMap::new(), false);
        assert!(!view.dictation && !view.screen_ask);
        assert!(!view.microphone && !view.screen);

        config.dictation.enabled = true;
        config.privacy.microphone_enabled = true;
        config.privacy.screen_enabled = true;
        config.voice.stt_model = "  ".into();
        let view = view_of(&config, &HashMap::new(), false);
        assert!(view.dictation && !view.screen_ask);
        assert!(view.microphone && view.screen);
        assert!(!view.stt_model, "a blank model name is no model");
        config.voice.stt_model = "some/asr".into();
        assert!(view_of(&config, &HashMap::new(), false).stt_model);
    }

    #[test]
    fn view_has_the_shape_the_page_reads() {
        let mut bound = HashMap::new();
        bound.insert("chat", combo("Ctrl+Alt+M"));
        let json = serde_json::to_value(view_of(&Config::default(), &bound, false)).unwrap();
        for key in [
            "hotkeys",
            "dictation",
            "screen_ask",
            "microphone",
            "screen",
            "stt_model",
        ] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(
            json["hotkeys"][0],
            serde_json::json!({ "id": "chat", "combo": "Ctrl+Alt+M", "custom": false, "status": "on" })
        );
    }

    #[test]
    fn every_action_has_a_name_and_every_refusal_a_text() {
        for def in HOTKEYS {
            let key = format!("hotkeys.action.{}", def.id);
            assert_ne!(tr(&key), key, "{key} is missing in ru.json");
        }
        use mika_core::hotkeys::ComboError;
        for e in [
            ComboError::NoKey,
            ComboError::UnknownKey("Escape".into()),
            ComboError::Modifier,
            ComboError::Reserved,
        ] {
            let key = format!("hotkeys.err_{}", e.code());
            assert_ne!(tr(&key), key, "{key} is missing in ru.json");
        }
        assert!(tr("hotkeys.err_same").contains("{action}"));
        assert!(tr("hotkeys.err_taken").contains("{combo}"));
    }

    /// `Combo::accelerator` must be something the plugin can read, or the
    /// action silently gets no hotkey.
    #[test]
    fn the_plugin_reads_every_key_a_combination_can_have() {
        use std::str::FromStr;
        use tauri_plugin_global_shortcut::Shortcut;

        let mut keys: Vec<String> = Vec::new();
        keys.extend(('A'..='Z').map(String::from));
        keys.extend(('0'..='9').map(String::from));
        keys.extend((1..=24).map(|n| format!("F{n}")));
        keys.extend((0..=9).map(|n| format!("Numpad{n}")));
        keys.extend(
            [
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
            ]
            .map(String::from),
        );
        for key in keys {
            let combo = Combo::parse(&format!("Ctrl+Alt+Shift+Win+{key}")).unwrap();
            assert!(
                Shortcut::from_str(&combo.accelerator()).is_ok(),
                "{}",
                combo.accelerator()
            );
            let bare = Combo::parse(&key).unwrap();
            assert!(Shortcut::from_str(&bare.accelerator()).is_ok(), "{key}");
        }
        for def in HOTKEYS {
            for text in def.defaults {
                let accelerator = combo(text).accelerator();
                assert!(Shortcut::from_str(&accelerator).is_ok(), "{accelerator}");
            }
        }
    }

    /// Two combinations that differ for us must differ for Windows too, and
    /// the modifiers must arrive: `Win` is `Super` for the plugin.
    #[test]
    fn the_plugin_sees_the_modifiers_we_wrote() {
        use std::str::FromStr;
        use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

        let shortcut = Shortcut::from_str(&combo("Win+Shift+S").accelerator()).unwrap();
        assert_eq!(shortcut.mods, Modifiers::SUPER | Modifiers::SHIFT);
        assert_eq!(shortcut.key, Code::KeyS);
        let shortcut = Shortcut::from_str(&combo("Ctrl+Alt+D").accelerator()).unwrap();
        assert_eq!(shortcut.mods, Modifiers::CONTROL | Modifiers::ALT);
        assert_eq!(shortcut.key, Code::KeyD);
        let shortcut = Shortcut::from_str(&combo("Ctrl+Alt+Up").accelerator()).unwrap();
        assert_eq!(shortcut.key, Code::ArrowUp);
    }
}
