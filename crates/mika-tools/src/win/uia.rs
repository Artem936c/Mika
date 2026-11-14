//! UI Automation: the element tree of a window and actions on elements.
//!
//! COM objects stay on one worker thread. A call that gets no answer in time
//! (a button opened a modal dialog and `Invoke` waits for it) abandons that
//! thread and starts a fresh one, so the next call is not stuck behind it.

// The UI Automation constants keep their Windows names.
#![allow(non_upper_case_globals)]

use std::sync::mpsc;
use std::time::Duration;

use parking_lot::Mutex;
use windows::core::BSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::*;

use crate::sys::{Rect, Ui, UiNode};
use crate::ui::MAX_NODES;

const MAX_DEPTH: u8 = 14;
/// Elements visited in one window, listed or not.
const MAX_VISITED: usize = 4000;
const CALL_TIMEOUT: Duration = Duration::from_secs(8);
/// The question about the password field is asked on a key press of the user:
/// it must not hold the action up for long. It usually takes milliseconds; the
/// first one also starts its worker.
const FOCUS_TIMEOUT: Duration = Duration::from_secs(1);
/// A focus that is just moving gives an error once: asked again after this.
const FOCUS_RETRY: Duration = Duration::from_millis(60);

type Reply<T> = mpsc::Sender<Result<T, String>>;

enum Cmd {
    Tree(u64, Reply<Vec<UiNode>>),
    Rect(u64, u32, Reply<Rect>),
    Invoke(u64, u32, Reply<Option<&'static str>>),
    SetText(u64, u32, String, Reply<bool>),
    Focus(u64, u32, Reply<()>),
    FocusedPassword(Reply<Option<bool>>),
}

pub struct UiaUi {
    worker: Mutex<Option<mpsc::Sender<Cmd>>>,
    /// A worker of its own for the question about the password field: it is
    /// asked on the user's key press with a short wait, and must neither stand
    /// behind a long reading of a window nor throw that reading away.
    probe: Mutex<Option<mpsc::Sender<Cmd>>>,
}

impl UiaUi {
    pub fn new() -> Self {
        Self {
            worker: Mutex::new(None),
            probe: Mutex::new(None),
        }
    }

    fn sender(&self) -> mpsc::Sender<Cmd> {
        Self::sender_of(&self.worker, "ui-automation")
    }

    /// The worker behind `slot`, started when there is none.
    fn sender_of(slot: &Mutex<Option<mpsc::Sender<Cmd>>>, name: &str) -> mpsc::Sender<Cmd> {
        let mut slot = slot.lock();
        if let Some(tx) = slot.as_ref() {
            return tx.clone();
        }
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || worker(rx))
            .expect("spawn ui automation thread");
        *slot = Some(tx.clone());
        tx
    }

    /// Sends one command and waits for its answer. `None` means no answer in time.
    fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Cmd) -> Option<Result<T, String>> {
        let (tx, rx) = mpsc::channel();
        if self.sender().send(make(tx)).is_err() {
            // The worker died; the next call starts a new one.
            *self.worker.lock() = None;
            return Some(Err("служба чтения окон остановилась, повтори".into()));
        }
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(r) => Some(r),
            Err(_) => {
                *self.worker.lock() = None;
                None
            }
        }
    }
}

const NO_ANSWER: &str = "окно не ответило за 8 секунд";

impl Ui for UiaUi {
    fn tree(&self, window: u64) -> Result<Vec<UiNode>, String> {
        self.call(|r| Cmd::Tree(window, r))
            .unwrap_or_else(|| Err(NO_ANSWER.into()))
    }

    fn rect(&self, window: u64, n: u32) -> Result<Rect, String> {
        self.call(|r| Cmd::Rect(window, n, r))
            .unwrap_or_else(|| Err(NO_ANSWER.into()))
    }

    fn invoke(&self, window: u64, n: u32) -> Result<Option<&'static str>, String> {
        // No answer here usually means the press opened a modal dialog.
        self.call(|r| Cmd::Invoke(window, n, r))
            .unwrap_or(Ok(Some("Нажала, программа открыла окно и ждёт")))
    }

    fn set_text(&self, window: u64, n: u32, text: &str) -> Result<bool, String> {
        self.call(|r| Cmd::SetText(window, n, text.to_string(), r))
            .unwrap_or_else(|| Err(NO_ANSWER.into()))
    }

    fn focus(&self, window: u64, n: u32) -> Result<(), String> {
        self.call(|r| Cmd::Focus(window, n, r))
            .unwrap_or_else(|| Err(NO_ANSWER.into()))
    }

    fn focused_password(&self) -> Option<bool> {
        let (tx, rx) = mpsc::channel();
        let sent = Self::sender_of(&self.probe, "ui-automation-focus")
            .send(Cmd::FocusedPassword(tx))
            .is_ok();
        // No answer in time, or no worker: nobody knows what the field is, and
        // the caller must not take that for "an ordinary field". A worker that
        // hangs in a program is left behind; the next question starts a new one.
        let answer = sent.then(|| rx.recv_timeout(FOCUS_TIMEOUT).ok()).flatten();
        match answer {
            Some(known) => known.ok().flatten(),
            None => {
                *self.probe.lock() = None;
                None
            }
        }
    }
}

