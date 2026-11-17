//! Windows tools. Each one goes through the risk policy via `mika_core::tools::Tool`.

pub mod apps;
pub mod archive;
pub mod avatar;
pub mod batch;
pub mod browser;
pub mod calendar;
pub mod cliphist;
pub mod commands;
pub mod costs;
pub mod day;
pub mod devices;
pub mod direct;
pub mod docs;
pub mod ffmpeg;
pub mod files;
pub mod focus;
pub mod image;
pub mod info;
pub mod journal;
pub mod media;
pub mod memory;
pub mod messenger;
pub mod notes;
pub mod notify;
pub mod observe;
pub mod rdp;
pub mod reminders;
pub mod research;
pub mod screen;
pub mod search;
pub mod settings;
pub mod shell;
pub mod skills;
pub mod sys;
pub mod system;
pub mod todos;
pub mod ui;
pub mod usage;
pub mod watch;
pub mod web;
pub mod win;

mod grid;
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
    "av", "windows", "screen", "images", "commands", "events", "rdp",
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
    (
        "deep_search",
        "разобраться в сложном вопросе по нескольким источникам, сравнить варианты, собрать обзор со ссылками: «исследуй», «изучи», «глубокий поиск»",
    ),
    ("get_time", "который час, какое число, какой день недели"),
    ("volume", "громкость, звук"),
    (
        "app_volume",
        "сделать тише или громче одну программу: браузер, игру, Telegram",
    ),
    ("media_control", "пауза, следующий или предыдущий трек"),
    ("now_playing", "что сейчас играет"),
    ("open_app", "открыть программу"),
    (
        "focus_window",
        "переключиться на окно, сделать его активным; открыть вкладку браузера по названию (аргумент tab)",
    ),
    ("open_url", "открыть сайт в браузере"),
    ("find_files", "найти файл по имени"),
    ("search_text", "найти слово или фразу внутри файлов"),
    ("disk_usage", "что занимает место на диске или в папке"),
    ("run_shell", "выполнить команду PowerShell или cmd"),
    ("system_info", "нагрузка, память, диски"),
    (
        "system_health",
        "компьютер тормозит, лагает, медленно работает, шумит",
    ),
    (
        "ui_read",
        "что в окне программы; первый шаг перед нажатием или вводом в окне",
    ),
    ("ui_click", "нажать кнопку или пункт в окне программы"),
    ("ui_type", "напечатать текст в окне программы"),
    ("ui_keys", "нажать клавиши в окне программы"),
    ("screen_look", "что на экране, описать картинку на экране"),
    (
        "screen_click",
        "нажать в окне, где ui_read не видит кнопок: Telegram, игра, приложение-сайт",
    ),
    ("generate_image", "нарисовать картинку"),
    ("command_save", "сохранить свою команду из нескольких шагов"),
    (
        "skill_save",
        "сохрани это как навык, запомни этот порядок действий, чтобы повторять по имени",
    ),
    ("skill_run", "выполнить сохранённый навык"),
    ("skill_list", "какие есть навыки"),
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
    (
        "focus_mode",
        "не отвлекать, режим фокуса, тишина на время работы",
    ),
    (
        "screen_time",
        "сколько времени провёл за компьютером, в программах, в игре",
    ),
    (
        "clipboard_history",
        "что копировал раньше, вернуть скопированный текст",
    ),
    (
        "calendar_events",
        "встречи, созвоны, события в календаре, что сегодня по расписанию",
    ),
    (
        "day_summary",
        "как прошёл день, итог дня, что сделано сегодня или вчера, план на завтра",
    ),
    (
        "cost_report",
        "сколько потрачено на модели и голос, расходы, бюджет",
    ),
    (
        "docs_search",
        "вопрос по своим документам: договор, инструкция, что написано в файлах",
    ),
    (
        "messenger_send",
        "написать или отправить картинку, фото, файл человеку или в чат Telegram; путь к файлу — в file, не в text",
    ),
    (
        "chat_watch",
        "отвечать кому-то в Telegram за пользователя, следить за перепиской",
    ),
    (
        "trigger_create",
        "делать что-то само при событии: подключили наушники, запустили или закрыли программу, подключились по RDP, отошёл или вернулся",
    ),
    ("trigger_list", "какие есть сценарии по событию"),
    (
        "wait_for",
        "сказать, когда что-то случится: скачается файл, закончится сборка или программа, в окне появится надпись",
    ),
    ("wait_list", "за чем ты сейчас следишь"),
    ("av_info", "видео- или аудиофайл: длительность, размер, кодеки"),
    (
        "av_convert",
        "перевести видео или звук в другой формат, вытащить звук из видео",
    ),
    (
        "video_compress",
        "сжать видео, уменьшить размер файла или разрешение",
    ),
    ("av_cut", "вырезать кусок видео или звука"),
    ("av_join", "склеить несколько видео или звуковых файлов"),
    ("video_gif", "сделать GIF из видео"),
    ("video_rotate", "повернуть видео"),
    ("av_speed", "ускорить или замедлить видео или звук"),
    (
        "audio_process",
        "громкость, шум, тишина по краям, выровнять звук в файле",
    ),
    ("video_subtitles", "вшить субтитры в видео"),
    (
        "av_transcribe",
        "расшифровать речь из видео или записи в текст, сделать субтитры",
    ),
    ("video_describe", "что происходит в видеофайле"),
    (
        "av_folder",
        "сделать одно и то же со всеми видео или звуковыми файлами папки: перевести, сжать, обработать звук, расшифровать",
    ),
    (
        "screen_record",
        "записать видео с экрана или окна, со звуком компьютера",
    ),
    ("screen_record_stop", "остановить запись экрана"),
    (
        "bg_jobs",
        "как идёт фоновая работа с видео и звуком, что в очереди",
    ),
    ("bg_job_stop", "остановить фоновое задание"),
    (
        "avatar_gesture",
        "помахать, поклониться, станцевать, показать жест телом",
    ),
    (
        "settings_get",
        "какие у тебя настройки, какой сейчас голос, громкость голоса, включён ли микрофон",
    ),
    (
        "settings_set",
        "говори тише, громче, быстрее, медленнее; смени голос, характер или язык; включи или выключи свою функцию: озвучку, активационную фразу, диктовку, микрофон, перерывы",
    ),
    (
        "journal_read",
        "почему у самой помощницы что-то не сработало, зависло или сломалось, какие были ошибки",
    ),
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
    // Only moves the model on the screen (decision 0040).
    reg.register(Arc::new(avatar::AvatarGestureTool {
        bus: env.shared.bus.clone(),
        motions: mika_core::paths::motions_dir(),
    }));
    // Her own settings by conversation (decision 0046): about herself, in no group.
    reg.register(Arc::new(settings::SettingsGetTool(
        env.shared.settings.clone(),
    )));
    reg.register(Arc::new(settings::SettingsSetTool(
        env.shared.settings.clone(),
    )));
    reg.register(Arc::new(TaskPlanTool(env.shared.tasks.clone())));
    reg.register(Arc::new(TaskStepTool(env.shared.tasks.clone())));

    if on("system") {
        reg.register(Arc::new(system::VolumeTool(env.volume.clone())));
        reg.register(Arc::new(system::AppVolumeTool(env.volume.clone())));
        reg.register(Arc::new(system::PowerTool(env.power.clone())));
        reg.register(Arc::new(system::ClipboardTool(env.clipboard.clone())));
        reg.register(Arc::new(system::SystemInfoTool(env.probe.clone())));
        reg.register(Arc::new(system::SystemHealthTool(env.probe.clone())));
        reg.register(Arc::new(system::ProcessesTool(env.probe.clone())));
        reg.register(Arc::new(system::KillProcessTool(env.runner.clone())));
        reg.register(Arc::new(usage::ScreenTimeTool {
            store: usage::UsageStore(env.db.clone()),
            enabled: env.shared.app_time_on.clone(),
        }));
        reg.register(Arc::new(cliphist::ClipboardHistoryTool {
            history: env.shared.clips.clone(),
            enabled: env.shared.clipboard_on.clone(),
        }));
        reg.register(Arc::new(devices::RadioTool(env.radios.clone())));
        reg.register(Arc::new(devices::BrightnessTool(env.runner.clone())));
        // The journal the app installed: what it did and what failed.
        reg.register(Arc::new(journal::JournalReadTool(None)));
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
        clips: env.shared.clips.clone(),
    };

    if on("apps") {
        reg.register(Arc::new(
            apps::OpenAppTool::with_start_menu(env.launcher.clone())
                .waiting_for_window(env.windows.clone(), ui_env.own_process.clone()),
        ));
        // Tabs are read like the rest of a window: only with the window tools on.
        let tabs_ui = on("windows").then(|| env.ui.clone());
        reg.register(Arc::new(apps::ListWindowsTool {
            windows: env.windows.clone(),
            ui: tabs_ui.clone(),
        }));
        reg.register(Arc::new(apps::FocusWindowTool {
            windows: env.windows.clone(),
            ui: tabs_ui,
        }));
        reg.register(Arc::new(apps::CloseAppTool(env.windows.clone())));
        reg.register(Arc::new(ui::ArrangeWindowTool(ui_env.clone())));
        reg.register(Arc::new(messenger::MessengerSendTool(screen::ScreenEnv {
            ui: ui_env.clone(),
            screen: env.screen.clone(),
            enabled: env.shared.screen_enabled.clone(),
            user_profile: profile.clone(),
        })));
        reg.register(Arc::new(watch::ChatWatchTool(env.shared.watches.clone())));
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
        reg.register(Arc::new(search::SearchTextTool {
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(search::DiskUsageTool {
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(batch::FileBatchTool {
            trash: env.trash.clone(),
            user_profile: profile.clone(),
        }));
        reg.register(Arc::new(docs::DocsSearchTool(env.shared.docs.clone())));
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
        reg.register(Arc::new(research::DeepSearchTool(
            research::ResearchEnv::new(env),
        )));
        reg.register(Arc::new(web::WeatherTool {
            http: env.http.clone(),
            enabled: web.clone(),
            home: env.shared.home_city.clone(),
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
        reg.register(Arc::new(focus::FocusModeTool(env.shared.focus.clone())));
        reg.register(Arc::new(calendar::CalendarTool(
            env.shared.calendar.clone(),
        )));
        reg.register(Arc::new(day::DaySummaryTool(day::DayEnv::new(env))));
        reg.register(Arc::new(costs::CostReportTool(
            mika_core::costs::installed_or(env.db.clone()),
        )));
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

    if on("av") {
        let av = ffmpeg::AvEnv::new(env);
        // Long calls of these nine answer at once and go on as background jobs.
        reg.register(Arc::new(ffmpeg::AvInfoTool(av.clone())));
        reg.register(Arc::new(ffmpeg::in_background(
            ffmpeg::AvConvertTool(av.clone()),
            &av,
        )));
        reg.register(Arc::new(ffmpeg::in_background(
            ffmpeg::VideoCompressTool(av.clone()),
            &av,
        )));
        reg.register(Arc::new(ffmpeg::in_background(
            ffmpeg::AvCutTool(av.clone()),
            &av,
        )));
        reg.register(Arc::new(ffmpeg::in_background(
            ffmpeg::AvJoinTool(av.clone()),
            &av,
        )));
        reg.register(Arc::new(ffmpeg::VideoGifTool(av.clone())));
        reg.register(Arc::new(ffmpeg::in_background(
            ffmpeg::VideoRotateTool(av.clone()),
            &av,
        )));
        reg.register(Arc::new(ffmpeg::in_background(
            ffmpeg::AvSpeedTool(av.clone()),
            &av,
        )));
        reg.register(Arc::new(ffmpeg::in_background(
            ffmpeg::AudioProcessTool(av.clone()),
            &av,
        )));
        reg.register(Arc::new(ffmpeg::in_background(
            ffmpeg::VideoSubtitlesTool(av.clone()),
            &av,
        )));
        reg.register(Arc::new(ffmpeg::in_background(
            ffmpeg::AvTranscribeTool(av.clone()),
            &av,
        )));
        reg.register(Arc::new(ffmpeg::VideoDescribeTool(av.clone())));
        reg.register(Arc::new(ffmpeg::AvFolderTool::new(av.clone())));
        reg.register(Arc::new(ffmpeg::ScreenRecordTool {
            av: av.clone(),
            ui: ui_env.clone(),
            screen_on: env.shared.screen_enabled.clone(),
            probe: env.probe.clone(),
        }));
        reg.register(Arc::new(ffmpeg::ScreenRecordStopTool(av)));
        reg.register(Arc::new(mika_core::bgjobs::BgJobsTool(
            env.shared.jobs.clone(),
        )));
        reg.register(Arc::new(mika_core::bgjobs::BgJobStopTool(
            env.shared.jobs.clone(),
        )));
    }

    if on("events") {
        let triggers = env.shared.triggers.clone();
        reg.register(Arc::new(observe::TriggerCreateTool {
            store: triggers.clone(),
            commands: CommandStore::new(env.db.clone()),
        }));
        reg.register(Arc::new(observe::TriggerListTool(triggers.clone())));
        reg.register(Arc::new(observe::TriggerDeleteTool(triggers)));
        let waits = env.shared.waits.clone();
        reg.register(Arc::new(observe::WaitForTool(observe::WaitEnv {
            waits: waits.clone(),
            sensors: env.sensors.clone(),
            ui: ui_env.clone(),
            windows_on: on("windows"),
            user_profile: profile.clone(),
        })));
        reg.register(Arc::new(observe::WaitListTool(waits.clone())));
        reg.register(Arc::new(observe::WaitStopTool(waits)));
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
        reg.register(Arc::new(screen::ScreenClickTool {
            env: screen_env.clone(),
            vision: env.shared.vision.clone(),
        }));
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
        // Skills (decision 0047): her own routines, saved by her and run by name.
        let skills = mika_core::skills::SkillStore::new(env.db.clone());
        reg.register(Arc::new(skills::SkillSaveTool {
            store: skills.clone(),
            book: env.shared.skills.clone(),
        }));
        reg.register(Arc::new(skills::SkillListTool(skills.clone())));
        reg.register(Arc::new(skills::SkillDeleteTool(skills.clone())));
        reg.register(Arc::new(skills::SkillRunTool(skills)));
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
        assert_eq!(specs.len(), 116);
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
        assert_eq!(reg.specs().len(), 6);
        assert!(reg.get("get_time").is_some() && reg.get("task_plan").is_some());
        // A gesture only moves the model on the screen: it belongs to no group.
        assert!(reg.get("avatar_gesture").is_some());
        // Her own settings are about herself: no group either (decision 0046).
        assert!(reg.get("settings_get").is_some() && reg.get("settings_set").is_some());

        let mut reg = ToolRegistry::new();
        register_all(&mut reg, &env, &["windows".into(), "screen".into()]);
        assert!(reg.get("ui_click").is_none() && reg.get("screen_look").is_none());
        assert!(reg.get("arrange_window").is_some());
    }

    #[test]
    fn the_journal_tool_is_a_system_tool_of_low_risk() {
        use mika_core::tools::RiskLevel;

        let (env, _) = testing::env();
        let all = reg_with(&env, &[]);
        let tool = all.get("journal_read").expect("journal_read is registered");
        // Whatever it is asked, it only reads: the policy lets it through.
        for args in [
            serde_json::json!({}),
            serde_json::json!({"level": "debug", "minutes": 1440, "limit": 100}),
        ] {
            assert_eq!(tool.assess(&args).unwrap().level, RiskLevel::Low);
        }
        let without = reg_with(&env, &["system"]);
        assert!(without.get("journal_read").is_none());
        assert!(tool_hints(&all).unwrap().contains(": journal_read"));
    }

    #[test]
    fn the_events_group_has_six_tools_and_reads_windows_only_with_the_window_group() {
        use mika_core::tools::ToolError;
        use serde_json::json;

        const EVENTS: [&str; 6] = [
            "trigger_create",
            "trigger_list",
            "trigger_delete",
            "wait_for",
            "wait_list",
            "wait_stop",
        ];
        let (env, f) = testing::env();
        let all = reg_with(&env, &[]);
        assert!(EVENTS.iter().all(|t| all.get(t).is_some()));
        let without = reg_with(&env, &["events"]);
        assert!(EVENTS.iter().all(|t| without.get(t).is_none()));
        assert_eq!(all.len() - without.len(), EVENTS.len());

        // A watch of a window is a way to read it: off with the window tools.
        *f.windows.lock() = vec![sys::WindowInfo {
            id: 7,
            title: "Установка".into(),
            process: "setup.exe".into(),
        }];
        let phrase = json!({"what": "window_text", "window": "setup", "text": "Готово"});
        assert!(reg_with(&env, &[])
            .get("wait_for")
            .unwrap()
            .assess(&phrase)
            .is_ok());
        let wait = reg_with(&env, &["windows"]).get("wait_for").unwrap();
        assert!(matches!(
            wait.assess(&phrase),
            Err(ToolError::Failed(why)) if why.contains("Управление окнами")
        ));
        assert!(wait
            .assess(&json!({"what": "file_ready", "path": "a.zip"}))
            .is_ok());
    }

    fn reg_with(env: &ToolEnv, off: &[&str]) -> ToolRegistry {
        let off: Vec<String> = off.iter().map(|g| g.to_string()).collect();
        let mut reg = ToolRegistry::new();
        register_all(&mut reg, env, &off);
        reg
    }
}
