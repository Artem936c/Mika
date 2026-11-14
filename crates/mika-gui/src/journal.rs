//! UI commands of the journal page of the settings (decision 0036): reading
//! the journal of the app, its settings, saving it to a file, clearing it and
//! the notice about a run that did not end cleanly. The text of «Сообщить о
//! проблеме» is put together here too: it is the same journal.
//!
//! Nothing leaves the PC: a file is written only where the user says in the
//! "Save as" dialog, and the report is a text for the user to copy.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Duration, FixedOffset, Local, NaiveDate, SecondsFormat};
use mika_core::config::{Config, LogConfig};
use mika_core::journal::line::format_line;
use mika_core::journal::{self, Journal, Level, Page, Query, Row, Tail, MAX_PAGE};

use crate::app::Ctx;
use crate::file_dialog::pick_save;
use crate::strings::tr;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// The longest text the page may search for, characters.
const MAX_SEARCH_CHARS: usize = 200;
/// The last records a problem report carries.
const REPORT_RECORDS: usize = 200;
/// The most errors of the last day a problem report adds to them.
const REPORT_ERRORS: usize = 100;
/// What the "Save as" dialog offers.
const FILE_PATTERN: &str = "*.txt";
const FILE_EXTENSION: &str = "txt";

/// One "Save as" dialog at a time.
static EXPORTING: AtomicBool = AtomicBool::new(false);

/// What the page shows besides the records. Every command that changes
/// something answers with it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct JournalView {
    level: Level,
    content: bool,
    keep_days: u32,
    keep_files: u32,
    dir: String,
    size_bytes: u64,
    files: u32,
    /// Time of the oldest record; `None` for an empty journal.
    oldest: Option<String>,
    /// The run before this one did not end cleanly, and the notice about it
    /// has not been closed yet.
    crashed: bool,
}

/// Reads the folder of the journal: call it off the main thread.
fn view_of(journal: &Journal) -> JournalView {
    let config = journal.config();
    let summary = journal.summary();
    JournalView {
        level: config.level,
        content: config.content,
        keep_days: config.keep_days,
        keep_files: config.keep_files,
        dir: journal
            .dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default(),
        size_bytes: summary.size_bytes,
        files: summary.files,
        oldest: summary.oldest,
        crashed: journal.previous_run_crashed(),
    }
}

/// The query of the page as the reader gets it: no more than `MAX_PAGE`
/// records at once and a search text of a sane length.
fn page_query(mut query: Query) -> Query {
    query.limit = query.limit.min(MAX_PAGE);
    if query.text.chars().count() > MAX_SEARCH_CHARS {
        query.text = query.text.chars().take(MAX_SEARCH_CHARS).collect();
    }
    query
}

/// What the journal says about a change of its own settings: nothing when
/// nothing changed. Turning the texts on is a warning, so that whoever reads
/// the journal later sees from which record on it may hold them.
fn change_notes(old: &LogConfig, new: &LogConfig) -> Vec<(Level, String)> {
    let mut notes = Vec::new();
    if old.level != new.level {
        notes.push((
            Level::Info,
            format!(
                "journal: level {} -> {}",
                old.level.name(),
                new.level.name()
            ),
        ));
    }
    match (old.content, new.content) {
        (false, true) => notes.push((
            Level::Warn,
            "journal: texts and arguments are written from now on".into(),
        )),
        (true, false) => notes.push((
            Level::Info,
            "journal: texts and arguments are no longer written".into(),
        )),
        _ => {}
    }
    if (old.keep_days, old.keep_files) != (new.keep_days, new.keep_files) {
        notes.push((
            Level::Info,
            format!(
                "journal: kept for {} days, {} earlier files",
                new.keep_days, new.keep_files
            ),
        ));
    }
    notes
}

/// What a report tells about the app besides its settings.
pub struct Facts {
    /// Edition and build of Windows.
    pub windows: String,
    /// Tools the model is offered now.
    pub tools: usize,
    /// State of the Telegram bridge, as the settings page shows it.
    pub telegram: String,
}

fn facts(ctx: &Ctx) -> Facts {
    Facts {
        windows: crate::platform::windows_build(),
        tools: ctx.session.agent.registry().len(),
        telegram: ctx.telegram_status.lock().clone(),
    }
}

