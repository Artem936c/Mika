//! Programs and windows: open, list, switch, close.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::sys::{Launcher, WindowInfo, Windows};
use crate::util::{failed, schema};

/// Russian and short names to what the shell can open.
const ALIASES: &[(&str, &str)] = &[
    ("блокнот", "notepad.exe"),
    ("notepad", "notepad.exe"),
    ("калькулятор", "calc.exe"),
    ("calculator", "calc.exe"),
    ("проводник", "explorer.exe"),
    ("explorer", "explorer.exe"),
    ("paint", "mspaint.exe"),
    ("пейнт", "mspaint.exe"),
    ("терминал", "wt.exe"),
    ("командная строка", "cmd.exe"),
    ("cmd", "cmd.exe"),
    ("powershell", "powershell.exe"),
    ("диспетчер задач", "taskmgr.exe"),
    ("параметры", "ms-settings:"),
    ("настройки windows", "ms-settings:"),
    ("панель управления", "control.exe"),
    ("ножницы", "snippingtool.exe"),
    ("удалённый рабочий стол", "mstsc.exe"),
    ("удаленный рабочий стол", "mstsc.exe"),
    ("редактор реестра", "regedit.exe"),
];

/// Russian names of common programs to words in their Start menu shortcuts.
const SHORTCUT_HINTS: &[(&str, &str)] = &[
    ("хром", "chrome"),
    ("гугл хром", "chrome"),
    ("эдж", "edge"),
    ("файрфокс", "firefox"),
    ("фаерфокс", "firefox"),
    ("яндекс браузер", "yandex"),
    ("ворд", "word"),
    ("эксель", "excel"),
    ("телеграм", "telegram"),
    ("дискорд", "discord"),
    ("стим", "steam"),
    ("спотифай", "spotify"),
    ("вскод", "visual studio code"),
    ("vs code", "visual studio code"),
];

pub struct OpenAppTool {
    pub launcher: Arc<dyn Launcher>,
    /// Folders with Start menu shortcuts.
    pub shortcut_dirs: Vec<PathBuf>,
    /// With windows, the tool waits for the program's window and names it.
    pub windows: Option<Arc<dyn Windows>>,
    /// Executable of the assistant: its own windows do not count.
    pub own_process: String,
    pub wait: std::time::Duration,
}

/// Longest wait for a started program to show its window.
const WINDOW_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

impl OpenAppTool {
    pub fn with_start_menu(launcher: Arc<dyn Launcher>) -> Self {
        let mut dirs = Vec::new();
        if let Ok(pd) = std::env::var("ProgramData") {
            dirs.push(PathBuf::from(pd).join("Microsoft\\Windows\\Start Menu\\Programs"));
        }
        if let Ok(ad) = std::env::var("APPDATA") {
            dirs.push(PathBuf::from(ad).join("Microsoft\\Windows\\Start Menu\\Programs"));
        }
        Self {
            launcher,
            shortcut_dirs: dirs,
            windows: None,
            own_process: String::new(),
            wait: WINDOW_WAIT,
        }
    }

    pub fn waiting_for_window(mut self, windows: Arc<dyn Windows>, own_process: String) -> Self {
        self.windows = Some(windows);
        self.own_process = own_process;
        self
    }

