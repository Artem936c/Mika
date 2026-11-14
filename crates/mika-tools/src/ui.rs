//! Control of any window: read its elements, click, type, press keys, scroll, arrange.
//!
//! Every tool resolves its window first and refuses the "blind zone".
//! The rule scope of the acting tools is the program, so "always allow" covers one program.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::events::{Event, EventBus};
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::blind::{is_blind_title, BLIND_REFUSAL};
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::apps::score;
use crate::sys::{Input, MouseButton, Placement, Rect, Ui, UiNode, WindowInfo, Windows};
use crate::util::{arg_bool, arg_u64, failed, truncate};

/// Elements shown to the model from one window.
pub const MAX_NODES: usize = 400;
const MAX_TREE_CHARS: usize = 14_000;
/// The highlight frame stays this long before the click.
pub(crate) const TARGET_PAUSE_MS: u64 = 350;
/// Time for the program to react before its window is read again.
const SETTLE_MS: u64 = 400;
/// Changed elements listed after an action.
const MAX_CHANGES: usize = 15;
/// Names in one click sequence.
const MAX_SEQUENCE: usize = 30;

/// The last tree read from each window: actions report what changed since.
pub type Seen = Arc<Mutex<HashMap<u64, Vec<UiNode>>>>;

/// The last text sent with Enter: window, text and when.
pub type Sent = Arc<Mutex<Option<(u64, String, std::time::Instant)>>>;

/// The same message to the same window this soon is a double send: small
/// models repeat ui_type when a chat window does not show what changed.
const RESEND_GUARD: std::time::Duration = std::time::Duration::from_secs(90);

/// Programs that draw their content themselves: UI Automation shows their
/// buttons and lists but not the text of messages.
const SELF_DRAWN: [&str; 4] = ["telegram", "ayugram", "kotatogram", "discord"];

/// Messengers, by the start of their executable name. Enter there sends a
/// message: the input is cleared first and the same text is not sent twice.
/// Elsewhere (Notepad, Word) Ctrl+A would select the whole document.
const CHAT_APPS: [&str; 12] = [
    "telegram",
    "ayugram",
    "kotatogram",
    "discord",
    "whatsapp",
    "viber",
    "slack",
    "teams",
    "ms-teams",
    "skype",
    "signal",
    "element",
];

fn is_chat(process: &str) -> bool {
    let p = process.to_lowercase();
    CHAT_APPS.iter().any(|c| p.starts_with(c))
}

