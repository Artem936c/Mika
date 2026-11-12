//! Looking at the screen: a question about a screenshot, text from it, saving it.
//! Nothing here runs until the user turns the "Screen" switch on.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::blind::{is_blind_title, BLIND_REFUSAL};
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::sys::{Screen, Shot, Vision, WindowInfo};
use crate::ui::UiEnv;
use crate::util::{failed, truncate};

/// Longer side of a picture sent to the vision model, pixels.
const VISION_MAX_SIDE: u32 = 1600;

pub const SCREEN_DISABLED: &str =
    "Просмотр экрана выключен. Его включает пользователь переключателем «Экран» в шапке чата.";

#[derive(Clone)]
pub struct ScreenEnv {
    pub ui: UiEnv,
    pub screen: Arc<dyn Screen>,
    pub enabled: Arc<AtomicBool>,
    pub user_profile: String,
}

/// What a tool looks at: one window, or the whole screen.
struct View {
    window: Option<WindowInfo>,
}

impl View {
    fn label(&self) -> String {
        match &self.window {
            Some(w) => format!("окно «{}»", truncate(&w.title, 60)),
            None => "весь экран".into(),
        }
    }

    fn scope(&self) -> String {
        match &self.window {
            Some(w) if !w.process.is_empty() => w.process.clone(),
            _ => "экран".into(),
        }
    }
}

impl ScreenEnv {
    fn ensure_enabled(&self) -> Result<(), ToolError> {
        if self.enabled.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(failed(SCREEN_DISABLED))
        }
    }

    /// The named window, or the whole screen when `window` is absent.
    /// For the whole screen the active window decides about the blind zone.
    fn view(&self, args: &Value) -> Result<View, ToolError> {
        let named = arg_str_opt(args, "window").is_some_and(|w| !w.trim().is_empty());
        if named {
            return Ok(View {
                window: Some(self.ui.target(args)?),
            });
        }
        if let Some(front) = self.ui.windows.foreground() {
            if is_blind_title(&front.title) {
                return Err(failed(BLIND_REFUSAL));
            }
        }
        Ok(View { window: None })
    }

    async fn capture(&self, view: &View, max_side: u32) -> Result<Shot, ToolError> {
        let (screen, id) = (self.screen.clone(), view.window.as_ref().map(|w| w.id));
        tokio::task::spawn_blocking(move || screen.capture(id, max_side))
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(failed)
    }
}

fn window_prop() -> Value {
    json!({
        "type": "string",
        "description": "Часть заголовка окна или имя программы; без него — весь экран"
    })
}

pub struct ScreenLookTool {
    pub env: ScreenEnv,
    pub vision: Arc<dyn Vision>,
}

#[async_trait]
impl Tool for ScreenLookTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "screen_look",
            "Посмотреть на экран или окно и ответить на вопрос о том, что там видно. Для игр и окон, которые не читаются через ui_read. Чтобы потом нажать в точку, попроси в вопросе координаты x, y в пикселях снимка.",
            json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "Что нужно узнать о картинке" },
                    "window": window_prop()
                },
                "required": ["question"]
            }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        arg_str(args, "question")?;
        self.env.ensure_enabled()?;
        let view = self.env.view(args)?;
        Ok(Assessment::new(
            RiskLevel::Medium,
            "Посмотреть на экран",
            view.label(),
            "снимок уйдёт провайдеру модели со зрением",
        )
        .with_scope(view.scope()))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        self.env.ensure_enabled()?;
        let view = self.env.view(&args)?;
        let question = arg_str(&args, "question")?.to_string();
        let shot = self.env.capture(&view, VISION_MAX_SIDE).await?;
        let asked = format!(
            "{question}\n\nСнимок {}×{} пикселей. Координаты называй в пикселях этого снимка от левого верхнего угла.",
            shot.width, shot.height
        );
        let answer = tokio::select! {
            _ = cancel.cancelled() => return Err(ToolError::Cancelled),
            a = self.vision.ask(&shot.png, &asked) => a.map_err(failed)?,
        };
        let scale = if shot.factor > 1.01 {
            format!(
                " Снимок уменьшен: для ui_click координаты со снимка умножай на {:.2}.",
                shot.factor
            )
        } else {
            String::new()
        };
        Ok(ToolOutput::new(format!(
            "На снимке ({}, {}×{}): {}{scale}",
            view.label(),
            shot.width,
            shot.height,
            answer.trim()
        )))
    }
}

pub struct ScreenTextTool(pub ScreenEnv);

#[async_trait]
impl Tool for ScreenTextTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "screen_text",
            "Прочитать текст с экрана или окна распознаванием картинки. Работает на этом компьютере, снимок никуда не уходит.",
            json!({ "type": "object", "properties": { "window": window_prop() } }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        self.0.ensure_enabled()?;
        let view = self.0.view(args)?;
        Ok(Assessment::new(
            RiskLevel::Medium,
            "Прочитать текст с экрана",
            view.label(),
            "снимок остаётся на компьютере, текст с него получит модель",
        )
        .with_scope(view.scope()))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        self.0.ensure_enabled()?;
        let view = self.0.view(&args)?;
        let shot = self.0.capture(&view, 0).await?;
        let screen = self.0.screen.clone();
        let text = tokio::task::spawn_blocking(move || screen.ocr(&shot.png))
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(failed)?;
        if text.trim().is_empty() {
            return Ok(ToolOutput::new("Текста на снимке не нашла."));
        }
        Ok(ToolOutput::new(truncate(text.trim(), 8000)))
    }
}

pub struct ScreenshotTool(pub ScreenEnv);

