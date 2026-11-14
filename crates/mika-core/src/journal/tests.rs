//! Tests of the journal as a whole: a private sink over a temporary folder,
//! never the logger of the process.

use std::fmt;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use log::Log;

use super::line::parse_line;
use super::*;
use crate::config::LogConfig;
use crate::secrets::Secret;

fn open(dir: &Path) -> Arc<Journal> {
    Journal::open(Options::in_dir(dir, LogConfig::default()))
}

/// A journal with small files that keeps `keep_files` earlier ones.
fn open_small(dir: &Path, max_file_bytes: u64, keep_files: u32) -> Arc<Journal> {
    Journal::open(Options {
        max_file_bytes,
        ..Options::in_dir(
            dir,
            LogConfig {
                keep_files,
                ..LogConfig::default()
            },
        )
    })
}

fn in_memory() -> Arc<Journal> {
    Journal::open(Options::default())
}

/// Writes a record the way a `log` macro does.
fn emit(journal: &Journal, level: log::Level, target: &str, text: &str) {
    journal.log(
        &log::Record::builder()
            .level(level)
            .target(target)
            .args(format_args!("{text}"))
            .build(),
    );
}

fn info(journal: &Journal, text: &str) {
    emit(journal, log::Level::Info, "mika_core::journal::tests", text);
}

/// Writes records `record 0000` … with some padding, numbered from `from`.
fn fill(journal: &Journal, from: usize, count: usize) {
    for n in from..from + count {
        info(journal, &format!("record {n:04} {}", "~".repeat(n % 40)));
    }
}

fn number(row: &Row) -> usize {
    row.message
        .strip_prefix("record ")
        .and_then(|rest| rest.get(..4))
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("not a numbered record: {:?}", row.message))
}

/// Every page of a query, one after another.
fn all_pages(journal: &Journal, mut query: Query) -> Vec<Row> {
    let mut rows = Vec::new();
    loop {
        let page = journal.read(&query);
        assert!(page.rows.len() <= query.limit.clamp(1, MAX_PAGE));
        rows.extend(page.rows);
        match page.next {
            Some(next) => query.before = Some(next),
            None => return rows,
        }
    }
}

fn limit(limit: usize) -> Query {
    Query {
        limit,
        ..Query::default()
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn current(dir: &Path) -> String {
    String::from_utf8(std::fs::read(dir.join("mika.log")).unwrap()).unwrap()
}

/// Sets the time a file was last written to `ago` before now.
fn age(path: &Path, ago: Duration) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - ago).unwrap();
}

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Ends a journal the way the end of a run does, and waits until it is gone:
/// the thread that writes the buffer out may hold it for a moment.
fn close(journal: Arc<Journal>) {
    let gone = Arc::downgrade(&journal);
    drop(journal);
    let started = Instant::now();
    while gone.strong_count() > 0 {
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::yield_now();
    }
}

/// Runs `work` on a thread and fails when it does not end in time: a
/// deadlock becomes a failed test, not a hung one.
fn in_time<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, wait) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(work());
    });
    wait.recv_timeout(Duration::from_secs(20))
        .expect("the journal did not answer in time")
}

#[test]
fn a_record_is_one_line_with_four_fields() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    emit(&j, log::Level::Warn, "tool", "open failed:\tno file\nC:\\x");
    let text = current(d.path());
    assert_eq!(text.lines().count(), 1);
    assert!(text.ends_with('\n'));
    let fields: Vec<&str> = text.trim_end_matches('\n').split('\t').collect();
    assert_eq!(fields.len(), 4, "{text:?}");
    assert_eq!(fields[1], "WARN");
    assert_eq!(fields[2], "tool");
    assert_eq!(fields[3], "open failed:\\tno file\\nC:\\\\x");
    // RFC 3339 with milliseconds and the local offset.
    let time = chrono::DateTime::parse_from_rfc3339(fields[0]).unwrap();
    assert_eq!(fields[0].len(), "2026-11-14T10:20:30.456+03:00".len());
    assert!(fields[0].as_bytes()[19] == b'.');
    let age = chrono::Local::now().signed_duration_since(time);
    assert!(age.num_seconds().abs() < 60, "{age}");
    let row = parse_line(&text).unwrap();
    assert_eq!(row.message, "open failed:\tno file\nC:\\x");
}

#[test]
fn the_category_comes_from_the_target_or_from_the_module() {
    let j = in_memory();
    emit(&j, log::Level::Info, "tool", "named");
    emit(&j, log::Level::Info, "mika_core::agent", "by the table");
    emit(
        &j,
        log::Level::Info,
        "mika_core::config",
        "ours, not listed",
    );
    emit(&j, log::Level::Warn, "reqwest::connect", "somebody else's");
    j.write(Level::Info, "voice", "written directly");
    j.write(Level::Info, "no such category", "written directly");
    let categories: Vec<String> = j
        .tail(None)
        .rows
        .into_iter()
        .map(|row| row.category)
        .collect();
    assert_eq!(categories, ["tool", "turn", "app", "lib", "voice", "app"]);
}

#[test]
fn other_crates_are_written_from_warn_only() {
    let j = in_memory();
    j.apply(&LogConfig {
        level: Level::Debug,
        ..LogConfig::default()
    });
    for level in [
        log::Level::Trace,
        log::Level::Debug,
        log::Level::Info,
        log::Level::Warn,
        log::Level::Error,
    ] {
        emit(&j, level, "hyper::client", "theirs");
        emit(&j, level, "mika_core::agent", "ours");
    }
    let rows = j.tail(None).rows;
    let of = |category: &str| -> Vec<Level> {
        rows.iter()
            .filter(|row| row.category == category)
            .map(|row| row.level)
            .collect()
    };
    assert_eq!(of("lib"), [Level::Warn, Level::Error]);
    // Ours: everything but trace, which the journal does not have.
    assert_eq!(
        of("turn"),
        [Level::Debug, Level::Info, Level::Warn, Level::Error]
    );
}

#[test]
fn a_level_change_applies_at_once() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    let meta = |level| {
        log::Metadata::builder()
            .level(level)
            .target("mika_core::agent")
            .build()
    };
    assert_eq!(j.level(), Level::Info);
    assert!(j.enabled(&meta(log::Level::Info)) && !j.enabled(&meta(log::Level::Debug)));
    emit(&j, log::Level::Debug, "app", "debug while info");
    emit(&j, log::Level::Info, "app", "info while info");

    j.apply(&LogConfig {
        level: Level::Debug,
        ..LogConfig::default()
    });
    assert!(j.enabled(&meta(log::Level::Debug)) && !j.enabled(&meta(log::Level::Trace)));
    emit(&j, log::Level::Debug, "app", "debug while debug");

    j.apply(&LogConfig {
        level: Level::Error,
        ..LogConfig::default()
    });
    assert_eq!(j.config().level, Level::Error);
    assert!(!j.enabled(&meta(log::Level::Warn)));
    emit(&j, log::Level::Warn, "app", "warn while error");
    emit(&j, log::Level::Error, "app", "error while error");
    j.write(Level::Info, "app", "direct info while error");

    j.flush();
    let messages: Vec<String> = current(d.path())
        .lines()
        .map(|line| parse_line(line).unwrap().message)
        .collect();
    assert_eq!(
        messages,
        ["info while info", "debug while debug", "error while error"]
    );
}

