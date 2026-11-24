//! The companion in games (decision 0054): a loop beside the rhythm of the
//! day. While a game fills the screen she looks at it every `every_s` seconds
//! and sometimes says a short line.
//!
//! What may happen is decided by `mika_core::companion`; here are the steps
//! that need the application: the picture of the window in front (the blind
//! zone, her own window and «Экран» are checked before it), the vision model,
//! the face of the avatar and the line in the chat and aloud.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mika_core::companion::{self as core, Hold, Said, Scene};
use mika_core::i18n::Lang;
use mika_core::journal;
use mika_core::tools::RiskLevel;
use mika_core::Event;

use crate::app::Ctx;
use crate::rhythm::speaks;

/// How often the loop asks whether to look. Cheap: a few switches.
const TICK: Duration = Duration::from_secs(2);
/// The longest wait for the vision model: a later line is about a moment gone.
const ASK_TIMEOUT: Duration = Duration::from_secs(30);

/// What the loop remembers between ticks.
#[derive(Default)]
struct Watch {
    /// When she looked last, unix seconds.
    last_look: Option<i64>,
    said: Said,
    /// Why she did not look at the last tick: written once per change.
    held: Option<Hold>,
    /// The program she looks at; another one is a new game.
    program: Option<String>,
}

impl Ctx {
    /// Starts the loop of the companion. It does nothing while the switch is off.
    pub fn start_companion(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tauri::async_runtime::spawn(async move {
            let mut w = Watch::default();
            loop {
                tokio::time::sleep(TICK).await;
                let Some(ctx) = me.upgrade() else { break };
                ctx.companion_tick(&mut w).await;
            }
        });
    }

    /// What is going on around her now. Without a game nothing else is
    /// asked: the loop runs every two seconds.
    fn companion_scene(&self) -> Scene {
        if !crate::window::game_in_front() {
            return Scene::default();
        }
        let at = chrono::Local::now().timestamp();
        // A call, a remote desktop, a browser or a player on the whole
        // screen hides the island as a game does, but she never looks at it.
        let not_game = self
            .tool_env()
            .windows
            .foreground()
            .is_some_and(|w| core::not_a_game(&w.process));
        // Read on its own: a guard inside the literal would live through
        // every other check below.
        let screen = self.config.lock().privacy.screen_enabled;
        Scene {
            game: true,
            not_game,
            screen,
            paused: self.session.pause().is_on(),
            focus: self.tool_env().shared.focus.active(at),
            busy: self.engaged(),
            heard: speaks(&self.speaker),
        }
    }

    async fn companion_tick(self: &Arc<Self>, w: &mut Watch) {
        let cfg = self.config.lock().companion.clone();
        if !cfg.enabled {
            w.held = Some(Hold::Off);
            w.program = None;
            return;
        }
        let scene = self.companion_scene();
        let now = chrono::Local::now().timestamp();
        let said = w.said.last_hour(now);
        // The spending is read only when everything else allows a look; the
        // first refusal of a period tells the user (decision 0028).
        let look = core::should_look(
            scene,
            now,
            w.last_look,
            said,
            &cfg,
            mika_core::costs::unattended_allowed,
        );
        if let Err(hold) = look {
            match hold {
                Hold::NoGame | Hold::NotGame => w.program = None,
                // The spending is read again only after the usual interval.
                Hold::Budget => w.last_look = Some(now),
                _ => {}
            }
            if w.held != Some(hold) && hold != Hold::TooSoon {
                log::debug!(target: "vision", "companion waits: reason={}", hold.as_str());
            }
            w.held = Some(hold);
            return;
        }
        w.held = None;
        w.last_look = Some(now);
        self.companion_look(w).await;
    }

    /// One look: the picture, the model, maybe a line.
    async fn companion_look(self: &Arc<Self>, w: &mut Watch) {
        let started = Instant::now();
        // Every refusal comes before the picture: «Экран», her own window,
        // the blind zone by the title of the window in front.
        let front = match self.direct().capture_front_at(core::CAPTURE_SIDE).await {
            Ok(front) => front,
            Err(e) => {
                log::info!(
                    target: "vision",
                    "companion did not look: reason={} error={}",
                    e.code(),
                    journal::error_text(&e.why())
                );
                return;
            }
        };
        let program = front.window.process.clone();
        // The window in front may have changed since the scene was read:
        // the picture of a program that is no game is dropped unsent.
        if core::not_a_game(&program) {
            log::info!(
                target: "vision",
                "companion did not look: reason={}",
                Hold::NotGame.as_str()
            );
            return;
        }
        if w.program.as_deref() != Some(program.as_str()) {
            // A new game: her old lines are of no use, and the actions list
            // gets one row per game, not one per look.
            w.said.forget_lines();
            w.program = Some(program.clone());
            self.audit.record(
                "companion",
                &format!("игра {program}"),
                RiskLevel::Medium,
                "direct",
                "companion",
            );
        }
        let lang = mika_core::i18n::lang();
        let (name, persona) = {
            let c = self.config.lock();
            let persona = match lang {
                Lang::Ru => c.assistant.persona_prompt(),
                Lang::En => mika_core::i18n::prompt::persona_en(&c.assistant),
            };
            (c.assistant.name.clone(), persona)
        };
        let system = core::system_prompt(lang, &name, &persona);
        let question = core::question(lang, &program, &w.said.recent());
        let vision = self.tool_env().shared.vision.clone();
        let asked = tokio::time::timeout(
            ASK_TIMEOUT,
            vision.ask_as(&system, &front.shot.png, &question),
        )
        .await;
        drop(front);
        let ms = started.elapsed().as_millis();
        let answer = match asked {
            Ok(Ok(answer)) => answer,
            Ok(Err(why)) => {
                log::warn!(
                    target: "vision",
                    "companion look failed: program={program} ms={ms} error={}",
                    journal::error_text(&why)
                );
                return;
            }
            Err(_) => {
                log::warn!(target: "vision", "companion look timed out: program={program} ms={ms}");
                return;
            }
        };
        let Some(line) = core::parse_line(&answer) else {
            log::info!(target: "vision", "companion looked: outcome=skip program={program} ms={ms}");
            return;
        };
        // The answer took seconds: the line is said only if nothing that
        // keeps her quiet came meanwhile (a stop, a request, the game left).
        let cfg = self.config.lock().companion.clone();
        if let Some(hold) = core::held(self.companion_scene(), &cfg) {
            log::info!(
                target: "vision",
                "companion line dropped: reason={} program={program} ms={ms}",
                hold.as_str()
            );
            return;
        }
        w.said.push(chrono::Local::now().timestamp(), &line.text);
        log::info!(
            target: "vision",
            "companion looked: outcome=line program={program} ms={ms} emotion={} line={}",
            line.emotion.map_or("none", |e| e.as_str()),
            journal::content(&line.text)
        );
        if let Some(emotion) = line.emotion {
            self.bus.publish(Event::EmotionCue { emotion });
        }
        self.remark(&line.text, core::REMARK_COMPANION, true);
    }
}