impl ScreenshotTool {
    fn folder(&self) -> PathBuf {
        PathBuf::from(&self.0.user_profile)
            .join("Pictures")
            .join("Screenshots")
    }
}

#[async_trait]
impl Tool for ScreenshotTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "screenshot_save",
            "Сделать снимок экрана или окна и сохранить его в «Изображения\\Screenshots».",
            json!({ "type": "object", "properties": { "window": window_prop() } }),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        self.0.ensure_enabled()?;
        let view = self.0.view(args)?;
        Ok(Assessment::new(
            RiskLevel::Medium,
            "Сохранить снимок экрана",
            format!("{} → {}", view.label(), self.folder().display()),
            "создаст файл с тем, что сейчас на экране",
        )
        .with_scope(view.scope()))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        self.0.ensure_enabled()?;
        let view = self.0.view(&args)?;
        let png = self.0.capture(&view, 0).await?.png;
        let folder = self.folder();
        tokio::fs::create_dir_all(&folder)
            .await
            .map_err(|e| failed(e.to_string()))?;
        let name = format!("Mika {}.png", mika_core::storage::now().replace(':', "-"));
        let path = folder.join(name);
        tokio::fs::write(&path, &png)
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(
            ToolOutput::new(format!("Снимок сохранён: {}", path.display()))
                .with_data(json!({ "kind": "image", "path": path.display().to_string() })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env_with, run, Fakes};
    use mika_core::storage::Db;

    fn screen_env(profile: &str) -> (ScreenEnv, Arc<dyn Vision>, Arc<Fakes>) {
        let (e, f) = env_with(Arc::new(Db::open_in_memory().unwrap()), profile);
        *f.windows.lock() = vec![
            WindowInfo {
                id: 7,
                title: "Игра".into(),
                process: "game.exe".into(),
            },
            WindowInfo {
                id: 9,
                title: "Оплата заказа".into(),
                process: "chrome.exe".into(),
            },
        ];
        e.shared.screen_enabled.store(true, Ordering::Relaxed);
        let env = ScreenEnv {
            ui: UiEnv {
                windows: e.windows.clone(),
                ui: e.ui.clone(),
                input: e.input.clone(),
                bus: e.shared.bus.clone(),
                own_process: "mika.exe".into(),
                seen: Default::default(),
            },
            screen: e.screen.clone(),
            enabled: e.shared.screen_enabled.clone(),
            user_profile: profile.into(),
        };
        (env, e.shared.vision.clone(), f)
    }

    #[tokio::test]
    async fn switched_off_screen_takes_no_picture() {
        let (env, vision, f) = screen_env("C:\\Users\\me");
        env.enabled.store(false, Ordering::Relaxed);
        let look = ScreenLookTool {
            env: env.clone(),
            vision,
        };
        for err in [
            look.assess(&json!({"question": "что тут?"})).unwrap_err(),
            ScreenTextTool(env.clone()).assess(&json!({})).unwrap_err(),
            ScreenshotTool(env.clone()).assess(&json!({})).unwrap_err(),
            run(&look, json!({"question": "что тут?"}))
                .await
                .unwrap_err(),
        ] {
            assert!(err.to_string().contains("выключен"), "{err}");
        }
        assert!(f.captures.lock().is_empty());
    }

    #[tokio::test]
    async fn look_sends_the_window_picture_to_the_vision_model() {
        let (env, vision, f) = screen_env("C:\\Users\\me");
        let look = ScreenLookTool { env, vision };
        let a = look
            .assess(&json!({"question": "где кнопка?", "window": "игра"}))
            .unwrap();
        assert_eq!((a.level, a.scope.as_str()), (RiskLevel::Medium, "game.exe"));
        let out = run(&look, json!({"question": "где кнопка?", "window": "игра"}))
            .await
            .unwrap()
            .text;
        assert_eq!(
            out,
            "На снимке (окно «Игра», 1600×900): вижу 4 байт, вопрос: где кнопка?\n\n\
             Снимок 1600×900 пикселей. Координаты называй в пикселях этого снимка от левого верхнего угла. \
             Снимок уменьшен: для ui_click координаты со снимка умножай на 2.00."
        );
        assert_eq!(*f.captures.lock(), vec![Some(7)]);
    }

    #[tokio::test]
    async fn blind_zone_blocks_window_and_whole_screen() {
        let (env, vision, f) = screen_env("C:\\Users\\me");
        let look = ScreenLookTool {
            env: env.clone(),
            vision,
        };
        assert!(look
            .assess(&json!({"question": "?", "window": "оплата"}))
            .unwrap_err()
            .to_string()
            .contains("слепой зоне"));
        *f.foreground.lock() = Some(f.windows.lock()[1].clone());
        assert!(ScreenTextTool(env).assess(&json!({})).is_err());
        assert!(f.captures.lock().is_empty());
    }

    #[tokio::test]
    async fn text_and_screenshot() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().to_string_lossy().into_owned();
        let (env, _, f) = screen_env(&profile);
        assert_eq!(
            run(&ScreenTextTool(env.clone()), json!({}))
                .await
                .unwrap()
                .text,
            "текст с экрана"
        );
        assert_eq!(*f.captures.lock(), vec![None]);
        let out = run(&ScreenshotTool(env), json!({"window": "game"}))
            .await
            .unwrap();
        let path = out.data.unwrap()["path"].as_str().unwrap().to_string();
        assert!(path.contains("Screenshots") && path.ends_with(".png"));
        assert_eq!(std::fs::read(path).unwrap(), b"\x89PNG");
    }
}