#[test]
fn no_secret_reaches_the_bytes_of_the_file() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    j.apply(&LogConfig {
        level: Level::Debug,
        content: true,
        ..LogConfig::default()
    });
    let key = Secret::new("zq-file-secret-4f7a9c1e");
    let texts = [
        format!("request failed: 401 for key {}", key.expose()),
        format!("several\nlines\twith {} inside", key.expose()),
        "GET https://api.example/v1/models?api_key=plain-pattern-key-123&page=2".to_string(),
        "Authorization: Bearer pattern.bearer-token-456".to_string(),
        "poll https://api.telegram.org/bot987654321:AAF-pattern_telegram_token_0123456789ab/getUpdates".to_string(),
        "the provider said sk-pattern-key-7890abcdef is not valid".to_string(),
        "proxy http://user:pattern-password@10.0.0.1:3128 refused".to_string(),
        r#"body {"token":"pattern-json-token","model":"m"}"#.to_string(),
        format!("{}{}", "~".repeat(MAX_MESSAGE_CHARS - 10), key.expose()),
    ];
    let levels = [
        log::Level::Error,
        log::Level::Warn,
        log::Level::Info,
        log::Level::Debug,
    ];
    for (n, text) in texts.iter().enumerate() {
        emit(&j, levels[n % levels.len()], "mika_providers::openai", text);
        j.write(Level::Info, "llm", text);
    }
    j.flush();

    let leaks = [
        "zq-file-secret",
        "4f7a9c1e",
        "plain-pattern-key",
        "pattern.bearer",
        "AAF-pattern",
        "sk-pattern",
        "pattern-password",
        "pattern-json-token",
    ];
    let bytes = std::fs::read(d.path().join("mika.log")).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(text.lines().count(), texts.len() * 2);
    for leak in leaks {
        assert!(!text.contains(leak), "{leak} is in the file");
    }
    assert!(text.contains("401 for key ***"));
    assert!(text.contains("api_key=***&page=2"));

    // Nor is it in memory, in a page or in the export.
    let dest = d.path().join("export.txt");
    j.export(None, &[format!("key {}", key.expose())], &dest)
        .unwrap();
    let exported = std::fs::read_to_string(&dest).unwrap();
    let shown: Vec<String> = j
        .tail(None)
        .rows
        .into_iter()
        .chain(all_pages(&j, limit(7)))
        .map(|row| row.message)
        .chain([exported])
        .collect();
    for leak in leaks {
        assert!(!shown.iter().any(|text| text.contains(leak)), "{leak}");
    }
}

#[test]
fn a_secret_learned_later_does_not_leave_the_file() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    info(&j, "typed by hand: zq-learned-later-77aa55");
    j.flush();
    assert!(current(d.path()).contains("zq-learned-later-77aa55"));
    let _secret = Secret::new("zq-learned-later-77aa55");
    let page = j.read(&Query::default());
    assert_eq!(page.rows[0].message, "typed by hand: ***");
    let dest = d.path().join("export.txt");
    assert_eq!(j.export(None, &[], &dest).unwrap(), 1);
    assert!(!std::fs::read_to_string(&dest)
        .unwrap()
        .contains("zq-learned-later"));
}

#[test]
fn a_record_is_never_longer_than_the_limit() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    // Fillers that no test of the crate remembers as a secret: the sink
    // scrubs with the set all the tests share.
    info(&j, &"ѣ".repeat(MAX_MESSAGE_CHARS * 3));
    info(&j, &"~\n".repeat(MAX_MESSAGE_CHARS));
    info(&j, &"~".repeat(MAX_MESSAGE_CHARS));
    j.flush();
    let rows = j.tail(None).rows;
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert!(row.message.chars().count() <= MAX_MESSAGE_CHARS);
    }
    assert!(rows[0]
        .message
        .ends_with(&format!(" <cut {} chars>", MAX_MESSAGE_CHARS * 2 + 32)));
    assert!(rows[1].message.contains(" <cut "));
    assert_eq!(rows[2].message, "~".repeat(MAX_MESSAGE_CHARS));
    // The lines of the file hold the same, escaped.
    let from_file: Vec<Row> = current(d.path())
        .lines()
        .map(|line| parse_line(line).unwrap())
        .collect();
    for (file, memory) in from_file.iter().zip(&rows) {
        assert_eq!(file.message, memory.message);
    }
}

#[test]
fn the_file_rotates_at_its_size_and_only_so_many_are_kept() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 2000, 3);
    fill(&j, 0, 300);
    j.flush();
    assert_eq!(
        names(d.path()),
        ["mika.1.log", "mika.2.log", "mika.3.log", "mika.log"]
    );
    for name in names(d.path()) {
        let size = std::fs::metadata(d.path().join(&name)).unwrap().len();
        assert!(size > 0 && size <= 2000, "{name}: {size}");
    }
    // The files hold the last records without a gap, the newest in `mika.log`.
    let rows = all_pages(&j, limit(50));
    let numbers: Vec<usize> = rows.iter().map(number).collect();
    let oldest = *numbers.last().unwrap();
    assert!(oldest > 0, "some files must have been removed");
    assert_eq!(numbers, (oldest..300).rev().collect::<Vec<_>>());
    assert!(current(d.path()).contains("record 0299"));
    let first = std::fs::read_to_string(d.path().join("mika.3.log")).unwrap();
    assert!(first.starts_with(&rows.last().unwrap().t));
}

#[test]
fn files_past_their_age_are_removed() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 2000, 10);
    fill(&j, 0, 150);
    close(j);
    let before = names(d.path());
    assert!(before.len() >= 4, "{before:?}");
    age(&d.path().join("mika.2.log"), 3 * DAY);
    age(&d.path().join("mika.3.log"), 40 * DAY);

    // At the start: fourteen days by default.
    let j = open_small(d.path(), 2000, 10);
    assert!(!d.path().join("mika.3.log").exists());
    assert!(d.path().join("mika.2.log").exists());
    // A shorter time applies at once.
    j.apply(&LogConfig {
        keep_days: 2,
        keep_files: 10,
        ..LogConfig::default()
    });
    assert!(!d.path().join("mika.2.log").exists());
    assert!(d.path().join("mika.1.log").exists());
    // And so does a smaller count.
    assert!(names(d.path()).len() > 2);
    j.apply(&LogConfig {
        keep_days: 2,
        keep_files: 1,
        ..LogConfig::default()
    });
    assert_eq!(names(d.path()), ["mika.1.log", "mika.log"]);
}

#[test]
fn a_current_file_nobody_wrote_to_for_too_long_is_not_kept() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    info(&j, "before the long break");
    close(j);
    age(&d.path().join("mika.log"), 15 * DAY);
    let j = open(d.path());
    assert_eq!(current(d.path()), "");
    assert!(j.read(&Query::default()).rows.is_empty());
    // Thirteen days are fine.
    info(&j, "after the break");
    close(j);
    age(&d.path().join("mika.log"), 13 * DAY);
    let j = open(d.path());
    assert_eq!(j.read(&Query::default()).rows.len(), 1);
}

#[test]
fn the_file_of_the_old_logger_is_removed() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("mika.old.log"), "[old] lines\n").unwrap();
    std::fs::write(d.path().join("keep.txt"), "not ours").unwrap();
    let j = open(d.path());
    info(&j, "first");
    j.flush();
    assert_eq!(names(d.path()), ["keep.txt", "mika.log"]);
}