/// Elements of the last read window, by their numbers.
struct Snapshot {
    window: u64,
    elements: Vec<IUIAutomationElement>,
}

fn hwnd(id: u64) -> HWND {
    HWND(id as usize as *mut core::ffi::c_void)
}

fn err(e: windows::core::Error) -> String {
    let m = e.message();
    if m.is_empty() {
        format!("ошибка Windows {:#x}", e.code().0)
    } else {
        m
    }
}

fn worker(rx: mpsc::Receiver<Cmd>) {
    // SAFETY: COM is initialised for this thread, which lives as long as the channel.
    let automation: Result<IUIAutomation, String> = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(err)
    };
    let mut snapshot: Option<Snapshot> = None;
    while let Ok(cmd) = rx.recv() {
        let uia = match &automation {
            Ok(a) => a,
            Err(e) => {
                let msg = format!("UI Automation недоступна: {e}");
                match cmd {
                    Cmd::Tree(_, r) => drop(r.send(Err(msg))),
                    Cmd::Rect(_, _, r) => drop(r.send(Err(msg))),
                    Cmd::Invoke(_, _, r) => drop(r.send(Err(msg))),
                    Cmd::SetText(_, _, _, r) => drop(r.send(Err(msg))),
                    Cmd::Focus(_, _, r) => drop(r.send(Err(msg))),
                    Cmd::FocusedPassword(r) => drop(r.send(Err(msg))),
                }
                continue;
            }
        };
        match cmd {
            Cmd::Tree(window, r) => {
                let out = read_tree(uia, window).map(|(nodes, elements)| {
                    snapshot = Some(Snapshot { window, elements });
                    nodes
                });
                let _ = r.send(out);
            }
            Cmd::Rect(window, n, r) => {
                let _ = r.send(element(uia, &snapshot, window, n).and_then(|e| rect_of(&e)));
            }
            Cmd::Invoke(window, n, r) => {
                let _ = r.send(element(uia, &snapshot, window, n).and_then(|e| invoke(&e)));
            }
            Cmd::SetText(window, n, text, r) => {
                let _ =
                    r.send(element(uia, &snapshot, window, n).and_then(|e| set_text(&e, &text)));
            }
            Cmd::Focus(window, n, r) => {
                let _ = r.send(
                    element(uia, &snapshot, window, n)
                        // SAFETY: plain COM call on a live element.
                        .and_then(|e| unsafe { e.SetFocus() }.map_err(err)),
                );
            }
            Cmd::FocusedPassword(r) => {
                let known = focused_password(uia).or_else(|| {
                    std::thread::sleep(FOCUS_RETRY);
                    focused_password(uia)
                });
                let _ = r.send(Ok(known));
            }
        }
    }
}

/// Whether the keyboard focus is in a password field. `None` on an error (no
/// focus, a program that does not answer): then it is not known.
fn focused_password(uia: &IUIAutomation) -> Option<bool> {
    // SAFETY: plain COM calls; the element is owned by this thread.
    unsafe {
        uia.GetFocusedElement()
            .and_then(|e| e.CurrentIsPassword())
            .ok()
            .map(|p| p.as_bool())
    }
}

fn element(
    uia: &IUIAutomation,
    snapshot: &Option<Snapshot>,
    window: u64,
    n: u32,
) -> Result<IUIAutomationElement, String> {
    if n == 0 {
        // SAFETY: a stale handle makes the call fail, nothing more.
        return unsafe { uia.ElementFromHandle(hwnd(window)) }
            .map_err(|_| "окно закрыто или недоступно".to_string());
    }
    snapshot
        .as_ref()
        .filter(|s| s.window == window)
        .and_then(|s| s.elements.get(n as usize))
        .cloned()
        .ok_or_else(|| {
            format!(
                "нет элемента №{n}: сначала прочитай это окно через ui_read и возьми номер оттуда"
            )
        })
}