    /// A window that was not there before the launch, once it shows up.
    async fn new_window(&self, before: &[u64]) -> Option<WindowInfo> {
        let windows = self.windows.as_ref()?;
        let deadline = std::time::Instant::now() + self.wait;
        loop {
            let fresh = windows.list().into_iter().find(|w| {
                !before.contains(&w.id) && w.process != self.own_process && !w.title.is_empty()
            });
            if fresh.is_some() || std::time::Instant::now() >= deadline {
                return fresh;
            }
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
    }

    /// (display name, what to open)
    fn resolve(&self, name: &str) -> Option<(String, String)> {
        let n = name.trim().to_lowercase();
        if let Some((_, target)) = ALIASES.iter().find(|(a, _)| *a == n) {
            return Some((n.clone(), (*target).to_string()));
        }
        // Models often pass the file name: "explorer.exe" means "проводник".
        // Only known programs; an arbitrary .exe name is not launched.
        let exe = if n.ends_with(".exe") {
            n.clone()
        } else {
            format!("{n}.exe")
        };
        if let Some((alias, target)) = ALIASES.iter().find(|(_, t)| *t == exe) {
            return Some(((*alias).to_string(), (*target).to_string()));
        }
        let wanted = SHORTCUT_HINTS
            .iter()
            .find(|(a, _)| *a == n)
            .map(|(_, w)| (*w).to_string())
            .unwrap_or(n);
        let shortcuts = self
            .shortcut_dirs
            .iter()
            .flat_map(|d| shortcuts_in(d, 4))
            .collect::<Vec<_>>();
        let stem = |p: &PathBuf| {
            p.file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        // Exact name first, then the shortest name containing the words.
        if let Some(p) = shortcuts.iter().find(|p| stem(p) == wanted) {
            return Some((stem(p), p.display().to_string()));
        }
        let by_shortcut = shortcuts
            .iter()
            .filter(|p| {
                let s = stem(p);
                !s.contains("uninstall") && !s.contains("удал") && s.contains(&wanted)
            })
            .min_by_key(|p| stem(p).len())
            .map(|p| (stem(p), p.display().to_string()));
        if by_shortcut.is_some() {
            return by_shortcut;
        }
        // Store apps have no shortcut file: the Start menu knows them by AppID.
        self.launcher
            .start_apps()
            .into_iter()
            .map(|(name, id)| (name.to_lowercase(), id))
            .filter(|(name, _)| {
                !name.contains("uninstall") && !name.contains("удал") && name.contains(&wanted)
            })
            .min_by_key(|(name, _)| (name != &wanted, name.len()))
            .map(|(name, id)| (name, format!("shell:AppsFolder\\{id}")))
    }

    /// A window of the program when it is already running and cannot be found
    /// to start: «телеграм» finds a Telegram window.
    fn running(&self, name: &str) -> Option<WindowInfo> {
        let windows = self.windows.as_ref()?;
        let n = name.trim().to_lowercase();
        let hint = SHORTCUT_HINTS
            .iter()
            .find(|(a, _)| *a == n)
            .map(|(_, w)| (*w).to_string())
            .unwrap_or_else(|| n.clone());
        windows
            .list()
            .into_iter()
            .filter(|w| w.process != self.own_process && !w.title.is_empty())
            .map(|w| (score(&w, &hint).max(score(&w, &n)), w))
            .filter(|(s, _)| *s > 0)
            .max_by_key(|(s, _)| *s)
            .map(|(_, w)| w)
    }
}

fn shortcuts_in(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.filter_map(Result::ok) {
        let p = e.path();
        if p.is_dir() && depth > 0 {
            out.extend(shortcuts_in(&p, depth - 1));
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("lnk") || x.eq_ignore_ascii_case("url"))
        {
            out.push(p);
        }
    }
    out
}

#[async_trait]
impl Tool for OpenAppTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "open_app",
            "Запустить программу по названию: «блокнот», «калькулятор», «параметры», «хром», «телеграм» или имя из меню «Пуск».",
            schema(&[("name", "string", "Название программы")], &["name"]),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let name = arg_str(args, "name")?;
        let Some((display, target)) = self.resolve(name) else {
            let w = self
                .running(name)
                .ok_or_else(|| failed(format!("не нашла программу «{name}» в меню «Пуск»")))?;
            return Ok(Assessment::new(
                RiskLevel::Low,
                format!("Переключиться на «{}»", w.title),
                w.process.clone(),
                "программа уже открыта, окно выходит вперёд",
            )
            .with_scope(w.process));
        };
        Ok(Assessment::new(
            RiskLevel::Low,
            format!("Запустить «{display}»"),
            target,
            "запуск программы, её можно закрыть",
        )
        .with_scope(display))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let name = arg_str(&args, "name")?;
        let Some((display, target)) = self.resolve(name) else {
            let w = self
                .running(name)
                .ok_or_else(|| failed(format!("не нашла программу «{name}»")))?;
            if let Some(windows) = &self.windows {
                windows.focus(w.id).map_err(failed)?;
            }
            return Ok(ToolOutput::new(format!(
                "«{name}» уже открыта: переключила на окно «{}» ({}). Дальше ui_read или ui_click с window.",
                w.title, w.process
            )));
        };
        let before: Vec<u64> = self
            .windows
            .as_ref()
            .map(|w| w.list().into_iter().map(|w| w.id).collect())
            .unwrap_or_default();
        self.launcher.open(&target, None).map_err(failed)?;
        if self.windows.is_none() {
            return Ok(ToolOutput::new(format!("Запущено: {display}.")));
        }
        Ok(ToolOutput::new(match self.new_window(&before).await {
            Some(w) => format!(
                "Запущено: {display}. Открылось окно «{}» ({}): дальше ui_read или ui_click с window.",
                w.title, w.process
            ),
            None => format!(
                "Запущено: {display}, но новое окно за {} с не появилось. Возможно, программа уже была открыта: посмотри list_windows.",
                self.wait.as_secs()
            ),
        }))
    }
}