#[test]
fn broken_lines_are_stepped_over() {
    let d = tempfile::tempdir().unwrap();
    let mut file = Vec::new();
    file.extend_from_slice(b"2026-11-14T10:00:00.000+03:00\tINFO\tapp\tfirst whole\n");
    file.extend_from_slice(b"garbage without fields\n");
    file.extend_from_slice(b"[2026-11-14T07:00:01Z INFO  mika_gui] the old format\n");
    file.extend_from_slice(b"\xff\xfe not text\tINFO\tapp\tx\n");
    file.extend_from_slice(b"2026-11-14T10:00:02.000+03:00\tINFO\tapp\n");
    file.extend_from_slice(b"\n");
    file.extend_from_slice(b"not a time\tINFO\tapp\tx\n");
    file.extend_from_slice(
        b"2026-11-14T10:00:03.000+03:00\tERROR\ttool\tsecond whole\\twith a tab\n",
    );
    std::fs::write(d.path().join("mika.log"), file).unwrap();

    let j = open(d.path());
    let page = j.read(&Query::default());
    assert_eq!(page.next, None);
    assert_eq!(
        page.rows,
        [
            Row {
                seq: None,
                t: "2026-11-14T10:00:03.000+03:00".into(),
                level: Level::Error,
                category: "tool".into(),
                message: "second whole\twith a tab".into(),
            },
            Row {
                seq: None,
                t: "2026-11-14T10:00:00.000+03:00".into(),
                level: Level::Info,
                category: "app".into(),
                message: "first whole".into(),
            },
        ]
    );
    assert_eq!(
        j.summary().oldest.as_deref(),
        Some("2026-11-14T10:00:00.000+03:00")
    );
    let dest = d.path().join("export.txt");
    assert_eq!(j.export(None, &[], &dest).unwrap(), 2);
    let exported = std::fs::read_to_string(&dest).unwrap();
    assert!(!exported.contains("garbage") && !exported.contains("old format"));
}

#[test]
fn a_half_written_last_line_is_not_a_record() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    info(&j, "record 0001");
    info(&j, "record 0002");
    j.flush();
    // A line that is still being written, as a reader may catch it.
    let mut raw = std::fs::OpenOptions::new()
        .append(true)
        .open(d.path().join("mika.log"))
        .unwrap();
    raw.write_all(b"2026-11-14T10:00:03.000+03:00\tINFO\tapp\thalf wri")
        .unwrap();
    drop(raw);

    let page = j.read(&Query::default());
    assert_eq!(page.rows.iter().map(number).collect::<Vec<_>>(), [2, 1]);
    assert_eq!(page.next, None);
    let dest = d.path().join("export.txt");
    assert_eq!(j.export(None, &[], &dest).unwrap(), 2);
    assert_eq!(j.summary().files, 1);

    // After a crash in the middle of a write the next run starts a new line.
    close(j);
    let j = open(d.path());
    info(&j, "after the restart");
    let messages: Vec<String> = all_pages(&j, limit(10))
        .into_iter()
        .map(|row| row.message)
        .collect();
    assert_eq!(
        messages,
        [
            "after the restart",
            "half wri",
            "record 0002",
            "record 0001"
        ]
    );
}

#[test]
fn pages_across_files_give_every_record_once_newest_first() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 1500, 50);
    fill(&j, 0, 137);
    assert!(names(d.path()).len() >= 5, "{:?}", names(d.path()));

    let mut query = limit(10);
    let mut numbers = Vec::new();
    let mut pages = 0;
    loop {
        let page = j.read(&query);
        pages += 1;
        // Records of this run carry their numbers.
        for row in &page.rows {
            assert_eq!(row.seq, Some(number(row) as u64 + 1));
        }
        numbers.extend(page.rows.iter().map(number));
        match page.next {
            Some(next) => {
                assert_eq!(page.rows.len(), 10);
                query.before = Some(next);
            }
            None => break,
        }
    }
    assert_eq!(numbers, (0..137).rev().collect::<Vec<_>>());
    assert_eq!(pages, 14);

    // A page that ends exactly at the oldest record knows it is the last.
    let page = j.read(&limit(137));
    assert_eq!((page.rows.len(), page.next), (137, None));
    // The limit has a ceiling and a default.
    assert_eq!(j.read(&limit(0)).rows.len(), DEFAULT_PAGE);
    fill(&j, 137, MAX_PAGE);
    let page = j.read(&limit(100_000));
    assert_eq!(page.rows.len(), MAX_PAGE);
    assert!(page.next.is_some());
}

#[test]
fn a_mark_survives_rotations_and_new_records() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 1500, 50);
    fill(&j, 0, 60);
    let first = j.read(&limit(10));
    assert_eq!(
        first.rows.iter().map(number).collect::<Vec<_>>(),
        (50..60).rev().collect::<Vec<_>>()
    );
    let files_before = names(d.path()).len();

    // Much is written and several files are rotated before the next page.
    fill(&j, 60, 150);
    assert!(names(d.path()).len() > files_before + 2);
    let rest = all_pages(
        &j,
        Query {
            before: first.next.clone(),
            ..limit(7)
        },
    );
    assert_eq!(
        rest.iter().map(number).collect::<Vec<_>>(),
        (0..50).rev().collect::<Vec<_>>()
    );

    // A mark nobody gave out gives nothing, and no next page.
    for mark in ["nonsense", "f999999.0", "m5", ""] {
        let page = j.read(&Query {
            before: Some(mark.to_string()),
            ..limit(10)
        });
        assert_eq!(page, Page::default(), "{mark:?}");
    }
}

#[test]
fn a_mark_into_a_file_that_was_removed_ends_the_reading() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 1500, 3);
    fill(&j, 0, 40);
    let first = j.read(&limit(5));
    assert!(first.next.is_some());
    // Enough to push every file of the first page out.
    fill(&j, 40, 400);
    let rest = all_pages(
        &j,
        Query {
            before: first.next,
            ..limit(5)
        },
    );
    assert!(rest.is_empty(), "{:?}", rest.first());
}

#[test]
fn records_of_an_earlier_run_have_no_number() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 1500, 50);
    fill(&j, 0, 30);
    close(j);
    let j = open_small(d.path(), 1500, 50);
    fill(&j, 30, 5);
    let rows = all_pages(&j, limit(8));
    assert_eq!(
        rows.iter().map(number).collect::<Vec<_>>(),
        (0..35).rev().collect::<Vec<_>>()
    );
    let seqs: Vec<Option<u64>> = rows.iter().map(|row| row.seq).collect();
    assert_eq!(seqs[..5], [Some(5), Some(4), Some(3), Some(2), Some(1)]);
    assert!(seqs[5..].iter().all(Option::is_none));
}

