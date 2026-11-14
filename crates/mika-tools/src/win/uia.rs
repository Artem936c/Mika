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

use crate::sys::{Rect, TabInfo, Ui, UiNode};
use crate::ui::MAX_NODES;

const MAX_DEPTH: u8 = 14;
/// Elements visited in one window, listed or not.
const MAX_VISITED: usize = 4000;
/// Elements visited in search of the tabs of a window. Its own interface is
/// small once the pages are left out.
const TAB_MAX_VISITED: usize = 2000;
const TAB_MAX_DEPTH: u8 = 14;
/// Tabs kept from one window.
const MAX_TABS: usize = 500;
/// Children read from one element in search of tabs: a tab strip holds all
/// its tabs as children.
const TAB_MAX_CHILDREN: usize = 600;
/// How long a tab is given to become the open one after it was chosen.
const TAB_SETTLE: Duration = Duration::from_millis(60);
const TAB_SETTLE_TRIES: u32 = 5;
/// `STATE_SYSTEM_SELECTED` of the older accessibility interface.
const LEGACY_SELECTED: u32 = 0x2;
/// `STATE_SYSTEM_SELECTABLE`: the element can be selected at all.
const LEGACY_SELECTABLE: u32 = 0x20_0000;
const CALL_TIMEOUT: Duration = Duration::from_secs(8);
/// The tabs of a window are read in well under a second. A window that takes
/// longer does not answer, and several windows may be asked in one call.
const TAB_TIMEOUT: Duration = Duration::from_secs(4);
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
    /// The tabs and whether some were left out.
    Tabs(u64, Reply<(Vec<TabInfo>, bool)>),
    SelectTab(u64, usize, Reply<()>),
}

pub struct UiaUi {
    /// The window whose last list of tabs was cut short.
    tabs_cut: Mutex<Option<u64>>,
    worker: Mutex<Option<mpsc::Sender<Cmd>>>,
    /// A worker of its own for the question about the password field: it is
    /// asked on the user's key press with a short wait, and must neither stand
    /// behind a long reading of a window nor throw that reading away.
    probe: Mutex<Option<mpsc::Sender<Cmd>>>,
}

impl UiaUi {
    pub fn new() -> Self {
        Self {
            tabs_cut: Mutex::new(None),
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
        self.call_within(CALL_TIMEOUT, make)
    }

    /// The same with its own wait. A worker that did not answer in time is
    /// left behind, and the next call starts a new one.
    fn call_within<T>(
        &self,
        wait: Duration,
        make: impl FnOnce(Reply<T>) -> Cmd,
    ) -> Option<Result<T, String>> {
        let (tx, rx) = mpsc::channel();
        if self.sender().send(make(tx)).is_err() {
            // The worker died; the next call starts a new one.
            *self.worker.lock() = None;
            return Some(Err("служба чтения окон остановилась, повтори".into()));
        }
        match rx.recv_timeout(wait) {
            Ok(r) => Some(r),
            Err(_) => {
                *self.worker.lock() = None;
                None
            }
        }
    }
}

const NO_ANSWER: &str = "окно не ответило за 8 секунд";
const TABS_NO_ANSWER: &str = "окно не ответило за 4 секунды";

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

    fn tabs(&self, window: u64) -> Result<Vec<TabInfo>, String> {
        let answer = self
            .call_within(TAB_TIMEOUT, |r| Cmd::Tabs(window, r))
            .unwrap_or_else(|| Err(TABS_NO_ANSWER.into()));
        let mut cut_window = self.tabs_cut.lock();
        if *cut_window == Some(window) {
            *cut_window = None;
        }
        let (tabs, cut) = answer?;
        if cut {
            *cut_window = Some(window);
        }
        Ok(tabs)
    }

    fn tabs_cut(&self, window: u64) -> bool {
        *self.tabs_cut.lock() == Some(window)
    }