fn rect_of(e: &IUIAutomationElement) -> Result<Rect, String> {
    // SAFETY: plain COM property read.
    let r = unsafe { e.CurrentBoundingRectangle() }.map_err(err)?;
    let rect = Rect {
        x: r.left,
        y: r.top,
        w: r.right - r.left,
        h: r.bottom - r.top,
    };
    if rect.w <= 0 || rect.h <= 0 {
        return Err("элемент сейчас не виден на экране".into());
    }
    Ok(rect)
}

fn role(id: UIA_CONTROLTYPE_ID) -> &'static str {
    match id {
        UIA_ButtonControlTypeId => "Button",
        UIA_CalendarControlTypeId => "Calendar",
        UIA_CheckBoxControlTypeId => "CheckBox",
        UIA_ComboBoxControlTypeId => "ComboBox",
        UIA_EditControlTypeId => "Edit",
        UIA_HyperlinkControlTypeId => "Link",
        UIA_ImageControlTypeId => "Image",
        UIA_ListItemControlTypeId => "ListItem",
        UIA_ListControlTypeId => "List",
        UIA_MenuControlTypeId => "Menu",
        UIA_MenuBarControlTypeId => "MenuBar",
        UIA_MenuItemControlTypeId => "MenuItem",
        UIA_ProgressBarControlTypeId => "ProgressBar",
        UIA_RadioButtonControlTypeId => "RadioButton",
        UIA_ScrollBarControlTypeId => "ScrollBar",
        UIA_SliderControlTypeId => "Slider",
        UIA_SpinnerControlTypeId => "Spinner",
        UIA_StatusBarControlTypeId => "StatusBar",
        UIA_TabControlTypeId => "Tab",
        UIA_TabItemControlTypeId => "TabItem",
        UIA_TextControlTypeId => "Text",
        UIA_ToolBarControlTypeId => "ToolBar",
        UIA_ToolTipControlTypeId => "ToolTip",
        UIA_TreeControlTypeId => "Tree",
        UIA_TreeItemControlTypeId => "TreeItem",
        UIA_GroupControlTypeId => "Group",
        UIA_ThumbControlTypeId => "Thumb",
        UIA_DataGridControlTypeId => "DataGrid",
        UIA_DataItemControlTypeId => "DataItem",
        UIA_DocumentControlTypeId => "Document",
        UIA_SplitButtonControlTypeId => "SplitButton",
        UIA_WindowControlTypeId => "Window",
        UIA_PaneControlTypeId => "Pane",
        UIA_HeaderControlTypeId => "Header",
        UIA_HeaderItemControlTypeId => "HeaderItem",
        UIA_TableControlTypeId => "Table",
        UIA_TitleBarControlTypeId => "TitleBar",
        UIA_SeparatorControlTypeId => "Separator",
        _ => "Custom",
    }
}

/// Containers that say nothing by themselves: listed only when they have a name.
fn is_filler(id: UIA_CONTROLTYPE_ID) -> bool {
    matches!(
        id,
        UIA_PaneControlTypeId
            | UIA_GroupControlTypeId
            | UIA_CustomControlTypeId
            | UIA_SeparatorControlTypeId
            | UIA_ThumbControlTypeId
            | UIA_ScrollBarControlTypeId
            | UIA_TitleBarControlTypeId
    )
}

/// Text of an edit field or the state of a switch. Password fields give nothing.
fn value_of(e: &IUIAutomationElement) -> String {
    // SAFETY: plain COM property reads; a missing pattern is an error we ignore.
    unsafe {
        if e.CurrentIsPassword().is_ok_and(|p| p.as_bool()) {
            return "(пароль скрыт)".into();
        }
        if let Ok(v) = e.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
            if let Ok(text) = v.CurrentValue() {
                if !text.is_empty() {
                    return text.to_string();
                }
            }
        }
        // A drop-down list of WinUI (Settings → Scale) has no value: its
        // choice is the selected item, named «100% (рекомендуется)».
        if let Ok(s) =
            e.GetCurrentPatternAs::<IUIAutomationSelectionPattern>(UIA_SelectionPatternId)
        {
            if let Ok(chosen) = s.GetCurrentSelection() {
                let names: Vec<String> = (0..chosen.Length().unwrap_or(0))
                    .filter_map(|i| chosen.GetElement(i).ok())
                    .filter_map(|el| el.CurrentName().ok())
                    .map(|n| n.to_string())
                    .filter(|n| !n.is_empty())
                    .collect();
                if !names.is_empty() {
                    return names.join(", ");
                }
            }
        }
        if let Ok(t) = e.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId) {
            if let Ok(state) = t.CurrentToggleState() {
                return if state == ToggleState_On {
                    "включён".into()
                } else {
                    "выключен".into()
                };
            }
        }
        if let Ok(s) =
            e.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
        {
            if s.CurrentIsSelected().is_ok_and(|b| b.as_bool()) {
                return "выбран".into();
            }
        }
    }
    String::new()
}

