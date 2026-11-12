//! Windows tools. Each one goes through the risk policy via `mika_core::tools::Tool`.

pub mod apps;
pub mod archive;
pub mod batch;
pub mod browser;
pub mod commands;
pub mod devices;
pub mod files;
pub mod image;
pub mod info;
pub mod media;
pub mod memory;
pub mod notes;
pub mod rdp;
pub mod reminders;
pub mod screen;
pub mod shell;
pub mod sys;
pub mod system;
pub mod todos;
pub mod ui;
pub mod web;
pub mod win;

mod util;

#[cfg(test)]
mod testing;

use std::sync::Arc;

use mika_core::commands::CommandStore;
use mika_core::schedule::ReminderStore;
use mika_core::task::{TaskPlanTool, TaskStepTool, TASK_RULES};
use mika_core::tools::{TimeTool, ToolRegistry};

pub use sys::{Http, Shared, ToolEnv};

/// Tool groups the user can switch off on the "Tools" page. A switched-off group
/// is not sent to the model at all, which also keeps requests to small models short.
pub const GROUPS: &[&str] = &[
    "system", "apps", "files", "shell", "web", "browser", "notes", "daily", "memory", "media",
    "windows", "screen", "images", "commands", "rdp",
];

/// Executable name of this program, lower case: `mika.exe`.
pub fn own_process() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

/// Which tool answers which kind of request. Small models skip tools without it:
/// they say "remembered" and call nothing.
const HINTS: &[(&str, &str)] = &[
    (
        "memory_save",
        "просят запомнить что-то о себе, или пользователь сам сообщил важное о себе",
    ),
    ("memory_search", "спрашивают, что ты помнишь о пользователе"),
    ("history_search", "спрашивают, о чём говорили раньше"),
    (
        "reminder_set",
        "напомнить, поставить таймер или будильник, делать что-то по расписанию",
    ),
    ("reminder_list", "какие есть напоминания и таймеры"),
    ("todo_add", "добавить дело в список дел или на дату"),
    ("todo_list", "показать дела, планы на день"),
    ("note_create", "записать заметку"),
    ("get_rates", "курс валюты"),
    ("get_news", "новости"),
    ("get_weather", "погода"),
    ("web_search", "найти свежие сведения в интернете"),
    ("get_time", "который час, какое число, какой день недели"),
    ("volume", "громкость, звук"),
    ("media_control", "пауза, следующий или предыдущий трек"),
    ("now_playing", "что сейчас играет"),
    ("open_app", "открыть программу"),
    ("open_url", "открыть сайт в браузере"),
    ("find_files", "найти файл"),
    ("run_shell", "выполнить команду PowerShell или cmd"),
    ("system_info", "нагрузка, память, диски"),
    (
        "ui_read",
        "что в окне программы; первый шаг перед нажатием или вводом в окне",
    ),
    ("ui_click", "нажать кнопку или пункт в окне программы"),
    ("ui_type", "напечатать текст в окне программы"),
    ("ui_keys", "нажать клавиши в окне программы"),
    ("screen_look", "что на экране, описать картинку на экране"),
    ("generate_image", "нарисовать картинку"),
    ("command_save", "сохранить свою команду из нескольких шагов"),
    (
        "browser_open",
        "найти, прочитать или сделать что-то на сайте самой (нажать, заполнить); open_url только показывает сайт пользователю",
    ),
    ("browser_read", "что сейчас на странице в браузере Мики"),
    ("list_folder", "что лежит в папке, размеры и даты файлов"),
    (
        "file_batch",
        "разобрать, переложить, переименовать или скопировать много файлов сразу",
    ),
    ("task_plan", "задача из нескольких действий подряд"),
];

/// Paragraph for the system prompt: requests and the registered tools that serve them.
pub fn tool_hints(reg: &ToolRegistry) -> Option<String> {
    let lines: Vec<String> = HINTS
        .iter()
        .filter(|(tool, _)| reg.get(tool).is_some())
        .map(|(tool, when)| format!("- {when}: {tool}"))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let mut text = format!(
        "Какой инструмент вызывать. Никогда не отвечай «запомнила», «напомню», «открыла» \
и не называй курсы, погоду и новости без вызова инструмента: сначала вызов, потом ответ.\n{}",
        lines.join("\n")
    );
    if reg.get("task_plan").is_some() {
        text.push_str("\n\n");
        text.push_str(TASK_RULES);
    }
    Some(text)
}