    fn select_tab(&self, window: u64, index: usize) -> Result<(), String> {
        self.call(|r| Cmd::SelectTab(window, index, r))
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

/// Tabs of the window whose tabs were listed last, in the listed order. Kept
/// apart from `Snapshot`: listing tabs must not move the element numbers the
/// model got from `ui_read`.
struct TabSnapshot {
    window: u64,
    elements: Vec<IUIAutomationElement>,
    /// The program tells which tab is open (`open_tab_is_told`).
    told: bool,
}

const STALE_TABS: &str = "список вкладок устарел";

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
    let mut tab_snapshot: Option<TabSnapshot> = None;
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
                    Cmd::Tabs(_, r) => drop(r.send(Err(msg))),
                    Cmd::SelectTab(_, _, r) => drop(r.send(Err(msg))),
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
            Cmd::Tabs(window, r) => {
                // A failed reading leaves no tabs to choose from.
                tab_snapshot = None;
                let out = read_tabs(uia, window).map(|read| {
                    tab_snapshot = Some(TabSnapshot {
                        window,
                        elements: read.elements,
                        told: open_tab_is_told(&read.tabs),
                    });
                    (read.tabs, read.cut)
                });
                let _ = r.send(out);
            }
            Cmd::SelectTab(window, index, r) => {
                let tab = tab_snapshot
                    .as_ref()
                    .filter(|s| s.window == window)
                    .and_then(|s| s.elements.get(index).map(|e| (e.clone(), s.told)))
                    .ok_or_else(|| STALE_TABS.to_string());
                let _ = r.send(tab.and_then(|(e, told)| select_tab(&e, told)));
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

/// Children of an element in the control view, in their order, and whether
/// some were left out.
///
/// # Safety
/// `el` must be a live element of the thread that owns `walker`.
unsafe fn children_of(
    walker: &IUIAutomationTreeWalker,
    el: &IUIAutomationElement,
) -> (Vec<IUIAutomationElement>, bool) {
    let mut children = Vec::new();
    // SAFETY: plain COM calls; the caller keeps the element on its thread.
    let mut next = unsafe { walker.GetFirstChildElement(el) }.ok();
    while let Some(child) = next {
        if children.len() >= TAB_MAX_CHILDREN {
            return (children, true);
        }
        // SAFETY: as above.
        next = unsafe { walker.GetNextSiblingElement(&child) }.ok();
        children.push(child);
    }
    (children, false)
}

/// What the state of the older accessibility interface tells about a tab:
/// open, closed, or nothing. A missing "selected" bit means "closed" only for
/// an element that can be selected at all; many programs set neither bit.
fn legacy_selected(state: u32) -> Option<bool> {
    if state & LEGACY_SELECTED != 0 {
        Some(true)
    } else if state & LEGACY_SELECTABLE != 0 {
        Some(false)
    } else {
        None
    }
}

/// Whether the tab is the open one; `None` when the program does not tell.
fn tab_selected(e: &IUIAutomationElement) -> Option<bool> {
    // SAFETY: plain COM property reads; a missing pattern is an error we ignore.
    unsafe {
        if let Ok(p) =
            e.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
        {
            if let Ok(selected) = p.CurrentIsSelected() {
                return Some(selected.as_bool());
            }
        }
        // Programs seen through the older interface tell it as a state bit.
        if let Ok(p) = e.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
            UIA_LegacyIAccessiblePatternId,
        ) {
            if let Ok(state) = p.CurrentState() {
                return legacy_selected(state);
            }
        }
    }
    None
}

/// What one reading of a window's tabs gives.
struct TabsRead {
    tabs: Vec<TabInfo>,
    elements: Vec<IUIAutomationElement>,
    /// Some tabs may be missing: a limit was reached.
    cut: bool,
}

/// Tabs of the window's own interface with their elements, in the order of
/// the tab strip. A tab is known by its control type alone, whatever
/// patterns it has. Documents are not entered: a web page has tabs of its own
/// and thousands of elements, and it is not the window's interface.
fn read_tabs(uia: &IUIAutomation, window: u64) -> Result<TabsRead, String> {
    // SAFETY: plain COM calls; every element is owned by this thread.
    unsafe {
        let root = uia
            .ElementFromHandle(hwnd(window))
            .map_err(|_| "окно закрыто или недоступно".to_string())?;
        let walker = uia.ControlViewWalker().map_err(err)?;
        let mut read = TabsRead {
            tabs: Vec::new(),
            elements: Vec::new(),
            cut: false,
        };
        let mut visited = 0usize;
        // Depth-first without recursion, so tabs come in their order on screen.
        let mut stack: Vec<(IUIAutomationElement, u8)> = vec![(root, 0)];
        while let Some((el, depth)) = stack.pop() {
            visited += 1;
            if visited > TAB_MAX_VISITED || read.tabs.len() >= MAX_TABS {
                read.cut = true;
                break;
            }
            let kind = el.CurrentControlType().unwrap_or(UIA_CustomControlTypeId);
            if kind == UIA_DocumentControlTypeId && depth > 0 {
                continue;
            }
            if kind == UIA_TabItemControlTypeId {
                let name = el
                    .CurrentName()
                    .map(|b: BSTR| b.to_string())
                    .unwrap_or_default();
                // A tab without a name keeps its place: the caller skips it.
                read.tabs.push(TabInfo {
                    name: name.trim().replace(['\r', '\n'], " "),
                    selected: tab_selected(&el),
                });
                read.elements.push(el);
                // Inside a tab are its close button and its icon.
                continue;
            }
            if depth >= TAB_MAX_DEPTH {
                continue;
            }
            let (children, cut) = children_of(&walker, &el);
            read.cut |= cut;
            for child in children.into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
        Ok(read)
    }
}

/// Whether the program tells which tab is open: exactly one says "I am".
/// None means it does not tell; several mean the mark is something else (in
/// Chromium, the tabs picked with Ctrl), and it is not trusted either.
fn open_tab_is_told(tabs: &[TabInfo]) -> bool {
    tabs.iter().filter(|t| t.selected == Some(true)).count() == 1
}

/// Tries the ways to open a tab in order until one is accepted and the tab
/// is seen open. `accepted`: runs a way, true when the program took it.
/// `opened`: the tab is the open one now, or nobody can tell.
fn open_by(
    ways: usize,
    accepted: &mut dyn FnMut(usize) -> bool,
    opened: &mut dyn FnMut() -> bool,
) -> Result<(), String> {
    let mut any = false;
    for way in 0..ways {
        if accepted(way) {
            any = true;
            if opened() {
                return Ok(());
            }
        }
    }
    Err(if any {
        "программа приняла выбор вкладки, но вкладка не открылась".into()
    } else {
        "программа не даёт выбрать вкладку".into()
    })
}

/// A way to open a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabWay {
    /// `SelectionItemPattern.Select`.
    Select,
    /// `InvokePattern.Invoke`: a press.
    Invoke,
    /// The default action of the older accessibility interface.
    Default,
}

/// The ways in the order they are tried. A tab that already carries the
/// "selected" mark and still has to be opened (the mark is not trusted, see
/// `open_tab_is_told`) is pressed first: selecting what is selected does
/// nothing.
fn tab_ways(marked_selected: bool) -> [TabWay; 3] {
    if marked_selected {
        [TabWay::Invoke, TabWay::Default, TabWay::Select]
    } else {
        [TabWay::Select, TabWay::Invoke, TabWay::Default]
    }
}

/// Opens the tab: by selection, else by a press, else by the default action
/// of the older interface. A way that the program accepts and that leaves the
/// tab closed does not count: the next one is tried. `told`: the program
/// tells which tab is open; when it does not, the first accepted way is
/// taken at its word.
fn select_tab(e: &IUIAutomationElement, told: bool) -> Result<(), String> {
    let ways = tab_ways(tab_selected(e) == Some(true));
    let mut accepted = |way: usize| {
        // SAFETY: plain COM calls on a live element; a missing pattern is an error.
        unsafe {
            match ways[way] {
                TabWay::Select => e
                    .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                        UIA_SelectionItemPatternId,
                    )
                    .and_then(|p| p.Select())
                    .is_ok(),
                TabWay::Invoke => e
                    .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                    .and_then(|p| p.Invoke())
                    .is_ok(),
                TabWay::Default => e
                    .GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                        UIA_LegacyIAccessiblePatternId,
                    )
                    .and_then(|p| p.DoDefaultAction())
                    .is_ok(),
            }
        }
    };
    let mut opened = || {
        if !told {
            return true;
        }
        for attempt in 0..TAB_SETTLE_TRIES {
            if attempt > 0 {
                std::thread::sleep(TAB_SETTLE);
            }
            if tab_selected(e) != Some(false) {
                return true;
            }
        }
        false
    };
    open_by(ways.len(), &mut accepted, &mut opened)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_is_chosen_only_from_the_tabs_that_were_just_listed() {
        // Nothing on the desktop is touched: the handle is no window.
        let ui = UiaUi::new();
        assert!(ui.tabs(0).is_err(), "no window, no tabs");
        let stale = ui.select_tab(0, 0).unwrap_err();
        // A session without UI Automation says so instead.
        assert!(
            stale == STALE_TABS || stale.starts_with("UI Automation"),
            "{stale}"
        );
        assert!(!ui.tabs_cut(0));
    }