fn read_tree(
    uia: &IUIAutomation,
    window: u64,
) -> Result<(Vec<UiNode>, Vec<IUIAutomationElement>), String> {
    // SAFETY: plain COM calls; every element is owned by this thread.
    unsafe {
        let root = uia
            .ElementFromHandle(hwnd(window))
            .map_err(|_| "окно закрыто или недоступно".to_string())?;
        let walker = uia.ControlViewWalker().map_err(err)?;
        let mut nodes = Vec::new();
        let mut elements = Vec::new();
        let mut visited = 0usize;
        // Depth-first without recursion: (element, depth shown to the model).
        let mut stack: Vec<(IUIAutomationElement, u8)> = vec![(root, 0)];
        while let Some((el, depth)) = stack.pop() {
            visited += 1;
            if nodes.len() >= MAX_NODES || visited > MAX_VISITED {
                break;
            }
            let kind = el.CurrentControlType().unwrap_or(UIA_CustomControlTypeId);
            let name = el
                .CurrentName()
                .map(|b: BSTR| b.to_string())
                .unwrap_or_default();
            let listed = nodes.is_empty() || !(is_filler(kind) && name.trim().is_empty());
            if listed {
                nodes.push(UiNode {
                    n: nodes.len() as u32,
                    depth,
                    role: role(kind).into(),
                    name: name.trim().replace(['\r', '\n'], " "),
                    value: value_of(&el).trim().replace(['\r', '\n'], " "),
                    enabled: el.CurrentIsEnabled().is_ok_and(|b| b.as_bool()),
                });
                elements.push(el.clone());
            }
            let child_depth = if listed { depth + 1 } else { depth };
            if child_depth > MAX_DEPTH {
                continue;
            }
            // Children go on the stack in reverse, so the first child is read first.
            let mut children = Vec::new();
            let mut next = walker.GetFirstChildElement(&el).ok();
            while let Some(child) = next {
                if children.len() >= 300 {
                    break;
                }
                next = walker.GetNextSiblingElement(&child).ok();
                children.push(child);
            }
            for child in children.into_iter().rev() {
                stack.push((child, child_depth));
            }
        }
        Ok((nodes, elements))
    }
}

/// Presses the element through its pattern. A pattern that refuses (a menu
/// that closed under it: «An event was unable to invoke any of the
/// subscribers») gives `None`, and the caller clicks with the mouse instead.
fn invoke(e: &IUIAutomationElement) -> Result<Option<&'static str>, String> {
    // SAFETY: plain COM calls on a live element.
    unsafe {
        if !e.CurrentIsEnabled().is_ok_and(|b| b.as_bool()) {
            return Err("элемент недоступен: программа его отключила".into());
        }
        if let Ok(p) = e.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId) {
            if p.Invoke().is_ok() {
                return Ok(Some("Нажала"));
            }
            return Ok(None);
        }
        if let Ok(p) = e.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId) {
            if p.Toggle().is_ok() {
                return Ok(Some("Переключила"));
            }
            return Ok(None);
        }
        if let Ok(p) =
            e.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
        {
            if p.Select().is_ok() {
                return Ok(Some("Выбрала"));
            }
            return Ok(None);
        }
        if let Ok(p) =
            e.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
        {
            let open = p
                .CurrentExpandCollapseState()
                .is_ok_and(|s| s == ExpandCollapseState_Expanded);
            if open {
                return Ok(p.Collapse().is_ok().then_some("Свернула"));
            }
            return Ok(p.Expand().is_ok().then_some("Раскрыла"));
        }
    }
    Ok(None)
}

fn set_text(e: &IUIAutomationElement, text: &str) -> Result<bool, String> {
    // SAFETY: plain COM calls on a live element.
    unsafe {
        if e.CurrentIsPassword().is_ok_and(|p| p.as_bool()) {
            return Err("это поле пароля, я в такие не печатаю".into());
        }
        let Ok(p) = e.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) else {
            return Ok(false);
        };
        if p.CurrentIsReadOnly().is_ok_and(|b| b.as_bool()) {
            return Ok(false);
        }
        // Some editors expose the pattern and still refuse the value: type instead.
        Ok(p.SetValue(&BSTR::from(text)).is_ok())
    }
}