#[test]
fn filters_by_level_category_text_and_time() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 700, 50);
    j.apply(&LogConfig {
        level: Level::Debug,
        keep_files: 50,
        ..LogConfig::default()
    });
    let records = [
        (Level::Info, "app", "started 0.4.0"),
        (Level::Error, "tool", "files_read failed: Disk not ready"),
        (Level::Warn, "llm", "retry 2 after 429"),
        (Level::Debug, "tool", "shell_run took 15 ms"),
        (Level::Info, "tool", "files_list ok, 12 entries on DISK D"),
        (Level::Error, "llm", "Ошибка провайдера: тайм-аут"),
        (Level::Info, "voice", "synthesis 120 chars"),
        (Level::Warn, "tool", "screen_read: nothing in front"),
    ];
    for round in 0..3 {
        for (level, category, message) in records {
            j.write(level, category, &format!("{message} #{round}"));
        }
    }
    assert!(names(d.path()).len() > 2);
    let read = |query: Query| -> Vec<String> {
        all_pages(&j, Query { limit: 4, ..query })
            .into_iter()
            .map(|row| row.message)
            .collect()
    };
    let rounds = |messages: &[&str]| -> Vec<String> {
        (0..3)
            .rev()
            .flat_map(|round| messages.iter().map(move |m| format!("{m} #{round}")))
            .collect()
    };

    assert_eq!(read(Query::default()).len(), 24);
    assert_eq!(
        read(Query {
            levels: vec![Level::Error],
            ..Query::default()
        }),
        rounds(&[
            "Ошибка провайдера: тайм-аут",
            "files_read failed: Disk not ready"
        ])
    );
    assert_eq!(
        read(Query {
            levels: Level::at_least(Level::Warn),
            categories: vec!["tool".into()],
            ..Query::default()
        }),
        rounds(&[
            "screen_read: nothing in front",
            "files_read failed: Disk not ready"
        ])
    );
    assert_eq!(
        read(Query {
            categories: vec!["voice".into(), "app".into()],
            ..Query::default()
        }),
        rounds(&["synthesis 120 chars", "started 0.4.0"])
    );
    // The text is looked for without regard to letter case, in any script.
    assert_eq!(
        read(Query {
            text: " disk ".into(),
            ..Query::default()
        }),
        rounds(&[
            "files_list ok, 12 entries on DISK D",
            "files_read failed: Disk not ready"
        ])
    );
    assert_eq!(
        read(Query {
            text: "ошибка ПРОВАЙДЕРА".into(),
            ..Query::default()
        }),
        rounds(&["Ошибка провайдера: тайм-аут"])
    );
    assert_eq!(
        read(Query {
            text: "disk".into(),
            levels: vec![Level::Info],
            categories: vec!["tool".into()],
            ..Query::default()
        }),
        rounds(&["files_list ok, 12 entries on DISK D"])
    );
    assert!(read(Query {
        text: "no such words".into(),
        ..Query::default()
    })
    .is_empty());
    assert!(read(Query {
        categories: vec!["mcp".into()],
        ..Query::default()
    })
    .is_empty());

    // By time: everything is newer than an hour ago, nothing is from tomorrow.
    let now = chrono::Local::now().fixed_offset();
    assert_eq!(
        read(Query {
            since: Some(now - chrono::Duration::hours(1)),
            ..Query::default()
        })
        .len(),
        24
    );
    assert!(read(Query {
        since: Some(now + chrono::Duration::days(1)),
        ..Query::default()
    })
    .is_empty());
}

#[test]
fn reading_goes_on_while_another_thread_writes() {
    const TOTAL: usize = 3000;
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 20_000, 50);
    let done = Arc::new(AtomicBool::new(false));

    let writer = {
        let j = j.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            for n in 0..TOTAL {
                // Every tenth record is written out at once, the others wait
                // in the buffer.
                let level = if n % 10 == 0 {
                    log::Level::Warn
                } else {
                    log::Level::Info
                };
                emit(
                    &j,
                    level,
                    "tool",
                    &format!("record {n:04} {}", "~".repeat(n % 40)),
                );
            }
            done.store(true, Ordering::SeqCst);
        })
    };
    let reader = {
        let j = j.clone();
        let done = done.clone();
        std::thread::spawn(move || {
            let mut sessions = 0;
            let mut seen = 0;
            while !done.load(Ordering::SeqCst) || sessions == 0 {
                // One walk through all the pages: every record at most once,
                // newer before older, nothing torn.
                let numbers: Vec<usize> = all_pages(&j, limit(64)).iter().map(number).collect();
                assert!(
                    numbers.windows(2).all(|pair| pair[0] == pair[1] + 1),
                    "a gap or a repeat in {numbers:?}"
                );
                if let Some(oldest) = numbers.last() {
                    assert_eq!(*oldest, 0);
                }
                seen = seen.max(numbers.len());
                let tail = j.tail(None);
                assert!(tail.rows.len() <= RING_RECORDS);
                let _ = j.summary();
                sessions += 1;
            }
            (sessions, seen)
        })
    };
    in_time(move || writer.join().unwrap());
    let (sessions, seen) = in_time(move || reader.join().unwrap());
    assert!(sessions > 0 && seen <= TOTAL);

    let numbers: Vec<usize> = all_pages(&j, limit(500)).iter().map(number).collect();
    assert_eq!(numbers, (0..TOTAL).rev().collect::<Vec<_>>());
    // A rotation that a reader put off happens with the next record.
    info(&j, "one more");
    assert!(names(d.path()).len() >= 2, "{:?}", names(d.path()));
}

#[test]
fn many_threads_write_whole_lines() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 30_000, 50);
    let threads: Vec<_> = (0..8)
        .map(|thread| {
            let j = j.clone();
            std::thread::spawn(move || {
                for n in 0..250 {
                    info(
                        &j,
                        &format!("thread {thread} record {n} {}", "~".repeat(n % 50)),
                    );
                }
            })
        })
        .collect();
    for thread in threads {
        in_time(move || thread.join().unwrap());
    }
    let rows = all_pages(&j, limit(500));
    assert_eq!(rows.len(), 2000);
    // Numbers follow the order of the lines in the files.
    let seqs: Vec<u64> = rows.iter().filter_map(|row| row.seq).collect();
    assert_eq!(seqs, (1..=2000).rev().collect::<Vec<u64>>());
    for thread in 0..8 {
        let of_thread = rows
            .iter()
            .filter(|row| row.message.starts_with(&format!("thread {thread} ")))
            .count();
        assert_eq!(of_thread, 250);
    }
}

#[test]
fn the_tail_gives_what_came_after_a_number() {
    let j = in_memory();
    assert_eq!(j.tail(None), Tail::default());
    fill(&j, 0, 5);
    let all = j.tail(None);
    assert_eq!(all.last, 5);
    assert_eq!(
        all.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        [Some(1), Some(2), Some(3), Some(4), Some(5)]
    );
    assert_eq!(
        all.rows.iter().map(number).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4]
    );
    let newer = j.tail(Some(3));
    assert_eq!(newer.rows.iter().map(number).collect::<Vec<_>>(), [3, 4]);
    assert_eq!(newer.last, 5);
    assert_eq!(j.tail(Some(5)).rows, []);
    assert_eq!(j.tail(Some(500)).rows, []);
    assert_eq!(j.tail(Some(0)).rows.len(), 5);
}

#[test]
fn memory_holds_the_last_two_thousand_records() {
    let j = in_memory();
    fill(&j, 0, RING_RECORDS + 50);
    let tail = j.tail(None);
    assert_eq!(tail.rows.len(), RING_RECORDS);
    assert_eq!(tail.last, RING_RECORDS as u64 + 50);
    assert_eq!(tail.rows[0].seq, Some(51));
    assert_eq!(number(&tail.rows[0]), 50);
    assert_eq!(j.tail(Some(tail.last - 2)).rows.len(), 2);
}

