//! Control of any window: read its elements, click, type, press keys, scroll, arrange.
//!
//! Every tool resolves its window first and refuses the "blind zone".
//! The rule scope of the acting tools is the program, so "always allow" covers one program.

use std::sync::Arc;

use async_trait::async_trait;
use mika_core::events::{Event, EventBus};
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::blind::{is_blind_title, BLIND_REFUSAL};
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::apps::score;
use crate::sys::{Input, MouseButton, Placement, Rect, Ui, UiNode, WindowInfo, Windows};
use crate::util::{arg_bool, arg_u64, failed, truncate};

/// Elements shown to the model from one window.
pub const MAX_NODES: usize = 400;
const MAX_TREE_CHARS: usize = 14_000;
/// The highlight frame stays this long before the click.
const TARGET_PAUSE_MS: u64 = 350;

/// Shared parts of the window tools.
#[derive(Clone)]
pub struct UiEnv {
    pub windows: Arc<dyn Windows>,
    pub ui: Arc<dyn Ui>,
    pub input: Arc<dyn Input>,
    pub bus: EventBus,
    /// Executable name of the assistant itself, lower case; its windows are never a target.
    pub own_process: String,
}

impl UiEnv {
    /// The window named by `window`, or the topmost window of another program.
    pub fn target(&self, args: &Value) -> Result<WindowInfo, ToolError> {
        let list: Vec<WindowInfo> = self
            .windows
            .list()
            .into_iter()
            .filter(|w| w.process != self.own_process)
            .collect();
        let query = arg_str_opt(args, "window").map(str::trim).unwrap_or("");
        let found = if query.is_empty() {
            self.windows
                .foreground()
                .filter(|w| w.process != self.own_process)
                .or_else(|| list.first().cloned())
        } else {
            list.iter()
                .map(|w| (score(w, query), w))
                .filter(|(s, _)| *s > 0)
                // The first of equal scores: windows come topmost first.
                .fold(None::<(u8, &WindowInfo)>, |best, (s, w)| match best {
                    Some((bs, _)) if bs >= s => best,
                    _ => Some((s, w)),
                })
                .map(|(_, w)| w.clone())
        };
        let w = found.ok_or_else(|| {
            failed(if query.is_empty() {
                "нет открытых окон".to_string()
            } else {
                format!("нет окна «{query}»")
            })
        })?;
        if is_blind_title(&w.title) {
            return Err(failed(BLIND_REFUSAL));
        }
        Ok(w)
    }

    fn act(&self, w: &WindowInfo, summary: impl Into<String>, target: String) -> Assessment {
        Assessment::new(
            RiskLevel::Medium,
            summary,
            target,
            format!("действие в программе {}", program(w)),
        )
        .with_scope(program(w))
    }

    fn show_target(&self, r: Rect) {
        self.bus.publish(Event::UiTarget {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        });
    }
}

fn program(w: &WindowInfo) -> String {
    if w.process.is_empty() {
        "неизвестная программа".into()
    } else {
        w.process.clone()
    }
}

fn label(w: &WindowInfo) -> String {
    format!("«{}» ({})", truncate(&w.title, 60), program(w))
}

fn node_arg(args: &Value) -> Option<u32> {
    arg_u64(args, "n").map(|n| n as u32)
}

const WINDOW_PROP: (&str, &str) = (
    "window",
    "Часть заголовка окна или имя программы; без него — окно, в котором работает пользователь",
);

fn props(extra: &[(&str, Value)], required: &[&str]) -> Value {
    let mut p = serde_json::Map::new();
    p.insert(
        WINDOW_PROP.0.into(),
        json!({ "type": "string", "description": WINDOW_PROP.1 }),
    );
    for (name, schema) in extra {
        p.insert((*name).into(), schema.clone());
    }
    json!({ "type": "object", "properties": p, "required": required })
}

/// The tree as text: one element per line, children indented.
pub fn render_tree(nodes: &[UiNode]) -> String {
    let mut out = String::new();
    for node in nodes.iter().take(MAX_NODES) {
        let mut line = format!(
            "{}[{}] {}",
            "  ".repeat(node.depth.min(10) as usize),
            node.n,
            node.role
        );
        if !node.name.is_empty() {
            line.push_str(&format!(" «{}»", truncate(&node.name, 80)));
        }
        if !node.value.is_empty() {
            line.push_str(&format!(" = {}", truncate(&node.value, 120)));
        }
        if !node.enabled {
            line.push_str(" (недоступен)");
        }
        line.push('\n');
        if out.len() + line.len() > MAX_TREE_CHARS {
            out.push_str("… дальше элементы не показаны\n");
            break;
        }
        out.push_str(&line);
    }
    out
}