/// The lines every report starts with: the version, Windows and the settings
/// that matter for a diagnosis. No key is among the settings; an address is
/// scrubbed like a record of the journal.
fn report_head(c: &Config, facts: &Facts) -> Vec<String> {
    let lines = [
        format!("Mika {}", env!("CARGO_PKG_VERSION")),
        format!("Windows: {}", facts.windows),
        format!(
            "Provider: {} · {} · {}",
            c.llm.kind.as_str(),
            c.llm.effective_base_url(),
            c.llm.model
        ),
        format!(
            "Tools: {} (off: {})",
            facts.tools,
            c.tools.disabled_groups.join(", ")
        ),
        format!(
            "Web: {} · Screen: {} · Microphone: {} · Telegram: {} · MCP servers: {}",
            c.tools.web_enabled,
            c.privacy.screen_enabled,
            c.privacy.microphone_enabled,
            facts.telegram,
            c.mcp.servers.len()
        ),
        format!(
            "Embeddings: {}",
            if c.embedding.model.is_empty() {
                "off"
            } else {
                c.embedding.model.as_str()
            }
        ),
        format!(
            "Journal: level {} · texts and arguments {} · kept {} days, {} earlier files",
            c.log.level.name(),
            if c.log.content { "on" } else { "off" },
            c.log.keep_days,
            c.log.keep_files
        ),
    ];
    lines.iter().map(|line| journal::scrub(line)).collect()
}

/// A record as a line of a report: the line of the file without its end.
fn row_line(row: &Row) -> String {
    let line = format_line(&row.t, row.level, &row.category, &row.message);
    line.trim_end_matches('\n').to_string()
}

/// The text of «Сообщить о проблеме»: the head, the errors of the last day
/// that are older than the last records, and the last records, everything
/// from the oldest to the newest. `recent` and `errors` come as the reader
/// gives them, the newest first.
fn problem_text(head: &[String], recent: &[Row], errors: &[Row]) -> String {
    let same =
        |a: &Row, b: &Row| (&a.t, &a.category, &a.message) == (&b.t, &b.category, &b.message);
    let earlier: Vec<&Row> = errors
        .iter()
        .rev()
        .filter(|error| !recent.iter().any(|row| same(row, error)))
        .collect();
    let mut out = head.join("\n");
    if !earlier.is_empty() {
        out.push_str(&format!(
            "\n\n--- earlier errors of the last 24 hours: {} ---",
            earlier.len()
        ));
        for row in earlier {
            out.push('\n');
            out.push_str(&row_line(row));
        }
    }
    out.push_str(&format!(
        "\n\n--- journal, last records: {} ---",
        recent.len()
    ));
    for row in recent.iter().rev() {
        out.push('\n');
        out.push_str(&row_line(row));
    }
    out
}

/// The two readings of a problem report: the last records and the errors
/// written since `since`, both the newest first.
fn report_rows(journal: &Journal, since: DateTime<FixedOffset>) -> (Vec<Row>, Vec<Row>) {
    let recent = journal.read(&Query {
        limit: REPORT_RECORDS,
        ..Query::default()
    });
    let errors = journal.read(&Query {
        levels: vec![Level::Error],
        limit: REPORT_ERRORS,
        since: Some(since),
        ..Query::default()
    });
    (recent.rows, errors.rows)
}

/// Version, settings without secrets and the end of the journal, for the user
/// to copy. Nothing is sent anywhere. Reads files: call it off the main thread.
pub fn problem_report(ctx: &Ctx) -> String {
    let config = ctx.config.lock().clone();
    let head = report_head(&config, &facts(ctx));
    let (recent, errors) = match journal::global() {
        Some(journal) => report_rows(&journal, Local::now().fixed_offset() - Duration::hours(24)),
        None => Default::default(),
    };
    problem_text(&head, &recent, &errors)
}

/// `mika-journal-2026-11-14.txt`.
fn export_name(date: NaiveDate) -> String {
    format!("mika-journal-{}.{FILE_EXTENSION}", date.format("%Y-%m-%d"))
}