#[test]
fn a_journal_without_a_folder_reads_from_memory() {
    let j = in_memory();
    assert_eq!(j.dir(), None);
    fill(&j, 0, 25);
    j.write(Level::Error, "tool", "the one error");
    let numbers: Vec<usize> = all_pages(
        &j,
        Query {
            categories: vec!["app".into()],
            ..limit(10)
        },
    )
    .iter()
    .map(number)
    .collect();
    assert_eq!(numbers, (0..25).rev().collect::<Vec<_>>());
    let errors = j.read(&Query {
        levels: vec![Level::Error],
        ..Query::default()
    });
    assert_eq!(errors.rows.len(), 1);
    assert_eq!(errors.rows[0].message, "the one error");
    assert_eq!(errors.next, None);
    assert_eq!(j.summary(), Summary::default());
    assert!(!j.begin_run());
    j.end_run();

    let d = tempfile::tempdir().unwrap();
    let dest = d.path().join("export.txt");
    assert_eq!(j.export(None, &["memory only".into()], &dest).unwrap(), 26);
    let exported = std::fs::read_to_string(&dest).unwrap();
    assert!(exported.starts_with("# memory only\n"));
    assert!(exported.ends_with("\ttool\tthe one error\n"));
}

#[test]
fn the_content_helper_shows_text_only_when_allowed() {
    assert_eq!(content_if(false, "привет, мир"), "<hidden 11 chars>");
    assert_eq!(content_if(false, ""), "<hidden 0 chars>");
    assert_eq!(content_if(true, "привет, мир"), "привет, мир");
    let long = "д".repeat(CONTENT_CHARS + 100);
    let shown = content_if(true, &long);
    assert_eq!(shown, format!("{} <+100 chars>", "д".repeat(CONTENT_CHARS)));
    assert_eq!(
        content_if(true, &"д".repeat(CONTENT_CHARS)).chars().count(),
        CONTENT_CHARS
    );

    // The journal of the app is not installed in tests: nothing is allowed.
    assert!(!content_allowed());
    assert_eq!(content("what the user said"), "<hidden 18 chars>");
    assert_eq!(
        address("https://host/find?q=secret+plans#top"),
        "https://host/find?<hidden>"
    );

    // A journal answers by its own switch, and the switch applies at once.
    let j = in_memory();
    assert!(!j.content_allowed());
    assert_eq!(j.content("typed text"), "<hidden 10 chars>");
    j.apply(&LogConfig {
        content: true,
        ..LogConfig::default()
    });
    assert!(j.content_allowed());
    assert_eq!(j.content("typed text"), "typed text");
    j.apply(&LogConfig::default());
    assert_eq!(j.content("typed text"), "<hidden 10 chars>");
}

#[test]
fn addresses_and_errors_are_shortened() {
    assert_eq!(address_if(false, "https://host/path"), "https://host/path");
    assert_eq!(
        address_if(false, "https://host/path#part"),
        "https://host/path?<hidden>"
    );
    assert_eq!(
        address_if(true, "https://host/find?q=words"),
        "https://host/find?q=words"
    );
    let error = std::io::Error::other("е".repeat(ERROR_CHARS + 7));
    assert_eq!(
        error_text(&error),
        format!("{} <+7 chars>", "е".repeat(ERROR_CHARS))
    );
    assert_eq!(error_text(&"short"), "short");
}

#[test]
fn a_secret_across_the_cut_of_a_helper_does_not_survive_in_part() {
    // The helpers cut long texts. A secret that starts before the cut and
    // ends after it must not leave its beginning: the value net matches
    // whole values only, and the shapes need their whole length.
    let known = "zq-helper-secret-value-0123456789";
    let _ = Secret::new(known.to_string());
    let token = format!("ghp_{}", "Zq9".repeat(12));
    for (max, cut) in [(ERROR_CHARS, "error"), (CONTENT_CHARS, "content")] {
        for secret in [known, token.as_str()] {
            let text = format!("{} {secret} and more ~~~~~~~~", "~".repeat(max - 10));
            let shown = match cut {
                "error" => error_text(&text),
                _ => content_if(true, &text),
            };
            assert!(shown.contains("***"), "{cut}: {shown}");
            for part in [&secret[..8], &secret[..12]] {
                assert!(!shown.contains(part), "{cut}: {part} in {shown}");
            }
            // The same through the helpers that are built on them.
            let guarded = match cut {
                "error" => guarded_error_if(true, &text),
                _ => guarded_if(true, &text),
            };
            assert_eq!(guarded, shown);
            assert_eq!(complaint_if(true, &text), error_text(&text));
        }
    }
    // And the size says nothing of what was hidden but its length.
    assert_eq!(
        content_if(false, known),
        format!("<hidden {} chars>", known.len())
    );
}

#[test]
fn a_warning_is_on_the_disk_at_once_and_the_rest_soon() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    info(&j, "waits in the buffer");
    emit(&j, log::Level::Warn, "app", "written at once");
    // No flush: the warning took the buffered record with it.
    let text = current(d.path());
    assert!(text.contains("waits in the buffer") && text.contains("written at once"));

    info(&j, "written a moment later");
    let started = Instant::now();
    while !current(d.path()).contains("written a moment later") {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the buffer was not written out"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // And whatever is left is written when the journal goes away.
    info(&j, "written at the end");
    close(j);
    assert!(current(d.path()).contains("written at the end"));
}

#[test]
fn the_log_trait_flush_writes_the_buffer() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    info(&j, "buffered");
    Log::flush(&*j);
    assert!(current(d.path()).contains("buffered"));
}

/// Prints itself and logs while doing so.
struct Talkative(Arc<Journal>);

impl fmt::Display for Talkative {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        emit(
            &self.0,
            log::Level::Error,
            "app",
            "said while being printed",
        );
        self.0.flush();
        let _ = self.0.tail(None);
        f.write_str("the outer record")
    }
}

/// Fails to print itself.
struct Broken;

impl fmt::Display for Broken {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        Err(fmt::Error)
    }
}

#[test]
fn a_log_call_from_inside_the_sink_neither_hangs_nor_recurses() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    let rows = {
        let j = j.clone();
        in_time(move || {
            let talkative = Talkative(j.clone());
            j.log(
                &log::Record::builder()
                    .level(log::Level::Warn)
                    .target("app")
                    .args(format_args!("{talkative}"))
                    .build(),
            );
            // The thread is free again afterwards.
            info(&j, "after it");
            j.tail(None).rows
        })
    };
    let messages: Vec<&str> = rows.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["the outer record", "after it"]);
}

#[test]
fn the_thread_that_holds_the_sink_cannot_hang_on_it() {
    let j = in_memory();
    info(&j, "before");
    let rows = {
        let j = j.clone();
        in_time(move || {
            let held = j.lock();
            // A record from here is dropped: waiting for the lock would be
            // waiting for oneself.
            emit(&j, log::Level::Error, "app", "from under the lock");
            j.write(Level::Error, "app", "written from under the lock");
            // A panic at this moment waits a little and gives up.
            let started = Instant::now();
            j.record_panic("a panic under the lock");
            let waited = started.elapsed();
            drop(held);
            assert!(
                waited >= Duration::from_millis(500) && waited < Duration::from_secs(15),
                "{waited:?}"
            );
            info(&j, "after");
            j.tail(None).rows
        })
    };
    let messages: Vec<&str> = rows.iter().map(|row| row.message.as_str()).collect();
    assert_eq!(messages, ["before", "after"]);
}