/// Shared parts of the window tools.
#[derive(Clone)]
pub struct UiEnv {
    pub windows: Arc<dyn Windows>,
    pub ui: Arc<dyn Ui>,
    pub input: Arc<dyn Input>,
    pub bus: EventBus,
    /// Executable name of the assistant itself, lower case; its windows are never a target.
    pub own_process: String,
    pub seen: Seen,
    /// Text with emoji is pasted: typed by key events, it lands out of order in
    /// some programs (Telegram puts 😊 in the middle of the word).
    pub clipboard: Option<Arc<dyn crate::sys::Clipboard>>,
    pub sent: Sent,
    /// Clipboard history: it keeps quiet while she uses the clipboard.
    pub clips: crate::cliphist::ClipHistory,
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
                // The model may name the window in another language: show what there is.
                let open: Vec<String> = list
                    .iter()
                    .filter(|w| !is_blind_title(&w.title))
                    .take(12)
                    .map(label)
                    .collect();
                format!(
                    "нет окна «{query}». Открытые окна: {}. Укажи в window часть заголовка как здесь",
                    if open.is_empty() { "нет".to_string() } else { open.join(", ") }
                )
            })
        })?;
        if is_blind_title(&w.title) {
            return Err(failed(BLIND_REFUSAL));
        }
        Ok(w)
    }

    pub(crate) fn act(
        &self,
        w: &WindowInfo,
        summary: impl Into<String>,
        target: String,
    ) -> Assessment {
        Assessment::new(
            RiskLevel::Medium,
            summary,
            target,
            format!("действие в программе {}", program(w)),
        )
        .with_scope(program(w))
    }

    async fn tree(&self, id: u64) -> Result<Vec<UiNode>, ToolError> {
        let ui = self.ui.clone();
        let nodes = tokio::task::spawn_blocking(move || ui.tree(id))
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(failed)?;
        self.seen.lock().insert(id, nodes.clone());
        Ok(nodes)
    }

    /// The remembered tree of the window, or a fresh one.
    async fn known_tree(&self, id: u64) -> Result<Vec<UiNode>, ToolError> {
        let known = self.seen.lock().get(&id).cloned();
        match known {
            Some(t) => Ok(t),
            None => self.tree(id).await,
        }
    }

    pub(crate) fn foreign_windows(&self) -> Vec<WindowInfo> {
        self.windows
            .list()
            .into_iter()
            .filter(|w| w.process != self.own_process)
            .collect()
    }

    /// What an action changed: a closed window, a new window (a dialog), or the
    /// elements that differ from the last reading. Saves a separate `ui_read`.
    pub(crate) async fn outcome(&self, w: &WindowInfo, before: &[WindowInfo]) -> String {
        tokio::time::sleep(std::time::Duration::from_millis(SETTLE_MS)).await;
        let now = self.foreign_windows();
        let mut out = Vec::new();
        for n in now.iter().filter(|n| !before.iter().any(|b| b.id == n.id)) {
            out.push(if is_blind_title(&n.title) {
                format!(
                    "Появилось окно {} — оно в слепой зоне, его не трогаю.",
                    program(n)
                )
            } else {
                format!(
                    "Появилось окно {}: прочитай его через ui_read с window.",
                    label(n)
                )
            });
        }
        if !now.iter().any(|n| n.id == w.id) {
            self.seen.lock().remove(&w.id);
            out.push(format!("Окно {} закрылось.", label(w)));
            return out.join("\n");
        }
        let old = self.seen.lock().get(&w.id).cloned();
        match self.tree(w.id).await {
            Err(e) => out.push(format!("Окно не перечитала: {e}")),
            Ok(new) => match old {
                None => out.push(format!("Окно {} сейчас:\n{}", label(w), render_tree(&new))),
                Some(old) => out.push(changes(&old, &new)),
            },
        }
        out.join("\n")
    }

    /// One press of element `n`: UI Automation first, the mouse when it cannot.
    async fn press(
        &self,
        id: u64,
        n: u32,
        btn: MouseButton,
        double: bool,
    ) -> Result<(), ToolError> {
        let ui = self.ui.clone();
        let rect = tokio::task::spawn_blocking(move || ui.rect(id, n))
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(failed)?;
        self.show_target(rect);
        if btn == MouseButton::Left && !double {
            let ui = self.ui.clone();
            let done = tokio::task::spawn_blocking(move || ui.invoke(id, n))
                .await
                .map_err(|e| failed(e.to_string()))?
                .map_err(failed)?;
            if done.is_some() {
                return Ok(());
            }
        }
        self.windows.focus(id).map_err(failed)?;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let (x, y) = rect.center();
        self.input.click(x, y, btn, double).map_err(failed)
    }

    pub(crate) fn show_target(&self, r: Rect) {
        self.bus.publish(Event::UiTarget {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        });
    }
}

pub(crate) fn program(w: &WindowInfo) -> String {
    if w.process.is_empty() {
        "неизвестная программа".into()
    } else {
        w.process.clone()
    }
}

pub(crate) fn label(w: &WindowInfo) -> String {
    format!("«{}» ({})", truncate(&w.title, 60), program(w))
}

fn node_arg(args: &Value) -> Option<u32> {
    arg_u64(args, "n").map(|n| n as u32)
}

const WINDOW_PROP: (&str, &str) = (
    "window",
    "Часть заголовка окна или имя программы; без него — окно, в котором работает пользователь",
);