pub struct UiReadTool(pub UiEnv);

#[async_trait]
impl Tool for UiReadTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "ui_read",
            "Прочитать окно программы: кнопки, поля, меню и их текст. У каждого элемента номер в квадратных скобках — он нужен для ui_click и ui_type. После любого действия номера устаревают: прочитай окно заново.",
            props(&[], &[]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let w = self.0.target(args)?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Прочитать окно",
            label(&w),
            "только чтение содержимого окна",
        )
        .with_scope(program(&w)))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let w = self.0.target(&args)?;
        let ui = self.0.ui.clone();
        let id = w.id;
        let nodes = tokio::task::spawn_blocking(move || ui.tree(id))
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(failed)?;
        if nodes.len() <= 1 {
            return Ok(ToolOutput::new(format!(
                "Окно {} не отдаёт свои элементы. Попробуй клавиши (ui_keys) или посмотри на него (screen_look).",
                label(&w)
            )));
        }
        Ok(ToolOutput::new(format!(
            "Окно {}\n{}",
            label(&w),
            render_tree(&nodes)
        )))
    }
}

pub struct UiClickTool(pub UiEnv);

fn button(args: &Value) -> Result<MouseButton, ToolError> {
    match arg_str_opt(args, "button").unwrap_or("left") {
        "" | "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        other => Err(ToolError::BadArguments(format!(
            "неизвестная кнопка мыши {other}"
        ))),
    }
}

/// Where to click: an element number, or a point inside the window.
enum Spot {
    Node(u32),
    Point(i32, i32),
}

fn spot(args: &Value) -> Result<Spot, ToolError> {
    let point = (arg_u64(args, "x"), arg_u64(args, "y"));
    match (node_arg(args), point) {
        (Some(n), (None, None)) => Ok(Spot::Node(n)),
        (None, (Some(x), Some(y))) => Ok(Spot::Point(x as i32, y as i32)),
        _ => Err(ToolError::BadArguments(
            "укажи n (номер элемента из ui_read) или x и y (точка в окне)".into(),
        )),
    }
}

#[async_trait]
impl Tool for UiClickTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "ui_click",
            "Нажать на элемент окна по номеру из ui_read: кнопку, пункт меню, флажок, вкладку. Если у элемента нет номера, можно нажать в точку x, y — пиксели от левого верхнего угла окна, как на снимке screen_look.",
            props(
                &[
                    ("n", json!({ "type": "integer", "description": "Номер элемента из ui_read" })),
                    ("x", json!({ "type": "integer", "description": "Пиксель по горизонтали от левого края окна" })),
                    ("y", json!({ "type": "integer", "description": "Пиксель по вертикали от верхнего края окна" })),
                    ("button", json!({ "type": "string", "enum": ["left", "right", "middle"] })),
                    ("double", json!({ "type": "boolean", "description": "Двойной щелчок" })),
                ],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let w = self.0.target(args)?;
        button(args)?;
        let what = match spot(args)? {
            Spot::Node(n) => format!("элемент №{n}"),
            Spot::Point(x, y) => format!("точка {x}, {y}"),
        };
        Ok(self
            .0
            .act(&w, "Нажать в окне", format!("{what} в окне {}", label(&w))))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let env = self.0.clone();
        let w = env.target(&args)?;
        let btn = button(&args)?;
        let double = arg_bool(&args, "double");
        let where_ = spot(&args)?;
        let id = w.id;
        let ui = env.ui.clone();
        let rect = match where_ {
            Spot::Node(n) => tokio::task::spawn_blocking(move || ui.rect(id, n)),
            Spot::Point(..) => tokio::task::spawn_blocking(move || ui.rect(id, 0)),
        }
        .await
        .map_err(|e| failed(e.to_string()))?
        .map_err(failed)?;

        let (x, y, frame) = match where_ {
            Spot::Node(_) => {
                let (x, y) = rect.center();
                (x, y, rect)
            }
            Spot::Point(px, py) => {
                if px >= rect.w || py >= rect.h {
                    return Err(failed(format!(
                        "точка {px}, {py} за пределами окна {}×{}",
                        rect.w, rect.h
                    )));
                }
                let (x, y) = (rect.x + px, rect.y + py);
                (
                    x,
                    y,
                    Rect {
                        x: x - 12,
                        y: y - 12,
                        w: 24,
                        h: 24,
                    },
                )
            }
        };
        env.show_target(frame);
        tokio::time::sleep(std::time::Duration::from_millis(TARGET_PAUSE_MS)).await;

        // A plain left click on a known element goes through UI Automation:
        // it works when the window is covered or scrolled.
        if let (Spot::Node(n), MouseButton::Left, false) = (&where_, btn, double) {
            let (ui, n) = (env.ui.clone(), *n);
            let done = tokio::task::spawn_blocking(move || ui.invoke(id, n))
                .await
                .map_err(|e| failed(e.to_string()))?
                .map_err(failed)?;
            if let Some(what) = done {
                return Ok(ToolOutput::new(format!(
                    "{what}: элемент №{n}. Прочитай окно заново, чтобы увидеть результат."
                )));
            }
        }
        env.windows.focus(id).map_err(failed)?;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        env.input.click(x, y, btn, double).map_err(failed)?;
        Ok(ToolOutput::new(
            "Щёлкнула мышью. Прочитай окно заново, чтобы увидеть результат.",
        ))
    }
}