/// Panics while being printed.
struct Exploding;

impl fmt::Display for Exploding {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        panic!("zq-panic-inside the sink");
    }
}

#[test]
fn a_value_that_fails_to_print_does_not_panic_the_sink() {
    let j = in_memory();
    j.log(
        &log::Record::builder()
            .level(log::Level::Error)
            .target("app")
            .args(format_args!("before {} after", Broken))
            .build(),
    );
    info(&j, "still working");
    let rows = j.tail(None).rows;
    assert_eq!(rows.len(), 2);
    assert!(rows[0].message.starts_with("before "));
}

#[test]
fn a_folder_that_cannot_be_used_is_not_an_error() {
    let d = tempfile::tempdir().unwrap();
    // A file stands where the folder should be.
    let blocked = d.path().join("logs");
    std::fs::write(&blocked, "in the way").unwrap();
    let j = Journal::open(Options::in_dir(&blocked, LogConfig::default()));
    fill(&j, 0, 20);
    emit(&j, log::Level::Error, "app", "an error with nowhere to go");
    j.flush();
    assert_eq!(j.tail(None).rows.len(), 21);
    assert_eq!(j.summary().files, 0);
    // What could not be written is still read: the page, the problem report
    // and the tool of the model must show it exactly when the disk fails.
    let rows = all_pages(&j, limit(8));
    assert_eq!(rows.len(), 21);
    assert_eq!(rows[0].message, "an error with nowhere to go");
    assert_eq!(
        rows[1..].iter().map(number).collect::<Vec<_>>(),
        (0..20).rev().collect::<Vec<_>>()
    );
    assert_eq!(
        rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        (1..=21).rev().map(Some).collect::<Vec<_>>()
    );
    let errors = j.read(&Query {
        levels: vec![Level::Error],
        ..Query::default()
    });
    assert_eq!(errors.rows.len(), 1);
    let dest = d.path().join("export.txt");
    assert_eq!(j.export(None, &[], &dest).unwrap(), 21);
    assert!(!j.begin_run());
    j.end_run();
    assert!(j.clear().is_ok());
    assert_eq!(std::fs::read_to_string(&blocked).unwrap(), "in the way");
}

#[test]
fn a_file_removed_under_the_journal_is_made_again() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 1500, 5);
    fill(&j, 0, 10);
    j.flush();
    // Somebody removes the folder's files while the app runs.
    for name in names(d.path()) {
        let _ = std::fs::remove_file(d.path().join(name));
    }
    fill(&j, 10, 60);
    j.flush();
    assert_eq!(j.tail(None).rows.len(), 70);
    // Whatever the file system did, nothing panicked and reading works.
    let rows = all_pages(&j, limit(50));
    let numbers: Vec<usize> = rows.iter().map(number).collect();
    assert!(
        numbers.windows(2).all(|pair| pair[0] > pair[1]),
        "{numbers:?}"
    );
}

#[test]
fn records_go_on_reaching_the_disk_after_the_file_was_removed() {
    let d = tempfile::tempdir().unwrap();
    // The size of the app: no rotation comes to the rescue.
    let j = open(d.path());
    fill(&j, 0, 10);
    j.flush();
    std::fs::remove_file(d.path().join("mika.log")).unwrap();
    fill(&j, 10, 30);
    emit(&j, log::Level::Warn, "app", "record 0040 a warning");
    fill(&j, 41, 9);
    j.flush();
    // A new file was started, and it holds everything written since.
    let text = current(d.path());
    assert_eq!(text.lines().count(), 40, "{text}");
    assert!(text.starts_with("20") && text.contains("record 0010"));
    assert!(!text.contains("record 0009"));
    // Every record is read once, the newest first, each with its number:
    // those of the lost file come from memory.
    let rows = all_pages(&j, limit(15));
    assert_eq!(
        rows.iter().map(number).collect::<Vec<_>>(),
        (0..50).rev().collect::<Vec<_>>()
    );
    assert_eq!(
        rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        (1..=50).rev().map(Some).collect::<Vec<_>>()
    );
    assert_eq!(j.summary().files, 1);
    close(j);
    // The next run reads what the disk has.
    let j = open(d.path());
    assert_eq!(all_pages(&j, limit(500)).len(), 40);
}

#[test]
fn records_in_memory_only_keep_their_place_among_those_of_the_files() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    fill(&j, 0, 5);
    j.flush();
    // The file cannot be written for a while: what is recorded meanwhile
    // stays in memory.
    j.lock().close_for_test();
    fill(&j, 5, 5);
    j.flush();
    assert_eq!(current(d.path()).lines().count(), 5);
    // And it can be written again.
    j.lock().retry_for_test();
    fill(&j, 10, 5);
    j.flush();
    assert_eq!(current(d.path()).lines().count(), 10);
    for size in [1, 4, 7, 100] {
        let rows = all_pages(&j, limit(size));
        assert_eq!(
            rows.iter().map(number).collect::<Vec<_>>(),
            (0..15).rev().collect::<Vec<_>>(),
            "pages of {size}"
        );
    }
    // A filter applies to them like to the rest.
    let found = j.read(&Query {
        text: "record 0007".into(),
        ..Query::default()
    });
    assert_eq!(found.rows.len(), 1);
    assert_eq!(found.next, None);
}

#[test]
fn the_newest_page_has_no_row_the_tail_would_bring_again() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    fill(&j, 0, 50);
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (j, stop) = (j.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 50;
            while !stop.load(Ordering::Relaxed) {
                emit(&j, log::Level::Warn, "app", &format!("record {n:04}"));
                n += 1;
            }
        })
    };
    for _ in 0..200 {
        let page = j.read(&limit(20));
        // Every row of the newest page is a record of this run, so each
        // has its number: the page that follows the tail can tell it from
        // what the tail brings.
        assert!(
            page.rows.iter().all(|row| row.seq.is_some()),
            "{:?}",
            page.rows.iter().map(|row| row.seq).collect::<Vec<_>>()
        );
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
}

#[test]
fn the_current_file_of_the_old_logger_goes_at_the_first_start() {
    let d = tempfile::tempdir().unwrap();
    let mut old = String::new();
    for n in 0..6000 {
        old.push_str(&format!(
            "[2026-11-10T12:00:00Z INFO  mika_gui] wake phrase heard: zq-said-aloud {n}\n"
        ));
    }
    assert!(old.len() > 256 * 1024);
    std::fs::write(d.path().join("mika.log"), &old).unwrap();
    let j = open(d.path());
    info(&j, "the first record of the journal");
    j.flush();
    let text = current(d.path());
    assert!(!text.contains("zq-said-aloud"));
    assert_eq!(text.lines().count(), 1);
    // The page tells since when there are records.
    let summary = j.summary();
    assert_eq!(summary.files, 1);
    assert_eq!(
        summary.oldest,
        Some(j.read(&Query::default()).rows[0].t.clone())
    );
}

