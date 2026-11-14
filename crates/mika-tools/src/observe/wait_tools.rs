//! Tools for "wait and tell": watch a condition and say when it holds.
//!
//! `wait_for` only reads: whether a path is there, whether a process runs,
//! what a window says. It answers at once; the watcher checks the condition
//! afterwards and Mika tells the user herself. A window is found the way
//! `ui_read` finds it, so the "blind zone" is refused here too.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::policy::paths::{normalize_path, PathAccess};
use mika_core::tools::{
    arg_str, arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput,
};
use mika_core::triggers::{app_key, one_line};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::sensors::Sensors;
use super::service::is_network;
use super::waits::{
    file_name, holds, Condition, Waits, DEFAULT_TIMEOUT_MIN, FULL, MAX_PHRASE, MAX_SAY,
    MAX_TIMEOUT_MIN,
};
use crate::files::{path_arg, path_assessment};
use crate::sys::WindowInfo;
use crate::ui::{label, program, UiEnv};
use crate::util::{arg_u64, failed, schema};

/// What `wait_for` needs from the outside.
#[derive(Clone)]
pub struct WaitEnv {
    pub waits: Waits,
    pub sensors: Arc<dyn Sensors>,
    /// Finds the window the same way `ui_read` does.
    pub ui: UiEnv,
    /// The group "Управление окнами" is on: windows may be read.
    pub windows_on: bool,
    pub user_profile: String,
}

fn bad(message: impl Into<String>) -> ToolError {
    ToolError::BadArguments(message.into())
}

fn now() -> i64 {
    chrono::Local::now().timestamp()
}

/// A process to wait for: by its name or by its pid.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Process {
    Name(String),
    Pid(u32),
}

/// What the arguments ask to wait for, before anything is looked at.
#[derive(Debug, Clone, PartialEq)]
enum Ask {
    Path { path: PathBuf, settles: bool },
    Process(Process),
    Window { phrase: String, present: bool },
}

fn timeout_min(args: &Value) -> Result<u64, ToolError> {
    let out_of_range = || bad("timeout_min должно быть от 1 до 480");
    match args.get("timeout_min") {
        None | Some(Value::Null) => Ok(DEFAULT_TIMEOUT_MIN),
        Some(_) => match arg_u64(args, "timeout_min") {
            Some(n) if (1..=MAX_TIMEOUT_MIN).contains(&n) => Ok(n),
            _ => Err(out_of_range()),
        },
    }
}

/// The words the model asks to add to the message: one line, cut to the
/// length that is kept, exactly as they will be said.
fn say_arg(args: &Value) -> String {
    one_line(arg_str_opt(args, "say").unwrap_or(""))
        .chars()
        .take(MAX_SAY)
        .collect()
}

/// True for the folder of all user profiles and for the profile of another
/// user of this computer. The shared `Public` profile and the user's own one
/// are not foreign.
fn foreign_profile(path: &str, user_profile: &str) -> bool {
    let path = normalize_path(path).to_lowercase();
    let own = normalize_path(user_profile).to_lowercase();
    if !own.is_empty() && (path == own || path.starts_with(&format!("{own}\\"))) {
        return false;
    }
    // The drive, then the folder of profiles, then a profile.
    let mut parts = path.split('\\');
    let drive = parts.next().unwrap_or("");
    if drive.len() != 2 || !drive.ends_with(':') || parts.next() != Some("users") {
        return false;
    }
    parts.next() != Some("public")
}

fn process_arg(args: &Value) -> Result<Process, ToolError> {
    let need = || bad("нужно имя процесса, например cargo.exe, или pid");
    let raw = match args.get("process") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.trim().to_string(),
        _ => return Err(need()),
    };
    if !raw.is_empty() && raw.chars().all(|c| c.is_ascii_digit()) {
        return raw.parse().map(Process::Pid).map_err(|_| need());
    }
    app_key(&raw).map(Process::Name).ok_or_else(need)
}

impl WaitEnv {
    /// The window to watch, found the way `ui_read` finds it: a window of
    /// the "blind zone" is refused there. It must also be one the watcher
    /// finds again. A dialog or a window without a title can be in front
    /// now, but it is not in the list of windows, and its watch would end at
    /// once as "closed".
    fn window(&self, args: &Value) -> Result<WindowInfo, ToolError> {
        let w = self.ui.target(args)?;
        if !self.ui.windows.list().iter().any(|x| x.id == w.id) {
            return Err(failed(
                "За этим окном следить не получится: это диалог или окно без заголовка. Назови в window главное окно программы.",
            ));
        }
        Ok(w)
    }