/// How well a window matches a title part or a program name; 0 is no match.
pub(crate) fn score(w: &WindowInfo, query: &str) -> u8 {
    let q = query.trim().to_lowercase();
    let q_exe = if q.ends_with(".exe") {
        q.clone()
    } else {
        format!("{q}.exe")
    };
    let title = w.title.to_lowercase();
    if w.process == q_exe || title == q {
        3
    } else if title.contains(&q) {
        2
    } else if w.process.contains(&q) {
        1
    } else {
        0
    }
}

pub struct ListWindowsTool(pub Arc<dyn Windows>);

#[async_trait]
impl Tool for ListWindowsTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "list_windows",
            "Список открытых окон: заголовок и программа.",
            schema(&[], &[]),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть открытые окна",
            "список окон",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let lines: Vec<String> = self
            .0
            .list()
            .iter()
            .map(|w| format!("{} — {}", w.title, w.process))
            .collect();
        if lines.is_empty() {
            return Ok(ToolOutput::new("Открытых окон нет."));
        }
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

pub struct FocusWindowTool(pub Arc<dyn Windows>);

#[async_trait]
impl Tool for FocusWindowTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "focus_window",
            "Переключиться на окно по части заголовка или имени программы.",
            schema(
                &[("query", "string", "Часть заголовка или программа")],
                &["query"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let q = arg_str(args, "query")?;
        Ok(
            Assessment::new(RiskLevel::Low, "Переключить окно", q, "ничего не меняет")
                .with_scope("*"),
        )
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let q = arg_str(&args, "query")?;
        let best = self
            .0
            .list()
            .into_iter()
            .map(|w| (score(&w, q), w))
            .filter(|(s, _)| *s > 0)
            .max_by_key(|(s, _)| *s)
            .map(|(_, w)| w)
            .ok_or_else(|| failed(format!("нет окна «{q}»")))?;
        self.0.focus(best.id).map_err(failed)?;
        Ok(ToolOutput::new(format!("Переключила на «{}».", best.title)))
    }
}

pub struct CloseAppTool(pub Arc<dyn Windows>);