#[test]
fn clearing_removes_the_files_and_the_memory_but_not_the_numbers() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 1500, 50);
    assert!(!j.begin_run());
    fill(&j, 0, 80);
    let stale = j.read(&limit(5)).next;
    assert!(stale.is_some());
    assert!(names(d.path()).len() > 3);
    let last = j.tail(None).last;
    assert_eq!(last, 80);

    j.clear().unwrap();
    assert_eq!(names(d.path()), ["mika.log", "mika.running"]);
    let page = j.read(&Query::default());
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].message, "journal cleared");
    assert_eq!(page.rows[0].seq, Some(81));
    let tail = j.tail(Some(last));
    assert_eq!((tail.rows.len(), tail.last), (1, 81));
    assert_eq!(j.tail(None).rows.len(), 1);
    assert_eq!(j.summary().files, 1);

    // A mark from before the clearing leads nowhere, now and after new
    // records and rotations.
    let old = |j: &Journal| {
        j.read(&Query {
            before: stale.clone(),
            ..limit(5)
        })
    };
    assert_eq!(old(&j), Page::default());
    fill(&j, 100, 120);
    assert_eq!(old(&j), Page::default());
    let numbers: Vec<usize> = all_pages(&j, limit(50))
        .iter()
        .filter(|row| row.message != "journal cleared")
        .map(number)
        .collect();
    assert_eq!(numbers, (100..220).rev().collect::<Vec<_>>());
}

#[test]
fn the_summary_counts_files_and_bytes_and_knows_the_oldest_record() {
    let d = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 1500, 50);
    assert_eq!(
        j.summary(),
        Summary {
            size_bytes: 0,
            files: 1,
            oldest: None
        }
    );
    fill(&j, 0, 90);
    let summary = j.summary();
    let on_disk: u64 = names(d.path())
        .iter()
        .map(|name| std::fs::metadata(d.path().join(name)).unwrap().len())
        .sum();
    assert_eq!(summary.size_bytes, on_disk);
    assert_eq!(summary.files as usize, names(d.path()).len());
    let oldest = all_pages(&j, limit(500)).pop().unwrap();
    assert_eq!(number(&oldest), 0);
    assert_eq!(summary.oldest, Some(oldest.t));
}

#[test]
fn the_export_is_one_file_with_a_header_oldest_first() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let j = open_small(d.path(), 1500, 50);
    fill(&j, 0, 60);
    j.write(Level::Error, "tool", "the one error\nwith two lines");
    let dest = out.path().join("mika-journal.txt");
    let header = vec![
        "Mika 0.4.0".to_string(),
        "Windows 11\nlevel info, content off".to_string(),
        "key=zq-header-secret".to_string(),
    ];
    assert_eq!(j.export(None, &header, &dest).unwrap(), 61);
    let text = std::fs::read_to_string(&dest).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..4],
        [
            "# Mika 0.4.0",
            "# Windows 11",
            "# level info, content off",
            "# key=***"
        ]
    );
    let rows: Vec<Row> = lines[4..]
        .iter()
        .map(|line| parse_line(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 61);
    assert_eq!(
        rows[..60].iter().map(number).collect::<Vec<_>>(),
        (0..60).collect::<Vec<_>>()
    );
    assert_eq!(rows[60].message, "the one error\nwith two lines");

    // With a query: only what it wants; the mark and the limit do not count.
    let query = Query {
        levels: vec![Level::Error],
        before: Some("f0.0".into()),
        limit: 1,
        ..Query::default()
    };
    assert_eq!(j.export(Some(&query), &[], &dest).unwrap(), 1);
    let text = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.ends_with("\tERROR\ttool\tthe one error\\nwith two lines\n"));

    // Not over the journal itself, and an unusable place is an error. The
    // letter case of the name and the spelling of the folder do not count:
    // for Windows it is the same file.
    let spelled = d.path().join("..").join(d.path().file_name().unwrap());
    for dest in [
        d.path().join("mika.log"),
        d.path().join("mika.2.log"),
        d.path().join("mika.running"),
        d.path().join("Mika.log"),
        d.path().join("MIKA.1.LOG"),
        spelled.join("mika.log"),
        spelled.join("MIKA.LOG"),
    ] {
        assert!(
            matches!(j.export(None, &[], &dest), Err(JournalError::Io(_))),
            "{}",
            dest.display()
        );
    }
    // Another name in the folder of the journal is fine.
    assert_eq!(
        j.export(None, &[], &d.path().join("saved.txt")).unwrap(),
        61
    );
    std::fs::remove_file(d.path().join("saved.txt")).unwrap();
    assert!(j
        .export(None, &[], &out.path().join("no such folder").join("x.txt"))
        .is_err());
    assert_eq!(all_pages(&j, limit(500)).len(), 61);
}

#[test]
fn a_panic_leaves_a_record_on_the_disk() {
    let d = tempfile::tempdir().unwrap();
    let j = open(d.path());
    info(&j, "before the panic");

    let previous = Arc::new(std::panic::take_hook());
    let chained = previous.clone();
    std::panic::set_hook(panic_hook(j.clone(), Box::new(move |info| chained(info))));
    let number = 42;
    let caught = std::panic::catch_unwind(|| {
        panic!("zq-panic-marker {number} with token=zq-panic-secret-value");
    });
    let literal = std::panic::catch_unwind(|| {
        panic!("zq-panic-literal");
    });
    // A panic in the middle of a `log` call: the hook writes its record
    // through the very sink the thread is inside of.
    let inside = {
        let j = j.clone();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            j.log(
                &log::Record::builder()
                    .level(log::Level::Info)
                    .target("app")
                    .args(format_args!("never written: {Exploding}"))
                    .build(),
            );
        }))
    };
    std::panic::set_hook(Box::new(move |info| previous(info)));
    assert!(caught.is_err() && literal.is_err() && inside.is_err());
    // The thread is not left marked as being inside the sink.
    emit(&j, log::Level::Warn, "app", "after the panics");

    // No flush here: the hook itself put the record on the disk.
    let text = current(d.path());
    let rows: Vec<Row> = text
        .lines()
        .map(|line| parse_line(line).unwrap())
        .filter(|row| row.message.contains("zq-panic-"))
        .collect();
    assert_eq!(rows.len(), 3, "{text}");
    assert!(rows[2].message.ends_with(": zq-panic-inside the sink"));
    assert!(text.contains("after the panics") && !text.contains("never written"));
    for row in &rows {
        assert_eq!((row.level, row.category.as_str()), (Level::Error, "app"));
        assert!(
            row.message.starts_with("panic in thread '"),
            "{}",
            row.message
        );
        assert!(row.message.contains("tests.rs:"), "{}", row.message);
    }
    assert!(rows[0]
        .message
        .ends_with(": zq-panic-marker 42 with token=***"));
    assert!(rows[1].message.ends_with(": zq-panic-literal"));
    assert!(!text.contains("zq-panic-secret-value"));
    assert!(text.contains("before the panic"));
}