    fn ask(&self, args: &Value) -> Result<Ask, ToolError> {
        let what = arg_str(args, "what")?.trim();
        match what {
            "file_appears" | "file_ready" => {
                let path = path_arg(args, "path", &self.user_profile)?;
                if path.to_string_lossy().contains(['*', '?']) {
                    return Err(bad("нужен один путь без масок"));
                }
                Ok(Ask::Path {
                    path,
                    settles: what == "file_ready",
                })
            }
            "process_exits" => Ok(Ask::Process(process_arg(args)?)),
            "window_text" | "window_text_gone" => {
                if !self.windows_on {
                    return Err(failed(
                        "Чтение окон выключено: включи группу «Управление окнами» на странице «Инструменты».",
                    ));
                }
                let phrase = arg_str_opt(args, "text").unwrap_or("").trim();
                if phrase.is_empty() || phrase.chars().count() > MAX_PHRASE {
                    return Err(bad(
                        "нужна фраза text, которую ждём в окне, до 200 символов",
                    ));
                }
                Ok(Ask::Window {
                    phrase: phrase.to_string(),
                    present: what == "window_text",
                })
            }
            _ => Err(bad(
                "what: file_appears, file_ready, process_exits, window_text или window_text_gone",
            )),
        }
    }
}

pub struct WaitForTool(pub WaitEnv);

impl WaitForTool {
    /// The question about the condition itself, without the words to add.
    fn assess_condition(&self, args: &Value) -> Result<Assessment, ToolError> {
        let ask = self.0.ask(args)?;
        timeout_min(args)?;
        // No question is asked for a watch that cannot start.
        if self.0.waits.is_full() {
            return Err(failed(FULL));
        }
        match ask {
            Ask::Path { path, settles } => {
                let name = file_name(&path);
                let summary = if settles {
                    format!("Сказать, когда «{name}» перестанет расти")
                } else {
                    format!("Сказать, когда появится «{name}»")
                };
                let shown = path.display().to_string();
                let scope = format!("file:{}", shown.to_lowercase());
                let a = path_assessment(
                    &path,
                    &self.0.user_profile,
                    PathAccess::Read,
                    summary,
                    RiskLevel::Low,
                )
                .with_scope(scope);
                // A watch looks again and again, for hours. At another
                // computer or in another person's folder that is more than
                // one read: it is asked about as a medium risk.
                Ok(if is_network(&path) {
                    a.at_least(
                        RiskLevel::Medium,
                        "сетевая папка: пока идёт слежение, компьютер раз в 30 секунд обращается к этому серверу под твоей учётной записью",
                    )
                } else if foreign_profile(&shown, &self.0.user_profile) {
                    a.at_least(
                        RiskLevel::Medium,
                        "папка другого пользователя этого компьютера",
                    )
                } else {
                    a
                })
            }
            Ask::Process(process) => {
                let (shown, scope) = match &process {
                    Process::Name(name) => (name.clone(), format!("process:{name}")),
                    Process::Pid(pid) => (format!("pid {pid}"), format!("process:{pid}")),
                };
                Ok(Assessment::new(
                    RiskLevel::Low,
                    format!("Сказать, когда завершится процесс {shown}"),
                    format!("процесс {shown}"),
                    "только смотрит, работает ли процесс в твоём сеансе",
                )
                .with_scope(scope))
            }
            Ask::Window { phrase, present } => {
                let w = self.0.window(args)?;
                let summary = if present {
                    format!("Сказать, когда в окне появится «{phrase}»")
                } else {
                    format!("Сказать, когда из окна пропадёт «{phrase}»")
                };
                Ok(Assessment::new(
                    RiskLevel::Low,
                    summary,
                    label(&w),
                    "окно перечитывается каждые 10 секунд, пока идёт слежение; его текст никуда не уходит и не сохраняется",
                )
                .with_scope(program(&w)))
            }
        }
    }
}