/// What a saved file is limited to, as a line of its header; `None` for the
/// whole journal. The searched text is the user's: it is named only as far
/// as the journal itself would write it.
fn filter_line(query: Option<&Query>) -> Option<String> {
    let query = query?;
    let mut parts = Vec::new();
    if !query.levels.is_empty() {
        let names: Vec<&str> = query.levels.iter().map(|level| level.name()).collect();
        parts.push(format!("levels {}", names.join(", ")));
    }
    if !query.categories.is_empty() {
        parts.push(format!("categories {}", query.categories.join(", ")));
    }
    if !query.text.trim().is_empty() {
        parts.push(format!("text {}", journal::content(query.text.trim())));
    }
    (!parts.is_empty()).then(|| format!("Filter: {}", parts.join(" · ")))
}

/// The header of a saved file: what a problem report starts with, when the
/// file was saved and what it is limited to.
fn export_header(head: Vec<String>, saved: DateTime<Local>, query: Option<&Query>) -> Vec<String> {
    let mut header = head;
    header.push(format!(
        "Saved: {}",
        saved.to_rfc3339_opts(SecondsFormat::Secs, false)
    ));
    header.extend(filter_line(query));
    header
}

fn installed() -> Result<Arc<Journal>, String> {
    journal::global().ok_or_else(|| tr("error.journal_off"))
}

/// The commands of this page read files: none of them runs on the main thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn journal_view() -> Result<JournalView, String> {
    blocking(|| {
        let journal = installed()?;
        Ok(view_of(&journal))
    })
    .await
}

/// One page of records, the newest first. `next` of the answer is `before`
/// of the query for the page after it.
#[tauri::command]
pub async fn journal_read(query: Query) -> Result<Page, String> {
    blocking(move || Ok(installed()?.read(&page_query(query)))).await
}

/// Records from memory with a number above `after`, the oldest first: what
/// «Следить» adds to the list.
#[tauri::command(async)]
pub fn journal_tail(after: Option<u64>) -> Result<Tail, String> {
    Ok(installed()?.tail(after))
}

/// Which settings of the journal are in force after a change: the new ones
/// only when the file took them.
fn in_force(old: &LogConfig, new: &LogConfig, saved: bool) -> LogConfig {
    if saved {
        new.clone()
    } else {
        old.clone()
    }
}

/// Saves the `[log]` settings and applies them at once. A change that could
/// not be saved is not applied: the error is returned and the settings stay
/// as they were.
#[tauri::command]
pub async fn set_journal(ctx: CtxState<'_>, settings: LogConfig) -> Result<JournalView, String> {
    let ctx = ctx.inner().clone();
    blocking(move || {
        let journal = installed()?;
        let new = settings.normalized();
        let old = std::mem::replace(&mut ctx.config.lock().log, new.clone());
        let saved = ctx.save_config();
        if let Err(why) = saved {
            // The file did not take the settings: nothing changes. The page
            // redraws what it had, and that must be what is in force. Left
            // in force without the file, "write texts" would be on with its
            // switch drawn off.
            ctx.config.lock().log = in_force(&old, &new, false);
            log::warn!(
                target: "app",
                "journal: settings not saved, the earlier ones stay in force"
            );
            return Err(why);
        }
        let notes = change_notes(&old, &new);
        let write = |notes: &[(Level, String)]| {
            for (level, note) in notes {
                journal.write(*level, "app", note);
            }
        };
        // The change is written under whichever level lets more through, so
        // that a switch to «errors only» does not hide itself.
        let before = old.level >= new.level;
        if before {
            write(&notes);
        }
        journal::apply(&new);
        if !before {
            write(&notes);
        }
        Ok(view_of(&journal))
    })
    .await
}

/// Saves the journal, or the part of it the query wants, as one text file
/// where the user says. `None` when the dialog was closed.
#[tauri::command]
pub async fn journal_export(
    window: tauri::WebviewWindow,
    ctx: CtxState<'_>,
    query: Option<Query>,
) -> Result<Option<String>, String> {
    if EXPORTING.swap(true, Ordering::SeqCst) {
        // A dialog is already open.
        return Ok(None);
    }
    let owner = window.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    let ctx = ctx.inner().clone();
    let done = blocking(move || export_blocking(&ctx, owner, query.as_ref())).await;
    EXPORTING.store(false, Ordering::SeqCst);
    done
}