#[test]
fn the_marker_of_a_run_tells_an_unclean_exit() {
    let d = tempfile::tempdir().unwrap();
    let marker = d.path().join("mika.running");

    // The first run: nothing before it.
    let j = open(d.path());
    assert!(!j.begin_run());
    assert!(!j.previous_run_crashed());
    // Asked again, it does not take its own marker for a crash.
    assert!(!j.begin_run());
    let written = std::fs::read_to_string(&marker).unwrap();
    assert!(
        written.starts_with(&format!("pid {} started 20", std::process::id())),
        "{written}"
    );
    info(&j, "working");
    // It ends without `end_run`: a crash.
    close(j);
    assert!(marker.exists());

    // The second run finds the marker.
    let j = open(d.path());
    assert!(j.begin_run());
    assert!(j.previous_run_crashed());
    assert!(j.begin_run());
    let rows = j.read(&Query::default()).rows;
    assert_eq!(
        rows.iter().filter(|row| row.level == Level::Warn).count(),
        1
    );
    assert_eq!(rows[0].level, Level::Warn);
    assert_eq!(rows[0].category, "app");
    assert!(
        rows[0]
            .message
            .starts_with("previous run did not exit cleanly (pid "),
        "{}",
        rows[0].message
    );
    assert!(marker.exists());
    // The notice is shown once.
    j.acknowledge_crash();
    assert!(!j.previous_run_crashed());
    // Clearing the journal is not an exit.
    j.clear().unwrap();
    assert!(marker.exists());
    // A clean exit.
    info(&j, "buffered before the exit");
    j.end_run();
    assert!(!marker.exists());
    assert!(current(d.path()).contains("buffered before the exit"));
    close(j);

    // The third run: the one before it exited cleanly.
    let j = open(d.path());
    assert!(!j.begin_run());
    assert!(!j.previous_run_crashed());
    j.end_run();
    close(j);
    // A marker nobody can read still counts.
    std::fs::write(&marker, b"\xff\xfe").unwrap();
    let j = open(d.path());
    assert!(j.begin_run());
    assert!(j.previous_run_crashed());
}

#[test]
fn nothing_that_names_the_blind_zone_is_written_in_any_mode() {
    const LIKE: &str = "<hidden: reads like the blind zone, ";
    // Texts off: the size only, whatever the text is.
    assert_eq!(
        guarded_if(false, "Вход в банк — Chrome"),
        "<hidden 20 chars>"
    );
    assert_eq!(guarded_if(false, "notepad.exe"), "<hidden 11 chars>");
    // Texts on: an ordinary text is written, a blind-zone one never.
    assert_eq!(
        guarded_if(true, "{\"path\":\"notes.txt\"}"),
        "{\"path\":\"notes.txt\"}"
    );
    for text in [
        "{\"window\":\"Вход в банк — Chrome\"}",
        "{\"url\":\"https://accounts.google.com/v3/signin\"}",
        "KeePass",
        "{\"text\":\"мой пароль 123\"}",
    ] {
        let written = guarded_if(true, text);
        assert_eq!(
            written,
            format!("{LIKE}{} chars>", text.chars().count()),
            "{text}"
        );
    }
    // The refusal of the blind zone itself has a mark of its own, in both
    // modes: it says that such a window was there.
    let refusal = crate::policy::blind::BLIND_REFUSAL;
    for allowed in [false, true] {
        assert_eq!(guarded_error_if(allowed, &refusal), BLIND_MARK);
        assert_eq!(
            guarded_error_if(allowed, &format!("Ошибка: {refusal}.")),
            BLIND_MARK
        );
    }
    assert!(is_blind_refusal(&refusal));
    assert!(!is_blind_refusal(&"файл bank_statement.mp4 не найден"));
    // A text that only has a word of the blind zone in it is not that
    // refusal: it is hidden, and the record does not claim a window.
    assert_eq!(
        guarded_error_if(true, &"окно «Sign in to GitHub» не отвечает"),
        format!("{LIKE}36 chars>")
    );
    assert_eq!(
        guarded_error_if(true, &"файл bank_statement.mp4 не найден"),
        format!("{LIKE}33 chars>")
    );
}

#[test]
fn the_error_of_a_tool_is_written_only_while_texts_are() {
    // What tools say when they fail quotes what is not the app's: titles
    // of open windows, what a window shows, paths, arguments.
    let windows =
        "нет окна «отчёт». Открытые окна: «Письмо Ане — Почта» (mail), «Зарплата.xlsx» (excel)";
    let shown = guarded_error_if(false, &windows);
    assert_eq!(shown, format!("<hidden {} chars>", windows.chars().count()));
    for part in ["Письмо", "Зарплата", "отчёт"] {
        assert!(!shown.contains(part));
    }
    assert_eq!(guarded_error_if(true, &windows), windows);
    assert_eq!(
        guarded_error_if(false, &"нет такого пути: D:/docs/план.txt"),
        "<hidden 33 chars>"
    );
    let long = "ё".repeat(400);
    assert!(guarded_error_if(true, &long).ends_with("<+100 chars>"));
    // The journal of the app is not installed in tests: texts are off.
    assert_eq!(guarded_error(&"файл не найден"), "<hidden 14 chars>");
}

#[test]
fn the_complaint_of_a_program_keeps_its_reason_and_not_its_quotes() {
    // The reason after the last `: ` is the program's own words.
    assert_eq!(
        complaint_if(
            false,
            "D:/videos/Отпуск 2026.mp4: Invalid data found when processing input"
        ),
        "<hidden 25 chars>: Invalid data found when processing input"
    );
    assert_eq!(
        complaint_if(false, "Conversion failed!"),
        "Conversion failed!"
    );
    // A reason that quotes something is not kept.
    for quoting in [
        "Unable to find a suitable output format for 'D:/videos/a.xyz'",
        "Error opening input: D:/videos/a.mp4",
        "[in#0 @ 000001] Error opening input",
        "",
    ] {
        assert_eq!(
            complaint_if(false, quoting),
            format!("<hidden {} chars>", quoting.chars().count()),
            "{quoting}"
        );
    }
    // With texts on the whole line, unless it reads like the blind zone.
    assert_eq!(
        complaint_if(true, "D:/videos/a.mp4: Invalid data found"),
        "D:/videos/a.mp4: Invalid data found"
    );
    assert!(
        complaint_if(true, "D:/bank_statement.mp4: Invalid data found")
            .starts_with("<hidden: reads like the blind zone")
    );
    assert_eq!(
        complaint_if(false, "D:/bank_statement.mp4: Invalid data found"),
        "<hidden 21 chars>: Invalid data found"
    );
}

#[test]
fn the_records_of_the_code_pass_the_scrub_as_they_are() {
    // Names of fields the instrumentation uses: none of them reads like a
    // secret to the scrub, so nothing useful is masked.
    for line in [
        "tool call finished: tool=av_convert risk=medium decision=asked_yes ms=5321 outcome=ok",
        "request finished: kind=chat provider=openai host=api.example.test model=gpt-test ms=1234 messages=2 tools=109 http=200 retries=0 tokens_in=10 tokens_cached=2 tokens_out=30 stop=tool_calls outcome=ok",
        "a key was written to the credential store: entry=llm.openai.api_key",
        "a key cannot be read from the credential store: entry=telegram.bot_token error=secret store unavailable: no access",
        "turn finished: outcome=ok ms=1500 steps=3 tools=2 reply_chars=12",
        "hotkey registered: id=dictation combo=Ctrl+Alt+D",
        "settings saved: sections=hotkeys,privacy,telegram",
        "copy saved: bytes=4096 encrypted=true keys_inside=3 categories=settings:1,history:120",
        "deep search finished: outcome=ok ms=90000 searches=4 pages=9 sources=6 unread=3 unsourced=0 llm_calls=11 tokens=42000 refused=1 limits_hit=none report_saved=true",
        "incoming: kind=voice seconds=7 bytes=20480 taken=true",
    ] {
        assert_eq!(scrub(line), line);
    }
}