#[async_trait]
impl Tool for WaitForTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "wait_for",
            "Подождать и сказать: следить за условием и сообщить пользователю, когда оно выполнится. what: file_appears — появится файл или папка path; file_ready — файл или папка path перестанет расти (скачивание закончилось); process_exits — завершится процесс process (имя вроде cargo.exe или pid: сборка закончилась); window_text — в окне window появится фраза text; window_text_gone — фраза text пропадёт из окна window. Инструмент отвечает сразу, а о событии пользователю скажет Мика сама, позже. timeout_min — сколько ждать, по умолчанию 60, не больше 480. say — что сказать, когда случится.",
            schema(
                &[
                    (
                        "what",
                        "string",
                        "one of: file_appears, file_ready, process_exits, window_text, window_text_gone",
                    ),
                    ("path", "string", "Для file_*: файл или папка"),
                    (
                        "process",
                        "string",
                        "Для process_exits: имя процесса (cargo.exe) или pid",
                    ),
                    (
                        "window",
                        "string",
                        "Для window_*: часть заголовка окна или имя программы; без него — окно, в котором работает пользователь",
                    ),
                    ("text", "string", "Для window_*: фраза, которую ждём"),
                    (
                        "timeout_min",
                        "integer",
                        "Сколько минут ждать, по умолчанию 60, не больше 480",
                    ),
                    (
                        "say",
                        "string",
                        "Что сказать пользователю, когда случится",
                    ),
                ],
                &["what"],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let mut a = self.assess_condition(args)?;
        // Her words are shown before they can be said: they reach the chat,
        // the voice, a Windows notification and Telegram.
        let say = say_arg(args);
        if !say.is_empty() {
            a.target = format!("{}. К сообщению добавлю: «{say}»", a.target);
        }
        Ok(a)
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let ask = self.0.ask(&args)?;
        let minutes = timeout_min(&args)?;
        let say = say_arg(&args);
        let condition = match ask {
            Ask::Path {
                path,
                settles: false,
            } => {
                let probe = path.clone();
                let there = tokio::task::spawn_blocking(move || probe.exists())
                    .await
                    .map_err(|e| failed(e.to_string()))?;
                if there {
                    return Ok(ToolOutput::new(format!(
                        "«{}» уже есть: {}. Следить не стала.",
                        file_name(&path),
                        path.display()
                    )));
                }
                Condition::PathAppears { path }
            }
            Ask::Path {
                path,
                settles: true,
            } => Condition::PathSettles { path },
            Ask::Process(process) => {
                let sensors = self.0.sensors.clone();
                let procs = tokio::task::spawn_blocking(move || sensors.processes())
                    .await
                    .map_err(|e| failed(e.to_string()))?
                    .map_err(failed)?;
                match process {
                    Process::Name(name) => {
                        if !procs.iter().any(|p| p.name == name) {
                            return Err(failed(format!(
                                "процесса {name} в твоём сеансе сейчас нет"
                            )));
                        }
                        Condition::ProcessExits { name, pid: None }
                    }
                    Process::Pid(pid) => {
                        let p = procs.iter().find(|p| p.pid == pid).ok_or_else(|| {
                            failed(format!("процесса с pid {pid} в твоём сеансе нет"))
                        })?;
                        Condition::ProcessExits {
                            name: p.name.clone(),
                            pid: Some((pid, p.started)),
                        }
                    }
                }
            }
            Ask::Window { phrase, present } => {
                // Found again: the window may have changed since the question.
                let w = self.0.window(&args)?;
                let sensors = self.0.sensors.clone();
                let id = w.id;
                let text = tokio::task::spawn_blocking(move || sensors.window_text(id))
                    .await
                    .map_err(|e| failed(e.to_string()))?
                    .map_err(|e| failed(format!("окно не читается: {e}")))?;
                let there = holds(&text, &phrase);
                let process = program(&w);
                if present && there {
                    return Ok(ToolOutput::new(format!(
                        "Фраза «{phrase}» уже есть в окне программы {process}. Следить не стала."
                    )));
                }
                if !present && !there {
                    return Err(failed(format!(
                        "фразы «{phrase}» в окне сейчас нет, ждать её пропажи нельзя"
                    )));
                }
                Condition::WindowText {
                    window: w.id,
                    process,
                    phrase,
                    present,
                }
            }
        };
        let words = condition.words();
        let view = self
            .0
            .waits
            .add(condition, &say, minutes, now())
            .map_err(failed)?;
        Ok(ToolOutput::new(format!(
            "Слежу (№{}): {words}. Скажу, когда это случится, или через {minutes} мин, если не случится. Слежение живёт, пока я запущена. Ответь пользователю коротко и не вызывай wait_for повторно.",
            view.id
        )))
    }
}

pub struct WaitListTool(pub Waits);

