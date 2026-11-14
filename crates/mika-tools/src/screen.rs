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

use crate::sys::{MouseButton, Screen, Shot, Vision, WindowInfo};
use crate::ui::props;
use crate::ui::UiEnv;
use crate::util::{failed, truncate};

/// Longer side of a picture sent to the vision model, pixels.
pub(crate) const VISION_MAX_SIDE: u32 = 1600;

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

    /// A window brought to the front, as the screen shows it.
    async fn capture_visible(&self, window: u64, max_side: u32) -> Result<Shot, ToolError> {
        let screen = self.screen.clone();
        tokio::task::spawn_blocking(move || screen.capture_visible(window, max_side))
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(failed)
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

/// Grid of the first look for a vision model, and of the look inside its cell.
const GRID: (u32, u32) = (8, 6);
const FINE_GRID: (u32, u32) = (4, 4);

/// Clicks a label or a described thing in a window by its picture: for
/// programs UI Automation cannot read (Telegram, games, Electron apps).
pub struct ScreenClickTool {
    pub env: ScreenEnv,
    pub vision: Arc<dyn Vision>,
}

impl ScreenClickTool {
    /// Asks the vision model for a cell of a numbered grid over `pic`.
    async fn ask_cell(
        &self,
        pic: &crate::grid::Picture,
        (cols, rows): (u32, u32),
        target: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<crate::grid::Area>, ToolError> {
        let drawn = crate::grid::draw_grid(pic, cols, rows);
        let png = crate::grid::encode(&drawn).ok_or_else(|| failed("снимок не собрался"))?;
        let question = format!(
            "На снимке сетка, у каждой клетки номер в левом верхнем углу (от 1 до {}). \
В какой клетке находится середина «{target}»? Ответь только номером клетки. Если такого на снимке нет, ответь 0.",
            cols * rows
        );
        let answer = tokio::select! {
            _ = cancel.cancelled() => return Err(ToolError::Cancelled),
            a = self.vision.ask(&png, &question) => a.map_err(failed)?,
        };
        Ok(crate::grid::first_number(&answer)
            .and_then(|n| crate::grid::cell(pic.width, pic.height, cols, rows, n)))
    }

    async fn ask(
        &self,
        png: &[u8],
        question: &str,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        tokio::select! {
            _ = cancel.cancelled() => Err(ToolError::Cancelled),
            a = self.vision.ask(png, question) => a.map_err(failed),
        }
    }

    /// Point of `target` on the picture by the vision model. Picture pixels.
    /// A point is used only after the model, shown a crosshair on it, says yes:
    /// first by the grid, then by pixels named outright.
    async fn by_vision(
        &self,
        png: &[u8],
        target: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<(f32, f32)>, ToolError> {
        let pic = crate::grid::decode(png).ok_or_else(|| failed("снимок не читается"))?;
        let mut seen = false;
        if let Some(p) = self.grid_point(&pic, target, cancel).await? {
            seen = true;
            if self.point_holds(&pic, p, target, cancel).await? {
                return Ok(Some(p));
            }
        }
        if let Some(p) = self.named_point(&pic, target, cancel).await? {
            seen = true;
            if self.point_holds(&pic, p, target, cancel).await? {
                return Ok(Some(p));
            }
        }
        if seen {
            return Err(failed(format!(
                "не уверена, где «{target}»: модель со зрением дважды указала место, но при проверке не подтвердила его. \
Ничего не нажала. Не повторяй это нажатие: спроси пользователя или сделай по-другому (клавиши, ui_read, messenger_send с file)."
            )));
        }
        Ok(None)
    }

    /// The point named outright in pixels of the picture.
    async fn named_point(
        &self,
        pic: &crate::grid::Picture,
        target: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<(f32, f32)>, ToolError> {
        let png = crate::grid::encode(pic).ok_or_else(|| failed("снимок не собрался"))?;
        let question = format!(
            "Снимок {}×{} пикселей. Где середина «{target}»? Ответь только двумя числами: x и y в пикселях этого снимка \
от левого верхнего угла, например «120 340». Если такого нет, ответь «0 0».",
            pic.width, pic.height
        );
        let answer = self.ask(&png, &question, cancel).await?;
        Ok(crate::grid::two_numbers(&answer)
            .filter(|(x, y)| (*x > 0 || *y > 0) && *x < pic.width && *y < pic.height)
            .map(|(x, y)| (x as f32, y as f32)))
    }

    /// Whether the model, shown a crosshair on the point, agrees it is the target.
    /// Only a plain «да» counts: an unclear or empty answer is a no.
    async fn point_holds(
        &self,
        pic: &crate::grid::Picture,
        (x, y): (f32, f32),
        target: &str,
        cancel: &CancellationToken,
    ) -> Result<bool, ToolError> {
        let r = 110.0;
        let (near, at) = crate::grid::crop(
            pic,
            crate::grid::Area {
                x: x - r,
                y: y - r,
                w: 2.0 * r,
                h: 2.0 * r,
            },
        );
        let marked =
            crate::grid::crosshair(&near, (x - at.x).round() as i64, (y - at.y).round() as i64);
        let png = crate::grid::encode(&marked).ok_or_else(|| failed("снимок не собрался"))?;
        let question = format!(
            "На снимке красное перекрестие. Его центр стоит на «{target}»? Ответь одним словом: да или нет."
        );
        let answer = self.ask(&png, &question, cancel).await?;
        Ok(crate::grid::yes(&answer) == Some(true))
    }

    /// The point by the grid: a cell, then a cell inside it.
    async fn grid_point(
        &self,
        pic: &crate::grid::Picture,
        target: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<(f32, f32)>, ToolError> {
        let Some(first) = self.ask_cell(pic, GRID, target, cancel).await? else {
            return Ok(None);
        };
        // The cell with half a cell around it: the thing may sit on a line.
        let around = crate::grid::Area {
            x: first.x - first.w / 2.0,
            y: first.y - first.h / 2.0,
            w: first.w * 2.0,
            h: first.h * 2.0,
        };
        let (part, at) = crate::grid::crop(pic, around);
        let fine = self.ask_cell(&part, FINE_GRID, target, cancel).await?;
        let (x, y) = match fine {
            Some(c) => {
                let (cx, cy) = c.center();
                (at.x + cx, at.y + cy)
            }
            None => first.center(),
        };
        Ok(Some((x, y)))
    }
}

#[async_trait]
impl Tool for ScreenClickTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "screen_click",
            "Нажать в окне на надпись или на то, что видно на картинке, когда ui_read не видит элементов (Telegram, игры, сайты в приложениях). \
Сначала ищет надпись распознаванием текста на этом компьютере, если не нашла — спрашивает модель со зрением. В ответе — что изменилось.",
            props(
                &[
                    ("target", json!({ "type": "string", "description": "Надпись на кнопке или описание: «Поиск», «значок шестерёнки»" })),
                    ("window", window_prop()),
                    ("n", json!({ "type": "integer", "description": "Какую из одинаковых надписей нажать, с 1 сверху" })),
                    ("button", json!({ "type": "string", "enum": ["left", "right"] })),
                    ("double", json!({ "type": "boolean", "description": "Двойной щелчок" })),
                ],
                &["target"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let target = arg_str(args, "target")?.trim();
        if target.is_empty() {
            return Err(ToolError::BadArguments(
                "нужно, на что нажать: target".into(),
            ));
        }
        self.env.ensure_enabled()?;
        let w = self.env.ui.target(args)?;
        Ok(self.env.ui.act(
            &w,
            "Нажать в окне по картинке",
            format!(
                "«{}» в окне «{}»",
                truncate(target, 60),
                truncate(&w.title, 60)
            ),
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        self.env.ensure_enabled()?;
        let target = arg_str(&args, "target")?.trim().to_string();
        let w = self.env.ui.target(&args)?;
        let button = match arg_str_opt(&args, "button") {
            Some("right") => MouseButton::Right,
            _ => MouseButton::Left,
        };
        let double = args.get("double").and_then(Value::as_bool).unwrap_or(false);
        let nth = args.get("n").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
        // A covered window cannot take a click where it is seen.
        self.env.ui.windows.focus(w.id).map_err(failed)?;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let shot = self.env.capture_visible(w.id, 0).await?;
        let screen = self.env.screen.clone();
        let png = shot.png.clone();
        let words = tokio::task::spawn_blocking(move || screen.ocr_words(&png))
            .await
            .map_err(|e| failed(e.to_string()))?
            .unwrap_or_default();
        let found = crate::grid::find_label(&words, &target);
        let (point, how) = if let Some(a) = found.get(nth - 1) {
            let (x, y) = a.center();
            ((x * shot.factor, y * shot.factor), "по надписи")
        } else {
            let small = self.env.capture_visible(w.id, VISION_MAX_SIDE).await?;
            match self.by_vision(&small.png, &target, &cancel).await {
                Ok(Some((x, y))) => (
                    (x * small.factor, y * small.factor),
                    "по картинке, примерно",
                ),
                Ok(None) => {
                    return Err(failed(format!(
                        "не нашла «{target}» в окне «{}»: ни надписи, ни похожего на картинке",
                        truncate(&w.title, 60)
                    )))
                }
                Err(e) => return Err(e),
            }
        };
        let (x, y) = (
            shot.origin.0 + point.0.round() as i32,
            shot.origin.1 + point.1.round() as i32,
        );
        let before = self.env.ui.foreign_windows();
        self.env.ui.show_target(crate::sys::Rect {
            x: x - 12,
            y: y - 12,
            w: 24,
            h: 24,
        });
        tokio::time::sleep(std::time::Duration::from_millis(crate::ui::TARGET_PAUSE_MS)).await;
        self.env
            .ui
            .input
            .click(x, y, button, double)
            .map_err(failed)?;
        let more = if found.len() > 1 {
            format!(" Таких надписей {}, нажала {}-ю сверху.", found.len(), nth)
        } else {
            String::new()
        };
        let report = self.env.ui.outcome(&w, &before).await;
        Ok(ToolOutput::new(format!(
            "Нажала «{target}» ({how}).{more}\n{report}"
        )))
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
                clipboard: None,
                sent: Default::default(),
                clips: Default::default(),
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
    async fn a_guessed_point_is_clicked_only_after_the_crosshair_check() {
        let (env, vision, f) = screen_env("C:\\Users\\me");
        let pic = crate::grid::Picture {
            rgb: vec![230; 400 * 300 * 3],
            width: 400,
            height: 300,
        };
        *f.picture.lock() = Some((crate::grid::encode(&pic).unwrap(), 400, 300));
        let tool = ScreenClickTool { env, vision };
        // The grid points wrong, the named pixels are right.
        *f.vision_seq.lock() = vec![
            "5".into(),
            "1".into(),
            "нет".into(),
            "x=200, y=150".into(),
            "Да".into(),
        ];
        let out = run(&tool, json!({"target": "скрепка", "window": "игра"}))
            .await
            .unwrap()
            .text;
        assert!(out.starts_with("Нажала «скрепка» (по картинке"), "{out}");
        assert_eq!(
            f.clicks.lock().clone(),
            vec![(210, 170, crate::sys::MouseButton::Left, false)]
        );
        // Both guesses fail the check: no click at all.
        f.clicks.lock().clear();
        *f.vision_seq.lock() = vec![
            "5".into(),
            "1".into(),
            "нет".into(),
            "200 150".into(),
            "нет".into(),
        ];
        let err = run(&tool, json!({"target": "скрепка", "window": "игра"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Ничего не нажала"), "{err}");
        assert!(f.clicks.lock().is_empty());
        assert_eq!(crate::grid::two_numbers("(12; 7)"), Some((12, 7)));
        assert_eq!(crate::grid::yes("Нет."), Some(false));
        assert_eq!(crate::grid::yes("может"), None);
    }

    #[tokio::test]
    async fn only_a_plain_yes_on_the_crosshair_lets_the_click_through() {
        let (env, vision, f) = screen_env("C:\\Users\\me");
        let pic = crate::grid::Picture {
            rgb: vec![230; 400 * 300 * 3],
            width: 400,
            height: 300,
        };
        *f.picture.lock() = Some((crate::grid::encode(&pic).unwrap(), 400, 300));
        let tool = ScreenClickTool { env, vision };
        // A marked-up, partial or empty no is a no: nothing is clicked.
        for (grid_no, named, named_no) in [
            ("**Нет**", "x=200, y=150", "Не то, это поле ввода"),
            ("«Нет»", "200 150", "Неверно"),
            ("Nope", "200 150", ""),
        ] {
            *f.vision_seq.lock() = vec![
                "5".into(),
                "1".into(),
                grid_no.into(),
                named.into(),
                named_no.into(),
            ];
            let err = run(&tool, json!({"target": "скрепка", "window": "игра"}))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("Ничего не нажала"), "{err}");
            assert!(f.clicks.lock().is_empty(), "{grid_no} / {named_no}");
        }
        // Pixels with fractions keep y; a yes in bold still counts.
        *f.vision_seq.lock() = vec![
            "5".into(),
            "1".into(),
            "нет".into(),
            "x=200.5, y=150.0".into(),
            "**Да**".into(),
        ];
        run(&tool, json!({"target": "скрепка", "window": "игра"}))
            .await
            .unwrap();
        assert_eq!(
            f.clicks.lock().clone(),
            vec![(210, 170, crate::sys::MouseButton::Left, false)]
        );
    }

    #[tokio::test]
    async fn click_finds_the_label_by_text_and_clicks_its_screen_point() {
        let (env, vision, f) = screen_env("C:\\Users\\me");
        *f.ocr_words.lock() = vec![
            crate::sys::OcrWord {
                text: "Поиск".into(),
                line: 0,
                x: 100.0,
                y: 40.0,
                w: 60.0,
                h: 20.0,
            },
            crate::sys::OcrWord {
                text: "Поиск".into(),
                line: 3,
                x: 100.0,
                y: 400.0,
                w: 60.0,
                h: 20.0,
            },
        ];
        let tool = ScreenClickTool { env, vision };
        let a = tool
            .assess(&json!({"target": "Поиск", "window": "игра"}))
            .unwrap();
        assert_eq!((a.level, a.scope.as_str()), (RiskLevel::Medium, "game.exe"));
        assert!(tool
            .assess(&json!({"target": "Оплатить", "window": "оплата"}))
            .is_err());
        let out = run(&tool, json!({"target": "поиск", "window": "игра", "n": 2}))
            .await
            .unwrap()
            .text;
        assert!(
            out.starts_with("Нажала «поиск» (по надписи). Таких надписей 2"),
            "{out}"
        );
        // Picture origin (10, 20) plus the middle of the second word.
        let clicks = f.clicks.lock().clone();
        assert_eq!(
            clicks,
            vec![(140, 430, crate::sys::MouseButton::Left, false)]
        );
        assert_eq!(*f.focused.lock(), vec![7]);
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