    #[test]
    fn the_old_interface_tells_the_open_tab_only_when_it_can() {
        assert_eq!(legacy_selected(LEGACY_SELECTED), Some(true));
        assert_eq!(
            legacy_selected(LEGACY_SELECTED | LEGACY_SELECTABLE),
            Some(true)
        );
        assert_eq!(legacy_selected(LEGACY_SELECTABLE), Some(false));
        // Neither bit: the program does not tell, the tab is not "closed".
        assert_eq!(legacy_selected(0), None);
        assert_eq!(legacy_selected(0x10_0004), None);
    }

    fn told(marks: &[Option<bool>]) -> bool {
        let tabs: Vec<TabInfo> = marks
            .iter()
            .map(|selected| TabInfo {
                name: "tab".into(),
                selected: *selected,
            })
            .collect();
        open_tab_is_told(&tabs)
    }

    #[test]
    fn the_open_tab_is_told_by_exactly_one_mark() {
        assert!(told(&[Some(false), Some(true), Some(false)]));
        assert!(told(&[None, Some(true)]));
        // Nobody is marked.
        assert!(!told(&[Some(false), Some(false)]));
        assert!(!told(&[None, None]));
        assert!(!told(&[]));
        // Several are: the mark is not "open" (tabs picked with Ctrl).
        assert!(!told(&[Some(true), Some(true), Some(false)]));
    }