pub struct UiTypeTool(pub UiEnv);

#[async_trait]
impl Tool for UiTypeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "ui_type",
            "Ввести текст в окно программы. С n — в поле с этим номером из ui_read (старый текст поля заменяется). Без n — печатает туда, где стоит курсор. enter=true нажмёт Enter после текста.",
            props(
                &[
                    ("text", json!({ "type": "string", "description": "Текст" })),
                    ("n", json!({ "type": "integer", "description": "Номер поля из ui_read" })),
                    ("enter", json!({ "type": "boolean", "description": "Нажать Enter после ввода" })),
                ],
                &["text"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let w = self.0.target(args)?;
        let text = arg_str(args, "text")?;
        Ok(self.0.act(
            &w,
            "Ввести текст в окно",
            format!("«{}» в окно {}", truncate(text, 200), label(&w)),
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let env = self.0.clone();
        let w = env.target(&args)?;
        let text = arg_str(&args, "text")?.to_string();
        let id = w.id;
        let mut typed_by_pattern = false;
        if let Some(n) = node_arg(&args) {
            let (ui, t) = (env.ui.clone(), text.clone());
            typed_by_pattern = tokio::task::spawn_blocking(move || ui.set_text(id, n, &t))
                .await
                .map_err(|e| failed(e.to_string()))?
                .map_err(failed)?;
            if !typed_by_pattern {
                env.windows.focus(id).map_err(failed)?;
                let ui = env.ui.clone();
                tokio::task::spawn_blocking(move || ui.focus(id, n))
                    .await
                    .map_err(|e| failed(e.to_string()))?
                    .map_err(failed)?;
            }
        } else {
            env.windows.focus(id).map_err(failed)?;
        }
        if !typed_by_pattern {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            env.input.text(&text).map_err(failed)?;
        }
        if arg_bool(&args, "enter") {
            if typed_by_pattern {
                env.windows.focus(id).map_err(failed)?;
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            }
            env.input.chord(&[VK_RETURN]).map_err(failed)?;
        }
        Ok(ToolOutput::new(format!(
            "Ввела текст в окно {}.",
            label(&w)
        )))
    }
}

const VK_RETURN: u16 = 0x0D;

/// Virtual key of one key name.
fn key_code(name: &str) -> Option<u16> {
    let n = name.trim().to_lowercase();
    Some(match n.as_str() {
        "ctrl" | "control" => 0x11,
        "alt" => 0x12,
        "shift" => 0x10,
        "win" | "windows" | "super" => 0x5B,
        "enter" | "return" => VK_RETURN,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "space" | "пробел" => 0x20,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "printscreen" | "prtsc" => 0x2C,
        "plus" => 0xBB,
        "minus" => 0xBD,
        _ => {
            if let Some(f) = n.strip_prefix('f').and_then(|d| d.parse::<u16>().ok()) {
                return (1..=24).contains(&f).then_some(0x70 + f - 1);
            }
            let mut chars = n.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => c.to_ascii_uppercase() as u16,
                (Some(c), None) if c.is_ascii_digit() => c as u16,
                _ => return None,
            }
        }
    })
}

/// `ctrl+shift+s` as virtual keys, modifiers first.
pub fn parse_chord(chord: &str) -> Result<Vec<u16>, String> {
    let keys: Vec<u16> = chord
        .split('+')
        .map(|k| key_code(k).ok_or_else(|| format!("не знаю клавишу «{}»", k.trim())))
        .collect::<Result<_, _>>()?;
    if keys.is_empty() {
        return Err("пустое сочетание клавиш".into());
    }
    Ok(keys)
}

/// `ctrl+a ctrl+c` or `ctrl+a, ctrl+c`: several chords pressed one after another.
pub fn parse_keys(text: &str) -> Result<Vec<Vec<u16>>, String> {
    let chords: Vec<&str> = text
        .split([' ', ','])
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    if chords.is_empty() {
        return Err("нужны клавиши, например ctrl+s или enter".into());
    }
    if chords.len() > 20 {
        return Err("слишком много сочетаний за один раз".into());
    }
    chords.into_iter().map(parse_chord).collect()
}

pub struct UiKeysTool(pub UiEnv);

#[async_trait]
impl Tool for UiKeysTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "ui_keys",
            "Нажать клавиши в окне программы: enter, tab, esc, стрелки, f5, сочетания вроде ctrl+s, alt+f4, ctrl+shift+t. Несколько нажатий подряд пишутся через пробел: «ctrl+a delete».",
            props(
                &[(
                    "keys",
                    json!({ "type": "string", "description": "Клавиши и сочетания через пробел" }),
                )],
                &["keys"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let w = self.0.target(args)?;
        let keys = arg_str(args, "keys")?;
        parse_keys(keys).map_err(ToolError::BadArguments)?;
        Ok(self.0.act(
            &w,
            "Нажать клавиши в окне",
            format!("{} в окне {}", keys.trim(), label(&w)),
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let w = self.0.target(&args)?;
        let chords = parse_keys(arg_str(&args, "keys")?).map_err(ToolError::BadArguments)?;
        self.0.windows.focus(w.id).map_err(failed)?;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        for chord in &chords {
            self.0.input.chord(chord).map_err(failed)?;
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        }
        Ok(ToolOutput::new(format!(
            "Нажала клавиши в окне {}.",
            label(&w)
        )))
    }
}

pub struct UiScrollTool(pub UiEnv);

#[async_trait]
impl Tool for UiScrollTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "ui_scroll",
            "Прокрутить окно или элемент колесом мыши.",
            props(
                &[
                    (
                        "direction",
                        json!({ "type": "string", "enum": ["down", "up"] }),
                    ),
                    (
                        "amount",
                        json!({ "type": "integer", "description": "Сколько щелчков колеса, 1–30" }),
                    ),
                    (
                        "n",
                        json!({ "type": "integer", "description": "Номер элемента из ui_read, необязательно" }),
                    ),
                ],
                &["direction"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let w = self.0.target(args)?;
        let dir = arg_str(args, "direction")?;
        if !matches!(dir, "down" | "up") {
            return Err(ToolError::BadArguments(format!(
                "неизвестное направление {dir}"
            )));
        }
        Ok(Assessment::new(
            RiskLevel::Low,
            "Прокрутить окно",
            label(&w),
            "прокрутка ничего не меняет",
        )
        .with_scope(program(&w)))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let w = self.0.target(&args)?;
        let amount = arg_u64(&args, "amount").unwrap_or(3).clamp(1, 30) as i32;
        let lines = if arg_str(&args, "direction")? == "up" {
            -amount
        } else {
            amount
        };
        let (ui, id, n) = (self.0.ui.clone(), w.id, node_arg(&args).unwrap_or(0));
        let rect = tokio::task::spawn_blocking(move || ui.rect(id, n))
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(failed)?;
        let (x, y) = rect.center();
        self.0.input.wheel(x, y, lines).map_err(failed)?;
        Ok(ToolOutput::new(format!("Прокрутила окно {}.", label(&w))))
    }
}

fn placement(s: &str) -> Result<(Placement, &'static str), ToolError> {
    Ok(match s {
        "left" => (Placement::Left, "в левую половину экрана"),
        "right" => (Placement::Right, "в правую половину экрана"),
        "top" => (Placement::Top, "в верхнюю половину экрана"),
        "bottom" => (Placement::Bottom, "в нижнюю половину экрана"),
        "maximize" => (Placement::Maximize, "на весь экран"),
        "minimize" => (Placement::Minimize, "в панель задач"),
        "restore" => (Placement::Restore, "в обычный размер"),
        other => {
            return Err(ToolError::BadArguments(format!(
                "неизвестное размещение {other}"
            )))
        }
    })
}

pub struct ArrangeWindowTool(pub UiEnv);

#[async_trait]
impl Tool for ArrangeWindowTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "arrange_window",
            "Разложить окно по экрану: в левую, правую, верхнюю или нижнюю половину, развернуть, свернуть, вернуть обычный размер.",
            props(
                &[(
                    "placement",
                    json!({ "type": "string", "enum": ["left", "right", "top", "bottom", "maximize", "minimize", "restore"] }),
                )],
                &["placement"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let w = self.0.target(args)?;
        let (_, where_) = placement(arg_str(args, "placement")?)?;
        Ok(Assessment::new(
            RiskLevel::Low,
            "Переставить окно",
            format!("{} {where_}", label(&w)),
            "окно легко вернуть на место",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let w = self.0.target(&args)?;
        let (p, where_) = placement(arg_str(&args, "placement")?)?;
        self.0.windows.place(w.id, p).map_err(failed)?;
        Ok(ToolOutput::new(format!(
            "Окно {} теперь {where_}.",
            label(&w)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env, run, Fakes};

    fn ui_env() -> (UiEnv, Arc<Fakes>) {
        let (e, f) = env();
        *f.windows.lock() = vec![
            WindowInfo {
                id: 1,
                title: "Мика".into(),
                process: "mika.exe".into(),
            },
            WindowInfo {
                id: 7,
                title: "Безымянный — Блокнот".into(),
                process: "notepad.exe".into(),
            },
            WindowInfo {
                id: 9,
                title: "Вход в банк — Chrome".into(),
                process: "chrome.exe".into(),
            },
        ];
        *f.tree.lock() = vec![
            UiNode {
                n: 0,
                depth: 0,
                role: "Window".into(),
                name: "Безымянный — Блокнот".into(),
                enabled: true,
                ..Default::default()
            },
            UiNode {
                n: 1,
                depth: 1,
                role: "Edit".into(),
                name: "Текстовый редактор".into(),
                value: "привет".into(),
                enabled: true,
            },
            UiNode {
                n: 2,
                depth: 1,
                role: "MenuItem".into(),
                name: "Файл".into(),
                enabled: false,
                ..Default::default()
            },
        ];
        let ui = UiEnv {
            windows: e.windows.clone(),
            ui: e.ui.clone(),
            input: e.input.clone(),
            bus: e.shared.bus.clone(),
            own_process: "mika.exe".into(),
        };
        (ui, f)
    }

    #[tokio::test]
    async fn read_renders_numbered_tree_of_the_topmost_foreign_window() {
        let (e, _) = ui_env();
        let out = run(&UiReadTool(e.clone()), json!({})).await.unwrap().text;
        assert_eq!(
            out,
            "Окно «Безымянный — Блокнот» (notepad.exe)\n\
             [0] Window «Безымянный — Блокнот»\n\
             \x20 [1] Edit «Текстовый редактор» = привет\n\
             \x20 [2] MenuItem «Файл» (недоступен)\n"
        );
        let a = UiReadTool(e).assess(&json!({"window": "блокнот"})).unwrap();
        assert_eq!((a.level, a.scope.as_str()), (RiskLevel::Low, "notepad.exe"));
    }

    #[tokio::test]
    async fn blind_zone_window_is_refused_before_anything_happens() {
        let (e, f) = ui_env();
        for tool in [
            &UiReadTool(e.clone()) as &dyn Tool,
            &UiClickTool(e.clone()),
            &UiTypeTool(e.clone()),
            &UiKeysTool(e.clone()),
            &UiScrollTool(e.clone()),
            &ArrangeWindowTool(e.clone()),
        ] {
            let err = tool
                .assess(&json!({
                    "window": "банк", "n": 1, "text": "x", "keys": "enter",
                    "direction": "down", "placement": "left"
                }))
                .unwrap_err();
            assert!(err.to_string().contains("слепой зоне"), "{err}");
        }
        assert!(f.ui_calls.lock().is_empty());
        assert!(f.typed.lock().is_empty());
    }

    #[tokio::test]
    async fn own_window_is_never_the_target() {
        let (e, _) = ui_env();
        assert!(UiReadTool(e.clone())
            .assess(&json!({"window": "мика"}))
            .is_err());
        assert!(UiReadTool(e)
            .assess(&json!({"window": "калькулятор"}))
            .is_err());
    }

    #[tokio::test]
    async fn click_uses_automation_first_and_shows_the_target() {
        let (e, f) = ui_env();
        let mut rx = e.bus.subscribe();
        *f.invoke_result.lock() = Some("Нажала");
        let tool = UiClickTool(e.clone());
        let a = tool.assess(&json!({"n": 2})).unwrap();
        assert_eq!(
            (a.level, a.scope.as_str()),
            (RiskLevel::Medium, "notepad.exe")
        );
        let out = run(&tool, json!({"n": 2})).await.unwrap().text;
        assert!(out.starts_with("Нажала: элемент №2"));
        assert!(f.clicks.lock().is_empty());
        assert!(matches!(
            rx.try_recv().unwrap(),
            Event::UiTarget {
                x: 100,
                y: 200,
                w: 80,
                h: 30
            }
        ));
    }

    #[tokio::test]
    async fn click_falls_back_to_the_mouse_and_supports_points() {
        let (e, f) = ui_env();
        let tool = UiClickTool(e.clone());
        run(&tool, json!({"n": 2})).await.unwrap();
        assert_eq!(*f.clicks.lock(), vec![(140, 215, MouseButton::Left, false)]);
        assert_eq!(*f.focused.lock(), vec![7]);

        run(&tool, json!({"x": 5, "y": 6, "button": "right"}))
            .await
            .unwrap();
        assert_eq!(f.clicks.lock()[1], (105, 206, MouseButton::Right, false));
        assert!(run(&tool, json!({"x": 500, "y": 6})).await.is_err());
        assert!(tool.assess(&json!({})).is_err());
        assert!(tool.assess(&json!({"n": 1, "x": 1, "y": 1})).is_err());
    }

    #[tokio::test]
    async fn type_sets_text_or_types_it_and_presses_enter() {
        let (e, f) = ui_env();
        let tool = UiTypeTool(e.clone());
        *f.set_text_ok.lock() = true;
        run(&tool, json!({"n": 1, "text": "привет"})).await.unwrap();
        assert_eq!(*f.ui_calls.lock(), vec!["set_text 7 1 привет".to_string()]);
        assert!(f.typed.lock().is_empty());

        *f.set_text_ok.lock() = false;
        run(&tool, json!({"n": 1, "text": "мир", "enter": true}))
            .await
            .unwrap();
        assert_eq!(*f.typed.lock(), vec!["мир".to_string()]);
        assert_eq!(*f.chords.lock(), vec![vec![VK_RETURN]]);
        assert!(f.ui_calls.lock().contains(&"focus 7 1".to_string()));
    }

    #[tokio::test]
    async fn keys_are_parsed_and_sent_in_order() {
        let (e, f) = ui_env();
        let tool = UiKeysTool(e);
        run(&tool, json!({"keys": "ctrl+a, Delete F5"}))
            .await
            .unwrap();
        assert_eq!(
            *f.chords.lock(),
            vec![vec![0x11, 0x41], vec![0x2E], vec![0x74]]
        );
        assert!(tool.assess(&json!({"keys": "ctrl+ъ"})).is_err());
        assert!(tool.assess(&json!({"keys": "  "})).is_err());
    }

    #[tokio::test]
    async fn scroll_and_arrange() {
        let (e, f) = ui_env();
        run(
            &UiScrollTool(e.clone()),
            json!({"direction": "up", "amount": 99}),
        )
        .await
        .unwrap();
        assert_eq!(*f.wheels.lock(), vec![(140, 215, -30)]);
        run(
            &ArrangeWindowTool(e.clone()),
            json!({"window": "notepad", "placement": "left"}),
        )
        .await
        .unwrap();
        assert_eq!(*f.placed.lock(), vec![(7, Placement::Left)]);
        assert!(ArrangeWindowTool(e)
            .assess(&json!({"placement": "sideways"}))
            .is_err());
    }

    #[test]
    fn chord_parser() {
        assert_eq!(parse_chord("Ctrl+Shift+S"), Ok(vec![0x11, 0x10, 0x53]));
        assert_eq!(parse_chord("win+r"), Ok(vec![0x5B, 0x52]));
        assert_eq!(parse_chord("f12"), Ok(vec![0x7B]));
        assert_eq!(parse_chord("7"), Ok(vec![0x37]));
        assert!(parse_chord("f99").is_err());
        assert!(parse_chord("ctrl+").is_err());
    }
}