#[async_trait]
impl Tool for CloseAppTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "close_app",
            "Закрыть окна программы, как кнопкой «закрыть». Программа может спросить о сохранении.",
            schema(
                &[("name", "string", "Программа (notepad) или часть заголовка")],
                &["name"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let name = arg_str(args, "name")?;
        Ok(Assessment::new(
            RiskLevel::Medium,
            format!("Закрыть «{name}»"),
            name,
            "несохранённая работа может пропасть",
        )
        .with_scope(name.trim().to_lowercase()))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let name = arg_str(&args, "name")?;
        let list = self.0.list();
        let best = list.iter().map(|w| score(w, name)).max().unwrap_or(0);
        if best == 0 {
            return Err(failed(format!("нет окна «{name}»")));
        }
        let mut closed = Vec::new();
        for w in list.iter().filter(|w| score(w, name) == best) {
            self.0.close(w.id).map_err(failed)?;
            closed.push(w.title.clone());
        }
        Ok(ToolOutput::new(format!("Закрываю: {}.", closed.join(", "))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{env, run};
    use serde_json::json;

    fn tool_with_shortcuts(
        names: &[&str],
    ) -> (OpenAppTool, tempfile::TempDir, Arc<crate::testing::Fakes>) {
        let (e, f) = env();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Google")).unwrap();
        for n in names {
            std::fs::write(dir.path().join(n), b"").unwrap();
        }
        let t = OpenAppTool {
            launcher: e.launcher,
            shortcut_dirs: vec![dir.path().to_path_buf()],
            windows: None,
            own_process: String::new(),
            wait: std::time::Duration::ZERO,
        };
        (t, dir, f)
    }

    #[tokio::test]
    async fn open_app_names_the_window_that_appeared() {
        let (e, f) = env();
        let t = OpenAppTool {
            launcher: e.launcher.clone(),
            shortcut_dirs: Vec::new(),
            windows: None,
            own_process: "mika.exe".into(),
            wait: std::time::Duration::from_millis(400),
        }
        .waiting_for_window(e.windows.clone(), "mika.exe".into());
        // The fake launcher does not open windows: a window added in the background stands in.
        let fakes = f.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            fakes.windows.lock().push(WindowInfo {
                id: 42,
                title: "Калькулятор".into(),
                process: "calc.exe".into(),
            });
        });
        let out = run(&t, json!({"name": "калькулятор"})).await.unwrap().text;
        assert!(
            out.contains("Открылось окно «Калькулятор» (calc.exe)"),
            "{out}"
        );
        let out = run(&t, json!({"name": "калькулятор"})).await.unwrap().text;
        assert!(out.contains("новое окно за 0 с не появилось"), "{out}");
    }

    #[tokio::test]
    async fn open_app_finds_store_apps_and_running_programs() {
        let (e, f) = env();
        f.start_apps.lock().push((
            "Telegram Desktop".into(),
            "TelegramMessengerLLP.TelegramDesktop_t4vj0pshhgkwm!Telegram".into(),
        ));
        let t = OpenAppTool {
            launcher: e.launcher.clone(),
            shortcut_dirs: Vec::new(),
            windows: Some(e.windows.clone()),
            own_process: "mika.exe".into(),
            wait: std::time::Duration::ZERO,
        };
        run(&t, json!({"name": "телеграм"})).await.unwrap();
        assert_eq!(
            f.opened.lock().last().unwrap().0,
            "shell:AppsFolder\\TelegramMessengerLLP.TelegramDesktop_t4vj0pshhgkwm!Telegram"
        );
        // Not in the Start menu but running: its window comes forward.
        f.start_apps.lock().clear();
        f.windows.lock().push(WindowInfo {
            id: 77,
            title: "Telegram (12)".into(),
            process: "Telegram.exe".into(),
        });
        let out = run(&t, json!({"name": "телеграм"})).await.unwrap().text;
        assert!(out.contains("уже открыта"), "{out}");
        assert_eq!(f.focused.lock().last(), Some(&77));
        assert!(t.assess(&json!({"name": "фотошоп"})).is_err());
    }

    #[tokio::test]
    async fn open_app_resolves_russian_alias() {
        let (t, _d, f) = tool_with_shortcuts(&[]);
        run(&t, json!({"name": "Блокнот"})).await.unwrap();
        assert_eq!(f.opened.lock()[0].0, "notepad.exe");
    }

    #[tokio::test]
    async fn open_app_finds_start_menu_shortcut() {
        let (t, d, f) = tool_with_shortcuts(&[
            "Google\\Google Chrome.lnk",
            "Uninstall Chrome.lnk",
            "Telegram.lnk",
        ]);
        let a = t.assess(&json!({"name": "хром"})).unwrap();
        assert_eq!(a.scope, "google chrome");
        assert_eq!(a.level, RiskLevel::Low);
        run(&t, json!({"name": "телеграм"})).await.unwrap();
        assert_eq!(
            f.opened.lock()[0].0,
            d.path().join("Telegram.lnk").display().to_string()
        );
    }

    #[test]
    fn open_app_accepts_file_names_of_known_programs() {
        let (t, _d, _f) = tool_with_shortcuts(&[]);
        for name in ["explorer.exe", "Explorer.EXE", "calc"] {
            let a = t.assess(&json!({ "name": name })).unwrap();
            assert_eq!(a.level, RiskLevel::Low, "{name}");
        }
        let by_file = t.assess(&json!({"name": "explorer.exe"})).unwrap();
        let by_name = t.assess(&json!({"name": "проводник"})).unwrap();
        assert_eq!(by_file.target, "explorer.exe");
        // Same scope, so one permanent rule covers both spellings.
        assert_eq!(by_file.scope, by_name.scope);
        assert_eq!(by_file.summary, "Запустить «проводник»");
    }

    #[test]
    fn open_app_does_not_run_arbitrary_exe_names() {
        let (t, _d, _f) = tool_with_shortcuts(&[]);
        assert!(t.assess(&json!({"name": "evil.exe"})).is_err());
    }

    #[test]
    fn open_app_unknown_is_error_without_question() {
        let (t, _d, _f) = tool_with_shortcuts(&[]);
        assert!(t.assess(&json!({"name": "несуществующая"})).is_err());
    }

    fn windows() -> Vec<WindowInfo> {
        vec![
            WindowInfo {
                id: 1,
                title: "Заметки.txt — Блокнот".into(),
                process: "notepad.exe".into(),
            },
            WindowInfo {
                id: 2,
                title: "Список.txt — Блокнот".into(),
                process: "notepad.exe".into(),
            },
            WindowInfo {
                id: 3,
                title: "Google Chrome".into(),
                process: "chrome.exe".into(),
            },
        ]
    }

    #[tokio::test]
    async fn close_app_closes_matching_windows_only() {
        let (e, f) = env();
        *f.windows.lock() = windows();
        let t = CloseAppTool(e.windows);
        assert_eq!(
            t.assess(&json!({"name": "notepad"})).unwrap().level,
            RiskLevel::Medium
        );
        run(&t, json!({"name": "notepad"})).await.unwrap();
        assert_eq!(*f.closed.lock(), vec![1, 2]);
    }

    #[tokio::test]
    async fn focus_window_picks_best_title_match() {
        let (e, f) = env();
        *f.windows.lock() = windows();
        run(
            &FocusWindowTool(e.windows.clone()),
            json!({"query": "список"}),
        )
        .await
        .unwrap();
        assert_eq!(*f.focused.lock(), vec![2]);
        assert!(run(&FocusWindowTool(e.windows), json!({"query": "word"}))
            .await
            .is_err());
    }
}