/// Registers every tool of the groups that are not in `disabled`.
/// Risk levels come from each tool's `assess`.
pub fn register_all(reg: &mut ToolRegistry, env: &ToolEnv, disabled: &[String]) {
    let on = |group: &str| !disabled.iter().any(|d| d == group);
    let profile = env.user_profile.clone();
    let web = env.shared.web_enabled.clone();
    reg.register(Arc::new(TimeTool));
    reg.register(Arc::new(TaskPlanTool(env.shared.tasks.clone())));
    reg.register(Arc::new(TaskStepTool(env.shared.tasks.clone())));

    if on("system") {
        reg.register(Arc::new(system::VolumeTool(env.volume.clone())));
        reg.register(Arc::new(system::PowerTool(env.power.clone())));
        reg.register(Arc::new(system::ClipboardTool(env.clipboard.clone())));
        reg.register(Arc::new(system::SystemInfoTool(env.probe.clone())));
        reg.register(Arc::new(system::ProcessesTool(env.probe.clone())));
        reg.register(Arc::new(system::KillProcessTool(env.runner.clone())));
        reg.register(Arc::new(devices::RadioTool(env.radios.clone())));
        reg.register(Arc::new(devices::BrightnessTool(env.runner.clone())));
    }

    let ui_env = ui::UiEnv {
        windows: env.windows.clone(),
        ui: env.ui.clone(),
        input: env.input.clone(),
        bus: env.shared.bus.clone(),
        own_process: own_process(),
        seen: Default::default(),
        clipboard: Some(env.clipboard.clone()),
        sent: Default::default(),
    };

    if on("apps") {
        reg.register(Arc::new(
            apps::OpenAppTool::with_start_menu(env.launcher.clone())
                .waiting_for_window(env.windows.clone(), ui_env.own_process.clone()),
        ));
        reg.register(Arc::new(apps::ListWindowsTool(env.windows.clone())));
        reg.register(Arc::new(apps::FocusWindowTool(env.windows.clone())));
        reg.register(Arc::new(apps::CloseAppTool(env.windows.clone())));
        reg.register(Arc::new(ui::ArrangeWindowTool(ui_env.clone())));
    }

    if on("files") {
        reg.register(Arc::new(files::FindFilesTool {
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(files::ReadFileTool {
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(files::WriteFileTool {
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(files::MovePathTool {
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(files::CreateFolderTool {
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(files::DeleteToTrashTool {
            trash: env.trash.clone(),
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(files::OpenPathTool {
            launcher: env.launcher.clone(),
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(archive::ArchiveCreateTool {
            runner: env.runner.clone(),
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(archive::ArchiveExtractTool {
            runner: env.runner.clone(),
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(batch::ListFolderTool {
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(batch::FileBatchTool {
            trash: env.trash.clone(),
            user_profile: profile.clone(),
        }));
    }

    if on("browser") {
        reg.register(Arc::new(browser::BrowserOpenTool {
            browser: env.browser.clone(),
            enabled: env.shared.web_enabled.clone(),
        }));
        reg.register(Arc::new(browser::BrowserReadTool(env.browser.clone())));
        reg.register(Arc::new(browser::BrowserClickTool(env.browser.clone())));
        reg.register(Arc::new(browser::BrowserTypeTool(env.browser.clone())));
        reg.register(Arc::new(browser::BrowserScrollTool(env.browser.clone())));
        reg.register(Arc::new(browser::BrowserBackTool(env.browser.clone())));
    }

    if on("shell") {
        reg.register(Arc::new(shell::RunShellTool(env.runner.clone())));
    }

    if on("web") {
        reg.register(Arc::new(web::OpenUrlTool(env.launcher.clone())));
        reg.register(Arc::new(web::WebSearchTool {
            http: env.http.clone(),
            enabled: web.clone(),
        }));
        reg.register(Arc::new(web::FetchPageTool {
            http: env.http.clone(),
            enabled: web.clone(),
        }));
        reg.register(Arc::new(web::WeatherTool {
            http: env.http.clone(),
            enabled: web.clone(),
        }));
        reg.register(Arc::new(info::RatesTool {
            http: env.http.clone(),
            enabled: web.clone(),
        }));
        reg.register(Arc::new(info::NewsTool {
            http: env.http.clone(),
            enabled: web,
            feed: env.shared.news_feed.clone(),
        }));
    }

    if on("notes") {
        reg.register(Arc::new(notes::NoteCreateTool(env.db.clone())));
        reg.register(Arc::new(notes::NoteSearchTool(env.db.clone())));
        reg.register(Arc::new(notes::NoteReadTool(env.db.clone())));
    }

    if on("daily") {
        let reminders = ReminderStore::new(env.db.clone());
        reg.register(Arc::new(reminders::ReminderSetTool(reminders.clone())));
        reg.register(Arc::new(reminders::ReminderListTool(reminders.clone())));
        reg.register(Arc::new(reminders::ReminderCancelTool(reminders)));
        reg.register(Arc::new(todos::TodoAddTool(env.db.clone())));
        reg.register(Arc::new(todos::TodoListTool(env.db.clone())));
        reg.register(Arc::new(todos::TodoDoneTool(env.db.clone())));
        reg.register(Arc::new(todos::TodoRemoveTool(env.db.clone())));
    }

    if on("memory") {
        let memory = env.shared.memory.clone();
        reg.register(Arc::new(memory::MemorySaveTool(memory.clone())));
        reg.register(Arc::new(memory::MemorySearchTool(memory.clone())));
        reg.register(Arc::new(memory::MemoryForgetTool(memory.clone())));
        reg.register(Arc::new(memory::HistorySearchTool(memory)));
    }

    if on("media") {
        reg.register(Arc::new(media::MediaControlTool(env.media.clone())));
        reg.register(Arc::new(media::NowPlayingTool(env.media.clone())));
    }

    if on("windows") {
        reg.register(Arc::new(ui::UiReadTool(ui_env.clone())));
        reg.register(Arc::new(ui::UiClickTool(ui_env.clone())));
        reg.register(Arc::new(ui::UiTypeTool(ui_env.clone())));
        reg.register(Arc::new(ui::UiKeysTool(ui_env.clone())));
        reg.register(Arc::new(ui::UiScrollTool(ui_env.clone())));
    }

    if on("screen") {
        let screen_env = screen::ScreenEnv {
            ui: ui_env,
            screen: env.screen.clone(),
            enabled: env.shared.screen_enabled.clone(),
            user_profile: profile.clone(),
        };
        reg.register(Arc::new(screen::ScreenLookTool {
            env: screen_env.clone(),
            vision: env.shared.vision.clone(),
        }));
        reg.register(Arc::new(screen::ScreenTextTool(screen_env.clone())));
        reg.register(Arc::new(screen::ScreenshotTool(screen_env)));
    }

    if on("images") {
        reg.register(Arc::new(image::GenerateImageTool {
            images: env.shared.images.clone(),
            user_profile: profile,
        }));
    }

    if on("commands") {
        let commands = CommandStore::new(env.db.clone());
        reg.register(Arc::new(commands::CommandSaveTool(commands.clone())));
        reg.register(Arc::new(commands::CommandListTool(commands.clone())));
        reg.register(Arc::new(commands::CommandDeleteTool(commands)));
    }

    if on("rdp") {
        reg.register(Arc::new(rdp::RdpListTool(env.rdp.clone())));
        reg.register(Arc::new(rdp::RdpOpenTool {
            source: env.rdp.clone(),
            launcher: env.launcher.clone(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_unique_name_and_object_schema() {
        let (env, _) = testing::env();
        let mut reg = ToolRegistry::new();
        register_all(&mut reg, &env, &[]);
        let specs = reg.specs();
        assert_eq!(specs.len(), 70);
        for s in specs {
            assert_eq!(s.parameters["type"], "object", "{}", s.name);
            assert!(!s.description.is_empty());
            assert!(
                s.name.len() <= 64
                    && s.name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{}",
                s.name
            );
        }
    }

    #[test]
    fn hints_name_only_registered_tools() {
        let (env, _) = testing::env();
        let mut reg = ToolRegistry::new();
        register_all(&mut reg, &env, &[]);
        let all = tool_hints(&reg).unwrap();
        for (tool, _) in HINTS {
            assert!(reg.get(tool).is_some(), "hint for unknown tool {tool}");
            assert!(all.contains(&format!(": {tool}")));
        }
        let mut reg = ToolRegistry::new();
        register_all(&mut reg, &env, &["memory".into(), "windows".into()]);
        let some = tool_hints(&reg).unwrap();
        assert!(!some.contains("memory_save") && !some.contains("ui_click"));
        assert!(some.contains("reminder_set"));
        assert_eq!(tool_hints(&ToolRegistry::new()), None);
    }

    #[test]
    fn switched_off_groups_are_not_registered() {
        let (env, _) = testing::env();
        let mut reg = ToolRegistry::new();
        let off: Vec<String> = GROUPS.iter().map(|g| g.to_string()).collect();
        register_all(&mut reg, &env, &off);
        assert_eq!(reg.specs().len(), 3);
        assert!(reg.get("get_time").is_some() && reg.get("task_plan").is_some());

        let mut reg = ToolRegistry::new();
        register_all(&mut reg, &env, &["windows".into(), "screen".into()]);
        assert!(reg.get("ui_click").is_none() && reg.get("screen_look").is_none());
        assert!(reg.get("arrange_window").is_some());
    }
}