    /// Runs `open_by` over a script: which ways the program accepts and after
    /// which way the tab is seen open. Returns the ways that were run.
    fn opening(accepts: [bool; 3], opens_after: Option<usize>) -> (Result<(), String>, Vec<usize>) {
        let ran = std::cell::RefCell::new(Vec::new());
        let mut accepted = |way: usize| {
            ran.borrow_mut().push(way);
            accepts[way]
        };
        let mut opened = || opens_after.is_some_and(|way| ran.borrow().last() >= Some(&way));
        let out = open_by(3, &mut accepted, &mut opened);
        (out, ran.into_inner())
    }

    #[test]
    fn the_first_way_that_opens_the_tab_ends_the_tries() {
        assert_eq!(opening([true, true, true], Some(0)), (Ok(()), vec![0]));
        // The first is refused, the second opens it.
        assert_eq!(opening([false, true, true], Some(1)), (Ok(()), vec![0, 1]));
        // The first is accepted and opens nothing: the next one is tried.
        assert_eq!(opening([true, true, false], Some(1)), (Ok(()), vec![0, 1]));
    }

    #[test]
    fn a_tab_that_never_opens_is_an_error_that_tells_why() {
        let (out, ran) = opening([true, false, true], None);
        assert_eq!(ran, vec![0, 1, 2]);
        assert_eq!(
            out.unwrap_err(),
            "программа приняла выбор вкладки, но вкладка не открылась"
        );
        let (out, ran) = opening([false, false, false], Some(0));
        assert_eq!(ran, vec![0, 1, 2]);
        assert_eq!(out.unwrap_err(), "программа не даёт выбрать вкладку");
    }

    #[test]
    fn a_tab_that_is_marked_selected_is_pressed_before_it_is_selected() {
        assert_eq!(
            tab_ways(false),
            [TabWay::Select, TabWay::Invoke, TabWay::Default]
        );
        assert_eq!(
            tab_ways(true),
            [TabWay::Invoke, TabWay::Default, TabWay::Select]
        );
    }

    /// The first window of a known browser whose title is not of the blind zone.
    fn a_browser_window() -> Option<u64> {
        use crate::sys::Windows;
        super::super::wnd::WinWindows
            .list()
            .into_iter()
            .find(|w| {
                crate::apps::BROWSERS.contains(&w.process.to_lowercase().as_str())
                    && !mika_core::policy::blind::is_blind_title(&w.title)
            })
            .map(|w| w.id)
    }

    #[test]
    #[ignore = "reads the tab strip of a browser window of the live desktop"]
    fn a_live_browser_window_shows_its_tabs_and_one_of_them_is_open() {
        let Some(window) = a_browser_window() else {
            eprintln!("no browser window is open");
            return;
        };
        let ui = UiaUi::new();
        let tabs = ui.tabs(window).expect("the tabs are read");
        let open = tabs.iter().filter(|t| t.selected == Some(true)).count();
        let unnamed = tabs.iter().filter(|t| t.name.is_empty()).count();
        let untold = tabs.iter().filter(|t| t.selected.is_none()).count();
        // Counts only: the names are the user's pages.
        eprintln!(
            "tabs: {}, open: {open}, without a name: {unnamed}, state not told: {untold}, cut: {}",
            tabs.len(),
            ui.tabs_cut(window)
        );
        assert!(!tabs.is_empty(), "the browser shows no tabs");
        assert_eq!(open, 1, "exactly one tab is the open one");
    }

    #[test]
    #[ignore = "chooses the already open tab of a browser window of the live desktop"]
    fn choosing_the_open_tab_of_a_live_browser_window_is_accepted() {
        let Some(window) = a_browser_window() else {
            eprintln!("no browser window is open");
            return;
        };
        let ui = UiaUi::new();
        let tabs = ui.tabs(window).expect("the tabs are read");
        let Some(open) = tabs.iter().position(|t| t.selected == Some(true)) else {
            panic!("no tab is marked open among {}", tabs.len());
        };
        // The page in front stays the same: the open tab is chosen again.
        ui.select_tab(window, open).expect("the choice is accepted");
        let after = ui.tabs(window).expect("the tabs are read again");
        assert_eq!(after.get(open).and_then(|t| t.selected), Some(true));
    }
}