#[async_trait]
impl Tool for WaitListTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "wait_list",
            "Показать, за чем Мика сейчас следит по просьбе «скажи, когда…».",
            json!({ "type": "object", "properties": {} }),
        )
    }

    fn assess(&self, _args: &Value) -> Result<Assessment, ToolError> {
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть, за чем слежу",
            "слежения",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        _args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let list = self.0.described();
        if list.is_empty() {
            return Ok(ToolOutput::new("Ни за чем не слежу."));
        }
        let at = now();
        let lines: Vec<String> = list
            .iter()
            .map(|(view, words)| {
                let left = (view.until - at).max(0);
                format!("№{}: {words}, ещё {} мин", view.id, (left + 59) / 60)
            })
            .collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

pub struct WaitStopTool(pub Waits);

#[async_trait]
impl Tool for WaitStopTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "wait_stop",
            "Перестать следить: за одним условием по его номеру из wait_list или, без номера, за всеми.",
            schema(
                &[(
                    "id",
                    "integer",
                    "Номер слежения из wait_list; без него — перестать следить за всем",
                )],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let (summary, target) = match arg_u64(args, "id") {
            Some(id) => (
                format!("Перестать следить (№{id})"),
                format!("слежение №{id}"),
            ),
            None => (
                "Перестать следить за всем".to_string(),
                "все слежения".to_string(),
            ),
        };
        Ok(Assessment::new(
            RiskLevel::Low,
            summary,
            target,
            "слежение можно начать снова",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        match arg_u64(&args, "id") {
            Some(id) => {
                if self.0.stop(id.min(u64::from(u32::MAX)) as u32) {
                    Ok(ToolOutput::new(format!("Больше не слежу (№{id}).")))
                } else {
                    Err(failed(format!("слежения №{id} нет")))
                }
            }
            None => {
                self.0.stop_all();
                Ok(ToolOutput::new("Больше ни за чем не слежу."))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::{Proc, WaitView, MAX_SAY, MAX_WAITS};
    use crate::sys::WindowInfo;
    use crate::testing::{self, run, Fakes};
    use mika_core::policy::blind::BLIND_REFUSAL;

    const PROFILE: &str = "C:\\Users\\me";

    fn tool_with(windows_on: bool) -> (WaitForTool, Arc<Fakes>) {
        let (env, f) = testing::env();
        let ui = UiEnv {
            windows: env.windows.clone(),
            ui: env.ui.clone(),
            input: env.input.clone(),
            bus: env.shared.bus.clone(),
            own_process: "mika.exe".into(),
            seen: Default::default(),
            clipboard: None,
            sent: Default::default(),
            clips: Default::default(),
        };
        let tool = WaitForTool(WaitEnv {
            waits: env.shared.waits.clone(),
            sensors: env.sensors.clone(),
            ui,
            windows_on,
            user_profile: PROFILE.into(),
        });
        (tool, f)
    }

    fn tool() -> (WaitForTool, Arc<Fakes>) {
        tool_with(true)
    }

    fn window(id: u64, title: &str, process: &str) -> WindowInfo {
        WindowInfo {
            id,
            title: title.into(),
            process: process.into(),
        }
    }

    fn proc(pid: u32, name: &str, started: u64) -> Proc {
        Proc {
            pid,
            name: name.into(),
            started,
        }
    }

    /// The only watch, without its times.
    fn only(tool: &WaitForTool) -> (u32, &'static str, String) {
        let list = tool.0.waits.list();
        assert_eq!(list.len(), 1, "{list:?}");
        (list[0].id, list[0].kind, list[0].what.clone())
    }

    #[tokio::test]
    async fn waiting_for_a_download_to_finish() {
        let (tool, f) = tool();
        let args = json!({"what": "file_ready", "path": "Downloads\\ubuntu.iso"});
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert_eq!(a.summary, "Сказать, когда «ubuntu.iso» перестанет расти");
        assert_eq!(a.target, "C:\\Users\\me\\Downloads\\ubuntu.iso");
        assert_eq!(a.scope, "file:c:\\users\\me\\downloads\\ubuntu.iso");
        let before = now();
        let out = run(&tool, args).await.unwrap();
        assert_eq!(
            out.text,
            "Слежу (№1): «ubuntu.iso» перестанет расти. Скажу, когда это случится, или через 60 мин, если не случится. Слежение живёт, пока я запущена. Ответь пользователю коротко и не вызывай wait_for повторно."
        );
        assert!(out.data.is_none());
        let view: WaitView = tool.0.waits.list().remove(0);
        assert_eq!(
            (view.id, view.kind, view.what.as_str()),
            (1, "file_ready", "ubuntu.iso")
        );
        assert!(view.since >= before && view.until == view.since + 3600);
        // A file is looked at by the watcher, not through the sensors.
        assert!(f.sensor_calls.lock().is_empty());
    }

    #[tokio::test]
    async fn waiting_for_a_file_that_is_already_there_starts_nothing() {
        let (tool, _) = tool();
        let d = tempfile::tempdir().unwrap();
        let there = d.path().join("report.pdf");
        std::fs::write(&there, b"x").unwrap();
        let out = run(
            &tool,
            json!({"what": "file_appears", "path": there.to_string_lossy()}),
        )
        .await
        .unwrap();
        assert!(
            out.text.starts_with("«report.pdf» уже есть: "),
            "{}",
            out.text
        );
        assert!(out.text.ends_with("Следить не стала."));
        assert!(tool.0.waits.is_empty());

        let missing = d.path().join("later.pdf");
        let args =
            json!({"what": "file_appears", "path": missing.to_string_lossy(), "timeout_min": 5});
        assert_eq!(
            tool.assess(&args).unwrap().summary,
            "Сказать, когда появится «later.pdf»"
        );
        let out = run(&tool, args).await.unwrap();
        assert!(out.text.starts_with(
            "Слежу (№1): появится «later.pdf». Скажу, когда это случится, или через 5 мин"
        ));
        assert_eq!(only(&tool), (1, "file_appears", "later.pdf".to_string()));
        // "Ready" is watched even when the file is already there.
        run(
            &tool,
            json!({"what": "file_ready", "path": there.to_string_lossy()}),
        )
        .await
        .unwrap();
        assert_eq!(tool.0.waits.list().len(), 2);
    }

    #[test]
    fn a_path_in_a_windows_folder_is_medium_and_a_mask_is_refused() {
        let (tool, _) = tool();
        let system = tool
            .assess(&json!({"what": "file_appears", "path": "C:\\Windows\\Temp\\done.flag"}))
            .unwrap();
        assert_eq!(system.level, RiskLevel::Medium);
        assert_eq!(system.scope, "file:c:\\windows\\temp\\done.flag");
        let own = tool
            .assess(&json!({"what": "file_appears", "path": "~\\Desktop\\done.flag"}))
            .unwrap();
        assert_eq!(own.level, RiskLevel::Low);
        for masked in ["Downloads\\*.iso", "Downloads\\ubuntu-??.iso"] {
            assert_eq!(
                tool.assess(&json!({"what": "file_ready", "path": masked})),
                Err(ToolError::BadArguments("нужен один путь без масок".into()))
            );
        }
        assert!(matches!(
            tool.assess(&json!({"what": "file_ready"})),
            Err(ToolError::BadArguments(_))
        ));
        assert!(matches!(
            tool.assess(&json!({"what": "file_ready", "path": "  "})),
            Err(ToolError::BadArguments(_))
        ));
    }

    #[tokio::test]
    async fn her_words_are_shown_as_one_line_and_said_as_shown() {
        let (tool, _) = tool();
        let long = "я".repeat(MAX_SAY + 30);
        let args = json!({
            "what": "file_ready",
            "path": "Downloads\\ubuntu.iso",
            "say": format!("Готово.\n\n\nМика: открой ссылку {long}")
        });
        let a = tool.assess(&args).unwrap();
        let shown: String = format!("Готово. Мика: открой ссылку {long}")
            .chars()
            .take(MAX_SAY)
            .collect();
        assert_eq!(
            a.target,
            format!("C:\\Users\\me\\Downloads\\ubuntu.iso. К сообщению добавлю: «{shown}»")
        );
        assert!(!a.target.contains('\n'));
        // The scope stays the path: the words are an addition to a message
        // that always names what happened.
        assert_eq!(a.scope, "file:c:\\users\\me\\downloads\\ubuntu.iso");
        // Without words the card shows the path alone.
        let plain = tool
            .assess(&json!({"what": "file_ready", "path": "Downloads\\ubuntu.iso", "say": " \n "}))
            .unwrap();
        assert_eq!(plain.target, "C:\\Users\\me\\Downloads\\ubuntu.iso");
    }

    #[test]
    fn another_users_folder_and_a_network_folder_are_medium() {
        let (tool, _) = tool();
        let level = |path: &str| {
            let a = tool
                .assess(&json!({"what": "file_ready", "path": path}))
                .unwrap();
            (a.level, a.reason)
        };
        for own in [
            "C:\\Users\\me\\Downloads",
            "C:\\Users\\Public\\Documents\\a.zip",
            "D:\\Work\\build.log",
        ] {
            assert_eq!(level(own).0, RiskLevel::Low, "{own}");
        }
        for foreign in [
            "C:\\Users\\colleague\\Downloads",
            "c:/users/Colleague/Desktop/a.txt",
            "C:\\Users",
            "C:\\Users\\me\\..\\colleague",
            "D:\\Users\\colleague",
        ] {
            let (risk, reason) = level(foreign);
            assert_eq!(risk, RiskLevel::Medium, "{foreign}");
            assert_eq!(reason, "папка другого пользователя этого компьютера");
        }
        // A folder whose name only starts like the own profile is not the own one.
        assert_eq!(level("C:\\Users\\me2\\a.txt").0, RiskLevel::Medium);
        let (risk, reason) = level("\\\\nas\\share\\x.iso");
        assert_eq!(risk, RiskLevel::Medium);
        assert!(
            reason.starts_with("сетевая папка") && reason.contains("под твоей учётной записью"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn a_dialog_in_front_is_not_watched_as_a_window() {
        let (tool, f) = tool();
        // The main window is listed; its progress dialog is in front but is
        // not in the list of windows.
        *f.windows.lock() = vec![window(7, "Установка Blender", "setup.exe")];
        *f.foreground.lock() = Some(window(8, "Копирование", "setup.exe"));
        f.window_texts.lock().insert(8, "Подождите".into());
        let args = json!({"what": "window_text", "text": "Готово"});
        let refused = ToolError::Failed(
            "За этим окном следить не получится: это диалог или окно без заголовка. Назови в window главное окно программы.".into(),
        );
        assert_eq!(tool.assess(&args), Err(refused.clone()));
        assert_eq!(run(&tool, args).await, Err(refused));
        assert!(tool.0.waits.is_empty());
        assert!(f.sensor_calls.lock().is_empty());
        // Named, the main window is watched.
        f.window_texts.lock().insert(7, "Подождите".into());
        run(
            &tool,
            json!({"what": "window_text", "window": "Установка", "text": "Готово"}),
        )
        .await
        .unwrap();
        assert_eq!(only(&tool).1, "window_text");
    }

    #[tokio::test]
    async fn waiting_for_a_process_by_name() {
        let (tool, f) = tool();
        *f.procs.lock() = vec![proc(10, "cargo.exe", 100), proc(11, "rustc.exe", 101)];
        let args =
            json!({"what": "process_exits", "process": "Cargo", "say": "Сборка закончилась"});
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert_eq!(a.summary, "Сказать, когда завершится процесс cargo.exe");
        // Her words are on the card before they can be said.
        assert_eq!(
            a.target,
            "процесс cargo.exe. К сообщению добавлю: «Сборка закончилась»"
        );
        assert_eq!(a.scope, "process:cargo.exe");
        let out = run(&tool, args).await.unwrap();
        assert!(out
            .text
            .starts_with("Слежу (№1): завершится процесс cargo.exe. Скажу"));
        assert_eq!(only(&tool), (1, "process_exits", "cargo.exe".to_string()));
        assert_eq!(
            tool.0.waits.conditions(),
            vec![Condition::ProcessExits {
                name: "cargo.exe".into(),
                pid: None
            }]
        );
        // Her own words are said when it ends, after what happened.
        let world = super::super::waits::World {
            procs: Some(vec![]),
            ..Default::default()
        };
        assert_eq!(
            tool.0.waits.check_all(&world, now()),
            vec![(
                1,
                "Процесс cargo.exe завершился. Сборка закончилась".to_string()
            )]
        );

        assert_eq!(
            run(
                &tool,
                json!({"what": "process_exits", "process": "notepad"})
            )
            .await,
            Err(ToolError::Failed(
                "процесса notepad.exe в твоём сеансе сейчас нет".into()
            ))
        );
        for bad in [json!(""), json!("C:\\tools\\cargo.exe"), json!(null)] {
            assert_eq!(
                tool.assess(&json!({"what": "process_exits", "process": bad})),
                Err(ToolError::BadArguments(
                    "нужно имя процесса, например cargo.exe, или pid".into()
                ))
            );
        }
        *f.sensor_error.lock() = Some("processes");
        assert_eq!(
            run(&tool, json!({"what": "process_exits", "process": "cargo"})).await,
            Err(ToolError::Failed("датчик не ответил".into()))
        );
    }

    #[tokio::test]
    async fn waiting_for_a_process_by_pid_keeps_its_start_time() {
        let (tool, f) = tool();
        *f.procs.lock() = vec![proc(4242, "cargo.exe", 100)];
        for pid in [json!(4242), json!("4242")] {
            let args = json!({"what": "process_exits", "process": pid});
            let a = tool.assess(&args).unwrap();
            assert_eq!(a.summary, "Сказать, когда завершится процесс pid 4242");
            assert_eq!(a.scope, "process:4242");
            let out = run(&tool, args).await.unwrap();
            assert!(out
                .text
                .starts_with("Слежу (№1): завершится процесс cargo.exe (pid 4242). Скажу"));
        }
        assert_eq!(
            tool.0.waits.conditions(),
            vec![Condition::ProcessExits {
                name: "cargo.exe".into(),
                pid: Some((4242, 100))
            }]
        );
        // The pid now belongs to a process started later: the watched one is gone.
        let world = super::super::waits::World {
            procs: Some(vec![proc(4242, "chrome.exe", 500)]),
            ..Default::default()
        };
        assert_eq!(
            tool.0.waits.check_all(&world, now()),
            vec![(1, "Процесс cargo.exe завершился.".to_string())]
        );
        assert_eq!(
            run(&tool, json!({"what": "process_exits", "process": 7})).await,
            Err(ToolError::Failed(
                "процесса с pid 7 в твоём сеансе нет".into()
            ))
        );
    }

    #[tokio::test]
    async fn waiting_for_a_phrase_in_a_window() {
        let (tool, f) = tool();
        *f.windows.lock() = vec![
            window(1, "Мика", "mika.exe"),
            window(7, "Установка Blender", "setup.exe"),
        ];
        f.window_texts
            .lock()
            .insert(7, "Копирование файлов\nПодождите".into());
        let args = json!({"what": "window_text", "window": "Установка", "text": " Готово "});
        let a = tool.assess(&args).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert_eq!(a.summary, "Сказать, когда в окне появится «Готово»");
        assert_eq!(a.target, "«Установка Blender» (setup.exe)");
        assert_eq!(a.scope, "setup.exe");
        assert!(a.reason.contains("каждые 10 секунд") && a.reason.contains("никуда не уходит"));
        let out = run(&tool, args).await.unwrap();
        assert!(out
            .text
            .starts_with("Слежу (№1): в окне программы setup.exe появится «Готово». Скажу"));
        // The watch keeps the program, never the title.
        assert_eq!(
            only(&tool),
            (1, "window_text", "setup.exe: Готово".to_string())
        );
        assert!(!out.text.contains("Blender"));

        let gone = json!({"what": "window_text_gone", "window": "Установка", "text": "подождите"});
        assert_eq!(
            tool.assess(&gone).unwrap().summary,
            "Сказать, когда из окна пропадёт «подождите»"
        );
        let out = run(&tool, gone).await.unwrap();
        assert!(out
            .text
            .starts_with("Слежу (№2): из окна программы setup.exe пропадёт «подождите». Скажу"));
        assert_eq!(tool.0.waits.list()[1].kind, "window_text_gone");
    }

    #[tokio::test]
    async fn a_phrase_already_there_or_already_gone_starts_no_watch() {
        let (tool, f) = tool();
        *f.windows.lock() = vec![window(7, "Установка Blender", "setup.exe")];
        *f.foreground.lock() = Some(window(7, "Установка Blender", "setup.exe"));
        f.window_texts.lock().insert(7, "Мастер\nГотово".into());
        // Without `window`: the window the user works in.
        let out = run(&tool, json!({"what": "window_text", "text": "готово"}))
            .await
            .unwrap();
        assert_eq!(
            out.text,
            "Фраза «готово» уже есть в окне программы setup.exe. Следить не стала."
        );
        assert_eq!(
            run(
                &tool,
                json!({"what": "window_text_gone", "text": "Подождите"})
            )
            .await,
            Err(ToolError::Failed(
                "фразы «Подождите» в окне сейчас нет, ждать её пропажи нельзя".into()
            ))
        );
        assert!(tool.0.waits.is_empty());
        // A window that cannot be read.
        f.window_texts.lock().clear();
        assert_eq!(
            run(&tool, json!({"what": "window_text", "text": "Готово"})).await,
            Err(ToolError::Failed(
                "окно не читается: окно не читается".into()
            ))
        );
        for bad in [json!(""), json!("  "), json!("я".repeat(MAX_PHRASE + 1))] {
            assert!(matches!(
                tool.assess(&json!({"what": "window_text", "text": bad})),
                Err(ToolError::BadArguments(_))
            ));
        }
    }

    #[tokio::test]
    async fn a_blind_window_is_refused_and_never_read() {
        let (tool, f) = tool();
        *f.windows.lock() = vec![window(9, "Вход в банк — Chrome", "chrome.exe")];
        f.window_texts.lock().insert(9, "Пароль".into());
        let args = json!({"what": "window_text", "window": "chrome", "text": "Готово"});
        assert_eq!(
            tool.assess(&args),
            Err(ToolError::Failed(BLIND_REFUSAL.into()))
        );
        // Even past the question (a title that changed after it): `run` looks again.
        assert_eq!(
            tool.run(args, Approval::for_tests(), CancellationToken::new())
                .await,
            Err(ToolError::Failed(BLIND_REFUSAL.into()))
        );
        assert!(f.sensor_calls.lock().is_empty());
        assert!(tool.0.waits.is_empty());
    }

    #[test]
    fn window_watches_need_the_window_group() {
        let (tool, f) = tool_with(false);
        *f.windows.lock() = vec![window(7, "Установка Blender", "setup.exe")];
        assert_eq!(
            tool.assess(&json!({"what": "window_text", "text": "Готово"})),
            Err(ToolError::Failed(
                "Чтение окон выключено: включи группу «Управление окнами» на странице «Инструменты».".into()
            ))
        );
        // The other kinds do not read windows.
        assert!(tool
            .assess(&json!({"what": "process_exits", "process": "cargo"}))
            .is_ok());
    }

    #[tokio::test]
    async fn the_term_has_bounds_and_the_sixth_watch_gets_no_question() {
        let (tool, _) = tool();
        let with =
            |minutes: Value| json!({"what": "file_ready", "path": "a.zip", "timeout_min": minutes});
        for bad in [json!(0), json!(481), json!(-5), json!("скоро")] {
            assert_eq!(
                tool.assess(&with(bad)),
                Err(ToolError::BadArguments(
                    "timeout_min должно быть от 1 до 480".into()
                ))
            );
        }
        let out = run(&tool, with(json!(480))).await.unwrap();
        assert!(out.text.contains("или через 480 мин"), "{}", out.text);
        let view = tool.0.waits.list().remove(0);
        assert_eq!(view.until - view.since, 480 * 60);
        assert!(tool.assess(&with(json!(1))).is_ok());
        assert!(tool.assess(&with(json!(null))).is_ok());

        for i in 1..MAX_WAITS {
            run(
                &tool,
                json!({"what": "file_ready", "path": format!("file{i}.zip")}),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            tool.assess(&json!({"what": "file_ready", "path": "sixth.zip"})),
            Err(ToolError::Failed(FULL.into()))
        );
        assert_eq!(tool.0.waits.list().len(), MAX_WAITS);
        assert!(matches!(
            tool.assess(&json!({"what": "download"})),
            Err(ToolError::BadArguments(_))
        ));
        assert!(matches!(
            tool.assess(&json!({})),
            Err(ToolError::BadArguments(_))
        ));
    }

    #[tokio::test]
    async fn listing_and_stopping_watches() {
        let (tool, f) = tool();
        let list = WaitListTool(tool.0.waits.clone());
        let stop = WaitStopTool(tool.0.waits.clone());
        assert_eq!(
            run(&list, json!({})).await.unwrap().text,
            "Ни за чем не слежу."
        );
        *f.procs.lock() = vec![proc(10, "cargo.exe", 100)];
        run(
            &tool,
            json!({"what": "file_ready", "path": "a.zip", "timeout_min": 30}),
        )
        .await
        .unwrap();
        run(
            &tool,
            json!({"what": "process_exits", "process": "cargo.exe"}),
        )
        .await
        .unwrap();
        assert_eq!(
            run(&list, json!({})).await.unwrap().text,
            "№1: «a.zip» перестанет расти, ещё 30 мин\n№2: завершится процесс cargo.exe, ещё 60 мин"
        );

        let a = stop.assess(&json!({"id": 1})).unwrap();
        assert_eq!(a.level, RiskLevel::Low);
        assert_eq!(a.summary, "Перестать следить (№1)");
        assert_eq!(
            run(&stop, json!({"id": 1})).await.unwrap().text,
            "Больше не слежу (№1)."
        );
        assert_eq!(
            run(&stop, json!({"id": 1})).await,
            Err(ToolError::Failed("слежения №1 нет".into()))
        );
        assert_eq!(
            stop.assess(&json!({})).unwrap().summary,
            "Перестать следить за всем"
        );
        assert_eq!(
            run(&stop, json!({})).await.unwrap().text,
            "Больше ни за чем не слежу."
        );
        assert!(tool.0.waits.is_empty());
    }
}