fn export_blocking(
    ctx: &Ctx,
    owner: isize,
    query: Option<&Query>,
) -> Result<Option<String>, String> {
    let journal = installed()?;
    let now = Local::now();
    let Some(path) = pick_save(
        owner,
        &tr("journal_file.dialog_save"),
        (&tr("journal_file.dialog_filter"), FILE_PATTERN),
        &export_name(now.date_naive()),
        FILE_EXTENSION,
    )?
    else {
        return Ok(None);
    };
    let config = ctx.config.lock().clone();
    let header = export_header(report_head(&config, &facts(ctx)), now, query);
    let count = journal
        .export(query, &header, &path)
        .map_err(|e| tr("error.journal_export").replace("{why}", &e.to_string()))?;
    log::info!(target: "app", "journal saved to a file: {count} records");
    Ok(Some(path.display().to_string()))
}

/// Removes every record, the files included.
#[tauri::command]
pub async fn journal_clear() -> Result<JournalView, String> {
    blocking(|| {
        let journal = installed()?;
        journal
            .clear()
            .map_err(|e| tr("error.journal_clear").replace("{why}", &e.to_string()))?;
        Ok(view_of(&journal))
    })
    .await
}

/// The notice about the run that did not end cleanly was read or closed: it
/// is not shown again.
#[tauri::command]
pub fn journal_crash_seen() {
    if let Some(journal) = journal::global() {
        journal.acknowledge_crash();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::journal::Options;

    fn open(dir: &std::path::Path) -> Arc<Journal> {
        Journal::open(Options::in_dir(dir, LogConfig::default()))
    }

    fn row(t: &str, level: Level, category: &str, message: &str) -> Row {
        Row {
            seq: None,
            t: t.into(),
            level,
            category: category.into(),
            message: message.into(),
        }
    }

    fn facts() -> Facts {
        Facts {
            windows: "Windows 11 Pro 24H2 (build 26100.2314)".into(),
            tools: 109,
            telegram: "off".into(),
        }
    }

    #[test]
    fn a_change_that_was_not_saved_does_not_come_into_force() {
        let old = LogConfig::default();
        let new = LogConfig {
            content: true,
            level: Level::Debug,
            ..LogConfig::default()
        };
        // The page redraws the old values after a refused save: they must
        // be the ones in force, above all "write texts".
        assert_eq!(in_force(&old, &new, false), old);
        assert!(!in_force(&old, &new, false).content);
        assert_eq!(in_force(&old, &new, true), new);
        // Switching texts off that could not be saved leaves them on, and
        // the page shows them on beside the error.
        assert!(in_force(&new, &old, false).content);
    }

    #[test]
    fn the_view_has_the_fields_the_page_reads() {
        let dir = tempfile::tempdir().unwrap();
        let journal = open(dir.path());
        // An empty journal: the file is there, nothing is dated.
        let empty = view_of(&journal);
        assert_eq!((empty.size_bytes, empty.oldest.clone()), (0, None));
        assert!(!empty.crashed);

        journal.write(Level::Info, "app", "first record");
        journal.write(Level::Warn, "tool", "second record");
        let view = view_of(&journal);
        let oldest = view.oldest.clone().expect("a dated journal");
        assert!(DateTime::parse_from_rfc3339(&oldest).is_ok(), "{oldest}");
        assert!(view.size_bytes > 0);
        assert_eq!(
            serde_json::to_value(&view).unwrap(),
            serde_json::json!({
                "level": "info",
                "content": false,
                "keep_days": 14,
                "keep_files": 5,
                "dir": dir.path().display().to_string(),
                "size_bytes": view.size_bytes,
                "files": 1,
                "oldest": oldest,
                "crashed": false,
            })
        );
    }

    #[test]
    fn the_view_follows_the_settings_and_the_notice_of_an_unclean_run() {
        let dir = tempfile::tempdir().unwrap();
        // A run that never said it ended.
        open(dir.path()).begin_run();
        let journal = open(dir.path());
        assert!(journal.begin_run());
        assert!(view_of(&journal).crashed);
        // The user has read the notice: it is not shown again.
        journal.acknowledge_crash();
        assert!(!view_of(&journal).crashed);

        journal.apply(&LogConfig {
            level: Level::Debug,
            content: true,
            keep_days: 9000,
            keep_files: 0,
        });
        let view = view_of(&journal);
        assert_eq!((view.level, view.content), (Level::Debug, true));
        // The retention is shown as it is in force.
        assert_eq!((view.keep_days, view.keep_files), (365, 1));
        // A clean end leaves nothing for the next run to warn about.
        journal.end_run();
        assert!(!open(dir.path()).begin_run());
    }

    #[test]
    fn a_page_is_never_longer_than_500_records() {
        let query: Query = serde_json::from_value(serde_json::json!({
            "levels": ["error", "warn"],
            "categories": ["tool"],
            "text": "failed",
            "before": null,
            "limit": 100_000,
        }))
        .unwrap();
        let capped = page_query(query);
        assert_eq!(capped.limit, 500);
        assert_eq!(capped.levels, [Level::Error, Level::Warn]);
        assert_eq!(capped.categories, ["tool"]);
        assert_eq!((capped.text.as_str(), capped.before), ("failed", None));
        // A smaller page and the mark of the next one pass as they are.
        let query: Query =
            serde_json::from_value(serde_json::json!({ "limit": 50, "before": "mark" })).unwrap();
        let passed = page_query(query);
        assert_eq!((passed.limit, passed.before.as_deref()), (50, Some("mark")));
        // An empty query is the default page of the reader.
        assert_eq!(page_query(Query::default()), Query::default());
        // A search text is cut by characters, not bytes.
        let long = page_query(Query {
            text: "ѣ".repeat(MAX_SEARCH_CHARS + 50),
            ..Query::default()
        });
        assert_eq!(long.text, "ѣ".repeat(MAX_SEARCH_CHARS));
        // A level nobody knows is refused, not read as «all».
        assert!(
            serde_json::from_value::<Query>(serde_json::json!({ "levels": ["loud"] })).is_err()
        );
    }

    #[test]
    fn the_capped_page_reads_a_real_journal() {
        let dir = tempfile::tempdir().unwrap();
        let journal = open(dir.path());
        for i in 0..520 {
            journal.write(Level::Info, "app", &format!("record {i}"));
        }
        let page = journal.read(&page_query(Query {
            limit: 100_000,
            ..Query::default()
        }));
        assert_eq!(page.rows.len(), 500);
        assert_eq!(page.rows[0].message, "record 519");
        let rest = journal.read(&page_query(Query {
            before: page.next,
            limit: 100_000,
            ..Query::default()
        }));
        assert_eq!(rest.rows.len(), 20);
        assert_eq!(rest.next, None);
    }

    #[test]
    fn a_change_of_the_settings_is_written_down() {
        let old = LogConfig::default();
        assert!(change_notes(&old, &old).is_empty());

        let mut new = old.clone();
        new.content = true;
        assert_eq!(
            change_notes(&old, &new),
            [(
                Level::Warn,
                "journal: texts and arguments are written from now on".to_string()
            )]
        );
        assert_eq!(
            change_notes(&new, &old),
            [(
                Level::Info,
                "journal: texts and arguments are no longer written".to_string()
            )]
        );

        let mut new = old.clone();
        new.level = Level::Debug;
        new.keep_days = 30;
        assert_eq!(
            change_notes(&old, &new),
            [
                (Level::Info, "journal: level info -> debug".to_string()),
                (
                    Level::Info,
                    "journal: kept for 30 days, 5 earlier files".to_string()
                ),
            ]
        );
    }

    #[test]
    fn the_head_of_a_report_names_the_app_windows_and_the_settings() {
        let mut config = Config::default();
        config.tools.disabled_groups = vec!["rdp".into(), "shell".into()];
        config.tools.web_enabled = true;
        config.log.level = Level::Debug;
        let head = report_head(&config, &facts());
        assert_eq!(head.len(), 7);
        assert_eq!(head[0], format!("Mika {}", env!("CARGO_PKG_VERSION")));
        assert_eq!(head[1], "Windows: Windows 11 Pro 24H2 (build 26100.2314)");
        assert!(head[2].starts_with("Provider: "), "{}", head[2]);
        assert!(head[2].ends_with(&format!(" · {}", config.llm.model)));
        assert_eq!(head[3], "Tools: 109 (off: rdp, shell)");
        assert_eq!(
            head[4],
            "Web: true · Screen: false · Microphone: false · Telegram: off · MCP servers: 0"
        );
        assert_eq!(head[5], "Embeddings: off");
        assert_eq!(
            head[6],
            "Journal: level debug · texts and arguments off · kept 14 days, 5 earlier files"
        );
    }

    #[test]
    fn an_address_with_a_key_in_it_does_not_reach_a_report() {
        let mut config = Config::default();
        config.llm.base_url =
            "https://user:hunter2hunter2@llm.example/v1?api_key=abcdef123456".into();
        let head = report_head(&config, &facts()).join("\n");
        assert!(head.contains("llm.example"), "{head}");
        assert!(!head.contains("hunter2hunter2"), "{head}");
        assert!(!head.contains("abcdef123456"), "{head}");
    }

    #[test]
    fn a_problem_report_goes_from_the_oldest_to_the_newest_without_repeats() {
        let head = vec!["Mika 0.4.0".to_string(), "Windows: Windows".to_string()];
        // As the reader gives them: the newest first.
        let recent = vec![
            row(
                "2026-11-14T12:00:03.000+03:00",
                Level::Info,
                "turn",
                "turn finished",
            ),
            row(
                "2026-11-14T12:00:02.000+03:00",
                Level::Error,
                "tool",
                "tool failed:\tno such file",
            ),
            row(
                "2026-11-14T12:00:01.000+03:00",
                Level::Info,
                "turn",
                "turn started",
            ),
        ];
        let errors = vec![
            row(
                "2026-11-14T12:00:02.000+03:00",
                Level::Error,
                "tool",
                "tool failed:\tno such file",
            ),
            row(
                "2026-11-14T09:30:00.000+03:00",
                Level::Error,
                "llm",
                "http 500",
            ),
            row(
                "2026-11-14T08:00:00.000+03:00",
                Level::Error,
                "voice",
                "no microphone",
            ),
        ];
        assert_eq!(
            problem_text(&head, &recent, &errors),
            "Mika 0.4.0\n\
             Windows: Windows\n\
             \n\
             --- earlier errors of the last 24 hours: 2 ---\n\
             2026-11-14T08:00:00.000+03:00\tERROR\tvoice\tno microphone\n\
             2026-11-14T09:30:00.000+03:00\tERROR\tllm\thttp 500\n\
             \n\
             --- journal, last records: 3 ---\n\
             2026-11-14T12:00:01.000+03:00\tINFO\tturn\tturn started\n\
             2026-11-14T12:00:02.000+03:00\tERROR\ttool\ttool failed:\\tno such file\n\
             2026-11-14T12:00:03.000+03:00\tINFO\tturn\tturn finished"
        );
        // Nothing earlier and nothing at all.
        assert_eq!(
            problem_text(&head, &recent[..1], &[]),
            "Mika 0.4.0\nWindows: Windows\n\n--- journal, last records: 1 ---\n\
             2026-11-14T12:00:03.000+03:00\tINFO\tturn\tturn finished"
        );
        assert_eq!(
            problem_text(&head, &[], &[]),
            "Mika 0.4.0\nWindows: Windows\n\n--- journal, last records: 0 ---"
        );
    }

    #[test]
    fn a_problem_report_reads_200_records_and_the_errors_of_the_last_day() {
        let dir = tempfile::tempdir().unwrap();
        let journal = open(dir.path());
        for i in 0..120 {
            journal.write(Level::Error, "llm", &format!("early failure {i}"));
        }
        for i in 0..250 {
            journal.write(Level::Info, "turn", &format!("step {i}"));
        }
        journal.write(Level::Error, "tool", "late failure");
        let day = Local::now().fixed_offset() - Duration::hours(24);
        let (recent, errors) = report_rows(&journal, day);
        assert_eq!(recent.len(), REPORT_RECORDS);
        assert_eq!(recent[0].message, "late failure");
        // No more than a hundred errors, the newest ones.
        assert_eq!(errors.len(), REPORT_ERRORS);
        assert_eq!(errors[0].message, "late failure");
        assert_eq!(errors[1].message, "early failure 119");
        assert!(errors.iter().all(|row| row.level == Level::Error));

        let text = problem_text(&["head".into()], &recent, &errors);
        // The late failure is among the last records: it is not said twice.
        assert_eq!(text.matches("late failure").count(), 1);
        assert!(text.contains("--- earlier errors of the last 24 hours: 99 ---"));
        assert!(text.contains("--- journal, last records: 200 ---"));
        assert!(text.ends_with("\tERROR\ttool\tlate failure"));

        // Errors older than the day are left out.
        let later = Local::now().fixed_offset() + Duration::hours(1);
        assert!(report_rows(&journal, later).1.is_empty());
    }

    #[test]
    fn a_saved_file_is_named_by_the_day_and_says_what_it_holds() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        assert_eq!(export_name(date), "mika-journal-2026-11-14.txt");
        assert_eq!(FILE_PATTERN, format!("*.{FILE_EXTENSION}"));

        assert_eq!(filter_line(None), None);
        assert_eq!(filter_line(Some(&Query::default())), None);
        let query = Query {
            levels: vec![Level::Error, Level::Warn],
            categories: vec!["tool".into(), "llm".into()],
            text: " where is my report ".into(),
            ..Query::default()
        };
        // The searched text is the user's: only its length is named.
        assert_eq!(
            filter_line(Some(&query)).as_deref(),
            Some("Filter: levels error, warn · categories tool, llm · text <hidden 18 chars>")
        );

        let saved = Local::now();
        let header = export_header(vec!["Mika 0.4.0".into()], saved, Some(&query));
        assert_eq!(header.len(), 3);
        assert_eq!(header[0], "Mika 0.4.0");
        let stamp = header[1].strip_prefix("Saved: ").unwrap();
        assert!(DateTime::parse_from_rfc3339(stamp).is_ok(), "{stamp}");
        assert!(header[2].starts_with("Filter: levels error, warn"));
        assert_eq!(export_header(vec![], saved, None).len(), 1);
    }

    #[test]
    fn a_saved_file_starts_with_the_header_and_holds_the_wanted_records() {
        let dir = tempfile::tempdir().unwrap();
        let journal = open(&dir.path().join("logs"));
        journal.write(Level::Info, "app", "started");
        journal.write(Level::Error, "tool", "tool failed");
        journal.write(Level::Warn, "llm", "slow answer");

        let header = export_header(
            report_head(&Config::default(), &facts()),
            Local::now(),
            None,
        );
        let whole = dir.path().join("whole.txt");
        assert_eq!(journal.export(None, &header, &whole).unwrap(), 3);
        let text = std::fs::read_to_string(&whole).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), header.len() + 3);
        assert_eq!(lines[0], format!("# Mika {}", env!("CARGO_PKG_VERSION")));
        assert_eq!(
            lines[1],
            "# Windows: Windows 11 Pro 24H2 (build 26100.2314)"
        );
        assert!(lines[..header.len()]
            .iter()
            .all(|line| line.starts_with("# ")));
        assert!(lines[header.len()].ends_with("\tINFO\tapp\tstarted"));
        assert!(lines[header.len() + 2].ends_with("\tWARN\tllm\tslow answer"));

        // Only what the page shows.
        let query = Query {
            levels: vec![Level::Error],
            ..Query::default()
        };
        let part = dir.path().join("part.txt");
        let header = export_header(vec!["head".into()], Local::now(), Some(&query));
        assert_eq!(journal.export(Some(&query), &header, &part).unwrap(), 1);
        let text = std::fs::read_to_string(&part).unwrap();
        assert!(text.contains("\n# Filter: levels error\n"), "{text}");
        assert!(text.ends_with("\tERROR\ttool\ttool failed\n"), "{text}");
    }

    #[test]
    fn the_texts_of_the_page_commands_are_in_the_locale_file() {
        for key in [
            "journal_file.dialog_save",
            "journal_file.dialog_filter",
            "error.journal_off",
            "error.journal_export",
            "error.journal_clear",
        ] {
            assert_ne!(tr(key), key, "{key} is missing in ru.json");
        }
        for key in ["error.journal_export", "error.journal_clear"] {
            assert!(tr(key).contains("{why}"), "{key}");
        }
    }
}