pub(crate) fn props(extra: &[(&str, Value)], required: &[&str]) -> Value {
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

fn node_line(node: &UiNode) -> String {
    let mut line = format!("[{}] {}", node.n, node.role);
    if !node.name.is_empty() {
        line.push_str(&format!(" «{}»", truncate(&node.name, 80)));
    }
    if !node.value.is_empty() {
        line.push_str(&format!(" = {}", truncate(&node.value, 120)));
    } else if node.role == "ComboBox" {
        // WinUI lists (Settings → Scale) show their choice only when open.
        line.push_str(" (выбор виден, если раскрыть: ui_click по нему, потом ui_keys esc)");
    }
    if !node.enabled {
        line.push_str(" (недоступен)");
    }
    line
}

/// Elements that appeared or changed since `old`, with their current numbers.
pub fn changes(old: &[UiNode], new: &[UiNode]) -> String {
    let key = |n: &UiNode| {
        (
            n.depth,
            n.role.clone(),
            n.name.clone(),
            n.value.clone(),
            n.enabled,
        )
    };
    let before: std::collections::HashSet<_> = old.iter().map(key).collect();
    let after: std::collections::HashSet<_> = new.iter().map(key).collect();
    let fresh: Vec<&UiNode> = new.iter().filter(|n| !before.contains(&key(n))).collect();
    let gone = old.iter().filter(|n| !after.contains(&key(n))).count();
    if fresh.is_empty() && gone == 0 {
        return "Видимых изменений в окне нет.".into();
    }
    let mut out = String::from("Изменилось в окне (номера актуальны):");
    for n in fresh.iter().take(MAX_CHANGES) {
        out.push('\n');
        out.push_str(&node_line(n));
    }
    if fresh.len() > MAX_CHANGES {
        out.push_str(&format!(
            "\n…и ещё {} новых элементов: прочитай окно",
            fresh.len() - MAX_CHANGES
        ));
    }
    if gone > 0 {
        out.push_str(&format!("\nПропало элементов: {gone}."));
    }
    out
}

const CLICKABLE: &[&str] = &[
    "button",
    "menuitem",
    "listitem",
    "hyperlink",
    "tabitem",
    "checkbox",
    "radiobutton",
    "treeitem",
    "splitbutton",
    "dataitem",
    "image",
];

/// The element called `name`: an exact name first, then one that contains it;
/// enabled clickable elements win over the rest.
pub fn find_by_name(nodes: &[UiNode], name: &str) -> Option<u32> {
    let want = name.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    let rank = |n: &UiNode| {
        let role = n.role.to_lowercase();
        (n.enabled as u8) * 2 + CLICKABLE.iter().any(|c| role == *c) as u8
    };
    let pick = |pred: &dyn Fn(&UiNode) -> bool| {
        nodes
            .iter()
            .filter(|n| n.n > 0 && pred(n))
            .max_by_key(|n| (rank(n), std::cmp::Reverse(n.n)))
            .map(|n| n.n)
    };
    pick(&|n| n.name.trim().to_lowercase() == want)
        .or_else(|| pick(&|n| n.name.to_lowercase().contains(&want)))
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
            "Прочитать окно программы целиком: кнопки, поля, меню и их текст. У каждого элемента номер в квадратных скобках для ui_click и ui_type. После действия ui_click, ui_type и ui_keys сами сообщают, что изменилось, так что читать всё окно заново нужно редко. Если нужного текста (сообщений, документа) в ответе нет, программа рисует его сама: прочитай его screen_text с этим окном.",
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
        let nodes = self.0.tree(w.id).await?;
        if nodes.len() <= 1 {
            return Ok(ToolOutput::new(format!(
                "Окно {} не отдаёт свои элементы. Попробуй клавиши (ui_keys) или посмотри на него (screen_look).",
                label(&w)
            )));
        }
        let process = w.process.to_lowercase();
        let note = if SELF_DRAWN.iter().any(|p| process.starts_with(p)) {
            "\n(Эта программа не отдаёт текст сообщений: прочитать переписку можно screen_text с этим окном, а написать человеку в Telegram — messenger_send.)"
        } else {
            ""
        };
        Ok(ToolOutput::new(format!(
            "Окно {}\n{}{note}",
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

/// Where to click: an element number, a point inside the window, or elements by name.
enum Spot {
    Node(u32),
    Point(i32, i32),
    Names(Vec<String>),
}

fn names_arg(args: &Value) -> Option<Vec<String>> {
    let list: Vec<String> = match (args.get("names"), args.get("name")) {
        (Some(Value::Array(a)), _) => a
            .iter()
            .filter_map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_i64().map(|i| i.to_string()))
            })
            .collect(),
        (_, Some(Value::String(s))) => vec![s.clone()],
        _ => return None,
    };
    let list: Vec<String> = list
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    (!list.is_empty()).then_some(list)
}

fn spot(args: &Value) -> Result<Spot, ToolError> {
    let point = (arg_u64(args, "x"), arg_u64(args, "y"));
    // Small models pass a number and a label together: the number wins, then
    // the labels, then the point.
    match (node_arg(args), point, names_arg(args)) {
        (Some(n), _, _) => Ok(Spot::Node(n)),
        (None, _, Some(names)) if names.len() <= MAX_SEQUENCE => Ok(Spot::Names(names)),
        (None, (Some(x), Some(y)), None) => Ok(Spot::Point(x as i32, y as i32)),
        (None, _, Some(_)) => Err(ToolError::BadArguments(format!(
            "не больше {MAX_SEQUENCE} нажатий за раз"
        ))),
        _ => Err(ToolError::BadArguments(
            "укажи одно: n (номер из ui_read), name или names (подписи элементов) или x и y (точка в окне)".into(),
        )),
    }
}

#[cfg(test)]
mod spot_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_number_wins_over_a_label_and_a_label_over_a_point() {
        assert!(matches!(
            spot(&json!({"n": "17", "name": "Выход"})),
            Ok(Spot::Node(17))
        ));
        assert!(matches!(
            spot(&json!({"name": "Выход", "x": 1, "y": 2})),
            Ok(Spot::Names(_))
        ));
        assert!(matches!(
            spot(&json!({"x": 1, "y": 2})),
            Ok(Spot::Point(1, 2))
        ));
        assert!(spot(&json!({"window": "блокнот"})).is_err());
    }
}

#[async_trait]
impl Tool for UiClickTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "ui_click",
            "Нажать на элемент окна: кнопку, пункт меню, флажок, вкладку. По номеру n из ui_read, по подписи name или цепочкой подписей names — например [\"1\", \"2\", \"Умножить\", \"3\", \"Равно\"] для калькулятора или [\"Файл\", \"Сохранить как\"] для меню. Если у элемента нет ни номера, ни подписи, можно нажать в точку x, y — пиксели от левого верхнего угла окна. В ответе — что изменилось в окне.",
            props(
                &[
                    ("n", json!({ "type": "integer", "description": "Номер элемента из ui_read" })),
                    ("name", json!({ "type": "string", "description": "Подпись элемента" })),
                    ("names", json!({ "type": "array", "items": { "type": "string" }, "description": "Подписи элементов, нажимаются по очереди" })),
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
            Spot::Names(names) => names
                .iter()
                .map(|n| format!("«{}»", truncate(n, 40)))
                .collect::<Vec<_>>()
                .join(", "),
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
        let before = env.foreign_windows();
        if let Spot::Names(names) = &where_ {
            let mut done = Vec::new();
            for name in names {
                let mut nodes = env.known_tree(id).await?;
                let mut n = find_by_name(&nodes, name);
                if n.is_none() {
                    // A menu or page may have opened since the last reading.
                    nodes = env.tree(id).await?;
                    n = find_by_name(&nodes, name);
                }
                let Some(n) = n else {
                    let report = env.outcome(&w, &before).await;
                    return Err(failed(format!(
                        "нет элемента «{name}» в окне {}{}. {report}",
                        label(&w),
                        if done.is_empty() {
                            String::new()
                        } else {
                            format!(" (уже нажала: {})", done.join(", "))
                        }
                    )));
                };
                env.press(id, n, btn, double).await?;
                done.push(format!("«{name}»"));
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            let report = env.outcome(&w, &before).await;
            return Ok(ToolOutput::new(format!(
                "Нажала {}.\n{report}",
                done.join(", ")
            )));
        }
        let ui = env.ui.clone();
        let rect = match where_ {
            Spot::Node(n) => tokio::task::spawn_blocking(move || ui.rect(id, n)),
            _ => tokio::task::spawn_blocking(move || ui.rect(id, 0)),
        }
        .await
        .map_err(|e| failed(e.to_string()))?
        .map_err(failed)?;

        let (x, y, frame) = match where_ {
            Spot::Node(_) | Spot::Names(_) => {
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
                let report = env.outcome(&w, &before).await;
                return Ok(ToolOutput::new(format!("{what}: элемент №{n}.\n{report}")));
            }
        }
        env.windows.focus(id).map_err(failed)?;
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        env.input.click(x, y, btn, double).map_err(failed)?;
        let report = env.outcome(&w, &before).await;
        Ok(ToolOutput::new(format!("Щёлкнула мышью.\n{report}")))
    }
}

pub struct UiTypeTool(pub UiEnv);

#[async_trait]
impl Tool for UiTypeTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "ui_type",
            "Ввести текст в окно программы. С n — в поле с этим номером из ui_read (старый текст поля заменяется). Без n — печатает туда, где стоит курсор. enter=true нажмёт Enter после текста. В мессенджерах (Telegram, WhatsApp, Discord…) так отправляют сообщение: поле сначала очищается, а тот же текст второй раз не уйдёт, так что отправленное не повторяй.",
            props(
                &[
                    ("text", json!({ "type": "string", "description": "Текст" })),
                    ("n", json!({ "type": "integer", "description": "Номер поля из ui_read" })),
                    ("enter", json!({ "type": "boolean", "description": "Нажать Enter после ввода" })),
                    ("clear", json!({ "type": "boolean", "description": "Очистить поле перед вводом (ctrl+a, delete). По умолчанию — только в мессенджере при enter без n" })),
                    ("again", json!({ "type": "boolean", "description": "Отправить тот же текст ещё раз, только если пользователь прямо попросил повторить" })),
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
        let enter = arg_bool(&args, "enter");
        let chat = enter && is_chat(&w.process);
        if chat && !arg_bool(&args, "again") {
            if let Some((sent_to, sent_text, at)) = env.sent.lock().clone() {
                if sent_to == id && sent_text == text && at.elapsed() < RESEND_GUARD {
                    return Err(failed(format!(
                        "этот же текст уже отправлен в это окно {} с назад, второй раз не отправляю. \
Проверь окно (ui_read или screen_text). Если пользователь просит повторить, вызови ui_type с again=true.",
                        at.elapsed().as_secs()
                    )));
                }
            }
        }
        let clear = args
            .get("clear")
            .and_then(Value::as_bool)
            .unwrap_or(chat && node_arg(&args).is_none());
        let before = env.foreign_windows();
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
            if clear {
                env.input.chord(&[VK_CONTROL, 0x41]).map_err(failed)?;
                env.input.chord(&[VK_DELETE]).map_err(failed)?;
            }
            env.type_text(&text).await?;
        }
        if enter {
            if typed_by_pattern {
                env.windows.focus(id).map_err(failed)?;
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            }
            env.input.chord(&[VK_RETURN]).map_err(failed)?;
            *env.sent.lock() = Some((id, text.clone(), std::time::Instant::now()));
        }
        let report = env.outcome(&w, &before).await;
        Ok(ToolOutput::new(format!(
            "Ввела текст в окно {}.\n{report}",
            label(&w)
        )))
    }
}

const VK_RETURN: u16 = 0x0D;
const VK_CONTROL: u16 = 0x11;
const VK_DELETE: u16 = 0x2E;

/// True for characters outside the basic plane: emoji and the like.
fn has_astral(text: &str) -> bool {
    text.chars().any(|c| c as u32 > 0xFFFF)
}

impl UiEnv {
    /// Types the text where the caret is. Text with emoji goes through the
    /// clipboard, which is put back afterwards, after an error too.
    pub(crate) async fn type_text(&self, text: &str) -> Result<(), ToolError> {
        let Some(clip) = self.clipboard.clone().filter(|_| has_astral(text)) else {
            return self.input.text(text).map_err(failed);
        };
        let _loan = crate::cliphist::ClipLoan::start(clip.clone(), &self.clips);
        clip.set(text).map_err(failed)?;
        self.input.chord(&[VK_CONTROL, 0x56]).map_err(failed)?;
        // The program reads the clipboard on its own time.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        Ok(())
    }
}

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

/// Refuses keys that would start one of the assistant's own global hotkeys.
fn refuse_own_hotkeys(chords: &[Vec<u16>]) -> Result<(), ToolError> {
    match chords
        .iter()
        .find_map(|chord| crate::direct::own_hotkey_in(chord))
    {
        Some(combo) => Err(failed(crate::direct::own_hotkey_refusal(&combo))),
        None => Ok(()),
    }
}

impl UiEnv {
    /// Keys go to the window in front, and one chord can change which window
    /// that is: they stop when it became the assistant's own window or one of
    /// the blind zone.
    fn keys_may_land(&self) -> Result<(), ToolError> {
        let Some(front) = self.windows.foreground() else {
            return Ok(());
        };
        if front.process == self.own_process {
            return Err(failed(
                "впереди оказалось моё собственное окно: клавиши в него не нажимаю",
            ));
        }
        if is_blind_title(&front.title) {
            return Err(failed(BLIND_REFUSAL));
        }
        Ok(())
    }
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
        let chords = parse_keys(keys).map_err(ToolError::BadArguments)?;
        refuse_own_hotkeys(&chords)?;
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
        refuse_own_hotkeys(&chords)?;
        let before = self.0.foreign_windows();
        self.0.windows.focus(w.id).map_err(failed)?;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        for chord in &chords {
            self.0.keys_may_land()?;
            self.0.input.chord(chord).map_err(failed)?;
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        }
        let report = self.0.outcome(&w, &before).await;
        Ok(ToolOutput::new(format!(
            "Нажала клавиши в окне {}.\n{report}",
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
            "Разложить окно по экрану: перенести на другой монитор, в левую, правую, верхнюю или нижнюю половину, развернуть, свернуть, вернуть обычный размер. \
Для переноса между мониторами используй её, а не PowerShell.",
            props(
                &[
                    (
                        "placement",
                        json!({ "type": "string", "enum": ["left", "right", "top", "bottom", "maximize", "minimize", "restore"] }),
                    ),
                    (
                        "monitor",
                        json!({ "type": "integer", "description": "Перенести на этот монитор: 1 — основной, дальше слева направо. Без него — где окно сейчас" }),
                    ),
                ],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let w = self.0.target(args)?;
        let monitor = args.get("monitor").and_then(Value::as_u64);
        let raw = arg_str_opt(args, "placement").unwrap_or("");
        if raw.is_empty() && monitor.is_none() {
            return Err(ToolError::BadArguments(
                "нужно placement или monitor".into(),
            ));
        }
        let mut where_ = String::new();
        if let Some(m) = monitor {
            where_.push_str(&format!("на монитор {m}"));
        }
        if !raw.is_empty() {
            let (_, w2) = placement(raw)?;
            if !where_.is_empty() {
                where_.push_str(", ");
            }
            where_.push_str(w2);
        }
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
        let mut done = Vec::new();
        if let Some(m) = args.get("monitor").and_then(Value::as_u64) {
            self.0
                .windows
                .to_monitor(w.id, m as usize)
                .map_err(failed)?;
            done.push(format!("на мониторе {m}"));
        }
        let raw = arg_str_opt(&args, "placement").unwrap_or("");
        if !raw.is_empty() {
            let (p, where_) = placement(raw)?;
            self.0.windows.place(w.id, p).map_err(failed)?;
            done.push(where_.to_string());
        }
        Ok(ToolOutput::new(format!(
            "Окно {} теперь {}.",
            label(&w),
            done.join(", ")
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
            seen: Default::default(),
            clipboard: Some(e.clipboard.clone()),
            sent: Default::default(),
            clips: e.shared.clips.clone(),
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
        let err = UiReadTool(e)
            .assess(&json!({"window": "Calculator"}))
            .unwrap_err()
            .to_string();
        // The answer lists the open windows, without the blind one and without Mika.
        assert!(
            err.contains("«Безымянный — Блокнот» (notepad.exe)"),
            "{err}"
        );
        assert!(!err.contains("банк") && !err.contains("mika.exe"), "{err}");
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
        // Nothing was read before, so the whole window comes back.
        assert!(
            out.contains("[1] Edit «Текстовый редактор» = привет"),
            "{out}"
        );
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
        // A number with a point: the number wins.
        assert!(tool.assess(&json!({"n": 1, "x": 1, "y": 1})).is_ok());
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
    async fn a_chat_message_clears_the_field_pastes_emoji_and_is_sent_once() {
        let (e, f) = ui_env();
        let tool = UiTypeTool(e.clone());
        f.windows.lock().push(WindowInfo {
            id: 9,
            title: "Telegram".into(),
            process: "Telegram.exe".into(),
        });
        *f.clipboard.lock() = "мой буфер".into();
        let msg = json!({"window": "telegram", "text": "Как дела? 😊", "enter": true});
        run(&tool, msg.clone()).await.unwrap();
        assert_eq!(
            *f.chords.lock(),
            vec![
                vec![VK_CONTROL, 0x41],
                vec![VK_DELETE],
                vec![VK_CONTROL, 0x56],
                vec![VK_RETURN]
            ]
        );
        assert!(f.typed.lock().is_empty(), "emoji text is pasted");
        assert_eq!(*f.clipboard.lock(), "мой буфер");
        // The same message again is refused, unless asked for.
        assert!(run(&tool, msg.clone()).await.is_err());
        let mut again = msg;
        again["again"] = json!(true);
        run(&tool, again).await.unwrap();
        // Plain text without Enter is typed where the caret is, nothing cleared.
        f.chords.lock().clear();
        run(&tool, json!({"window": "telegram", "text": "привет"}))
            .await
            .unwrap();
        assert!(f.chords.lock().is_empty());
        assert_eq!(f.typed.lock().last().unwrap(), "привет");
        // Enter in Notepad is a new line: the document is not selected and wiped,
        // and the same line may be typed twice.
        for _ in 0..2 {
            run(
                &tool,
                json!({"window": "блокнот", "text": "строка", "enter": true}),
            )
            .await
            .unwrap();
        }
        assert!(f.chords.lock().iter().all(|c| c == &vec![VK_RETURN]));
    }

    #[tokio::test]
    async fn emoji_typed_through_the_clipboard_are_not_the_users_copies() {
        let (e, f) = ui_env();
        *f.clip_watch.lock() = Some(e.clips.clone());
        *f.clipboard.lock() = "мой пароль".into();
        *f.clip_private.lock() = true;
        e.type_text("Привет 🙂 скоро буду").await.unwrap();
        assert!(e.clips.is_empty(), "{:?}", e.clips.find("", 5));
        assert_eq!(*f.clipboard.lock(), "мой пароль");
        assert!(*f.clip_private.lock(), "the password manager's marks stay");

        // A failed paste puts the clipboard back too.
        *f.chord_error.lock() = Some(vec![VK_CONTROL, 0x56]);
        *f.clipboard.lock() = "мой текст".into();
        *f.clip_private.lock() = false;
        e.clips.pause_for(0);
        assert!(e.type_text("Ещё 🙂").await.is_err());
        assert_eq!(*f.clipboard.lock(), "мой текст");
        assert!(e.clips.is_empty(), "{:?}", e.clips.find("", 5));
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
    async fn keys_never_press_her_own_global_hotkey() {
        crate::direct::set_test_hotkeys();
        let (e, f) = ui_env();
        let tool = UiKeysTool(e);
        // Refused before the user is asked, alone or hidden among other keys.
        for keys in [
            "ctrl+alt+f23",
            "alt+CTRL+F23",
            "ctrl+s ctrl+alt+f23 tab space",
        ] {
            let refused = tool.assess(&json!({ "keys": keys })).unwrap_err();
            assert!(refused.to_string().contains("Ctrl+Alt+F23"), "{refused}");
            assert!(run(&tool, json!({ "keys": keys })).await.is_err());
        }
        assert!(
            f.chords.lock().is_empty(),
            "no key of the sequence is pressed"
        );
        assert!(f.focused.lock().is_empty());
        // Other modifiers are another combination: Windows starts nothing.
        run(&tool, json!({"keys": "ctrl+f23 ctrl+alt+shift+f23"}))
            .await
            .unwrap();
        assert_eq!(f.chords.lock().len(), 2);
    }

    #[tokio::test]
    async fn keys_stop_when_her_own_window_or_a_blind_one_comes_to_front() {
        for (title, process) in [("Мика", "mika.exe"), ("Вход в банк — Chrome", "chrome.exe")]
        {
            let (e, f) = ui_env();
            let tool = UiKeysTool(e);
            let running =
                tokio::spawn(
                    async move { run(&tool, json!({"keys": "ctrl+n tab space enter"})).await },
                );
            // The first chord opened another window in front.
            for _ in 0..2000 {
                if !f.chords.lock().is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            *f.foreground.lock() = Some(WindowInfo {
                id: 99,
                title: title.into(),
                process: process.into(),
            });
            assert!(running.await.unwrap().is_err(), "{process}");
            assert_eq!(
                *f.chords.lock(),
                vec![vec![0x11, 0x4E]],
                "{process}: the rest of the keys must not land in that window"
            );
        }
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
        let out = run(
            &ArrangeWindowTool(e.clone()),
            json!({"window": "notepad", "monitor": 2, "placement": "maximize"}),
        )
        .await
        .unwrap()
        .text;
        assert!(
            out.contains("на мониторе 2, развёрнуто") || out.contains("на мониторе 2"),
            "{out}"
        );
        assert_eq!(*f.moved.lock(), vec![(7, 2)]);
        assert_eq!(f.placed.lock().last(), Some(&(7, Placement::Maximize)));
        assert!(run(
            &ArrangeWindowTool(e.clone()),
            json!({"window": "notepad", "monitor": 3})
        )
        .await
        .is_err());
        assert!(ArrangeWindowTool(e.clone())
            .assess(&json!({"window": "notepad"}))
            .is_err());
        assert!(ArrangeWindowTool(e)
            .assess(&json!({"placement": "sideways"}))
            .is_err());
    }

    #[tokio::test]
    async fn names_are_pressed_in_order_and_changes_reported() {
        let (e, f) = ui_env();
        *f.invoke_result.lock() = Some("Нажала");
        run(&UiReadTool(e.clone()), json!({})).await.unwrap();
        f.tree.lock().push(UiNode {
            n: 3,
            depth: 1,
            role: "Text".into(),
            name: "Результат".into(),
            value: "1000".into(),
            enabled: true,
        });
        let tool = UiClickTool(e.clone());
        let a = tool
            .assess(&json!({"names": ["Файл", "Текстовый"]}))
            .unwrap();
        assert!(a.target.starts_with("«Файл», «Текстовый»"), "{}", a.target);
        let out = run(&tool, json!({"names": ["Файл", "Текстовый"]}))
            .await
            .unwrap()
            .text;
        assert!(out.starts_with("Нажала «Файл», «Текстовый»."), "{out}");
        assert!(out.contains("[3] Text «Результат» = 1000"), "{out}");
        let err = run(&tool, json!({"name": "Сохранить"})).await.unwrap_err();
        assert!(err.to_string().contains("нет элемента «Сохранить»"));
    }

    #[test]
    fn find_by_name_prefers_exact_enabled_buttons() {
        let node = |n, role: &str, name: &str, enabled| UiNode {
            n,
            depth: 1,
            role: role.into(),
            name: name.into(),
            enabled,
            ..Default::default()
        };
        let nodes = vec![
            node(0, "Window", "Калькулятор", true),
            node(1, "Text", "8", true),
            node(2, "Button", "8", true),
            node(3, "Button", "Умножить на", true),
            node(4, "Button", "Сохранить как", false),
            node(5, "MenuItem", "Сохранить как", true),
        ];
        assert_eq!(find_by_name(&nodes, "8"), Some(2));
        assert_eq!(find_by_name(&nodes, "умножить"), Some(3));
        assert_eq!(find_by_name(&nodes, "Сохранить как"), Some(5));
        assert_eq!(
            find_by_name(&nodes, "калькулятор"),
            None,
            "the window itself is not a target"
        );
        assert_eq!(find_by_name(&nodes, "делить"), None);
    }

    #[test]
    fn changes_list_new_and_count_gone_elements() {
        let a = vec![UiNode {
            n: 1,
            depth: 1,
            role: "Text".into(),
            value: "0".into(),
            enabled: true,
            ..Default::default()
        }];
        let b = vec![UiNode {
            n: 1,
            depth: 1,
            role: "Text".into(),
            value: "125".into(),
            enabled: true,
            ..Default::default()
        }];
        let c = changes(&a, &b);
        assert!(
            c.contains("[1] Text = 125") && c.contains("Пропало элементов: 1"),
            "{c}"
        );
        assert_eq!(changes(&b, &b), "Видимых изменений в окне нет.");
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
