//! The report file. Everything here besides the body is written by code from
//! its own data: the sources, what was not read, how the report was made.

use std::path::{Path, PathBuf};

use mika_core::config::ResearchLimits;

use super::page::one_line;
use super::sources::Skipped;
use super::write::Body;
use super::Source;

/// Characters of the question in the heading and on the chat card.
pub const TITLE_CHARS: usize = 200;
const SLUG_WORDS: usize = 8;
const SLUG_CHARS: usize = 60;

/// A limit that cut the work short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitHit {
    Time,
    Tokens,
}

impl LimitHit {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitHit::Time => "time",
            LimitHit::Tokens => "tokens",
        }
    }
}

/// What one run used.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    /// Search queries that were sent.
    pub queries: Vec<String>,
    pub searches: u32,
    /// Pages whose text went to the model.
    pub pages: u32,
    pub llm_calls: u32,
    /// Estimate.
    pub tokens: u32,
    pub seconds: u64,
    pub limits_hit: Vec<LimitHit>,
    /// Addresses the network check refused.
    pub refused: usize,
}

/// Backslash before `\ [ ] < > * _` and the backtick: the text stays text in
/// Markdown.
pub fn md_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '[' | ']' | '<' | '>' | '*' | '_' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// An address for `<…>`: nothing in it can end the brackets or the line.
fn in_brackets(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        match c {
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            c if c.is_whitespace() || c.is_control() => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn incomplete_line(hits: &[LimitHit]) -> Option<String> {
    let time = hits.contains(&LimitHit::Time);
    let tokens = hits.contains(&LimitHit::Tokens);
    let what = match (time, tokens) {
        (true, true) => "кончились время и лимит токенов",
        (true, false) => "кончилось время",
        (false, true) => "кончился лимит токенов",
        (false, false) => return None,
    };
    Some(format!(
        "*Отчёт неполный: {what} раньше, чем всё прочитано.*"
    ))
}

/// The whole report file. `when` is `%d.%m.%Y %H:%M`; `model` is the model id
/// and may be empty.
#[allow(clippy::too_many_arguments)]
pub fn render(
    question: &str,
    when: &str,
    model: &str,
    body: &Body,
    sources: &[Source],
    skipped: &[Skipped],
    stats: &Stats,
    limits: ResearchLimits,
) -> String {
    let mut out = format!("# {}\n\n", md_escape(&one_line(question, TITLE_CHARS)));
    let model = one_line(model, 200);
    let by = if model.is_empty() {
        "моделью".to_string()
    } else {
        format!("моделью {}", md_escape(&model))
    };
    out.push_str(&format!(
        "*Глубокий поиск Мики, {when}. Собрано {by} по страницам из интернета: источников {}. Важное проверяй по ссылкам.*\n\n",
        sources.len()
    ));
    if let Some(line) = incomplete_line(&stats.limits_hit) {
        out.push_str(&line);
        out.push_str("\n\n");
    }
    out.push_str(body.markdown.trim());
    out.push_str("\n\n## Источники\n\n");
    for s in sources {
        out.push_str(&format!(
            "{}. {} — <{}>\n",
            s.n,
            md_escape(&s.shown_title()),
            in_brackets(&s.url)
        ));
    }
    if !skipped.is_empty() || stats.refused > 0 {
        out.push_str("\n## Не прочитано или не пригодилось\n\n");
        for s in skipped {
            out.push_str(&format!(
                "- <{}> — {}\n",
                in_brackets(&s.url),
                md_escape(&one_line(&s.reason, 200))
            ));
        }
        if stats.refused > 0 {
            if !skipped.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!(
                "Ещё адресов отклонено сетевой проверкой: {}.\n",
                stats.refused
            ));
        }
    }
    let queries: Vec<String> = stats
        .queries
        .iter()
        .map(|q| format!("«{}»", md_escape(&one_line(q, 150))))
        .collect();
    out.push_str("\n## Как собрано\n\n");
    out.push_str(&format!("- Запросы: {}.\n", queries.join("; ")));
    out.push_str(&format!(
        "- Поисков: {} из {}. Страниц прочитано: {} из {}. Обращений к модели: {} из {}.\n",
        stats.searches,
        limits.searches,
        stats.pages,
        limits.pages,
        stats.llm_calls,
        limits.llm_calls()
    ));
    out.push_str(&format!(
        "- Токенов, оценка: {} из {}. Время: {} мин {} с из {} мин.\n",
        stats.tokens,
        limits.tokens,
        stats.seconds / 60,
        stats.seconds % 60,
        limits.minutes
    ));
    out
}

/// `2026-11-14 12-30 <first words of the question>.md`. Of the question only
/// letters, digits and spaces survive.
pub fn file_name(question: &str, stamp: &str) -> String {
    let clean: String = question
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = clean.split_whitespace().take(SLUG_WORDS).collect();
    let slug: String = words.join(" ").chars().take(SLUG_CHARS).collect();
    let slug = slug.trim_end();
    let slug = if slug.is_empty() { "поиск" } else { slug };
    format!("{stamp} {slug}.md")
}

/// Names tried for one report before giving up.
const NAME_TRIES: u32 = 1000;
const PROBE_NAME: &str = ".mika-probe";

/// The name with ` (n)` before its extension; `n == 1` is the name itself.
fn numbered(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// Makes the folder for reports and checks that a file can be written there.
/// Runs before any paid work: an unplugged disk or a read-only place must not
/// cost a whole search.
pub fn ready_folder(folder: &Path) -> Result<(), String> {
    let problem = |e: std::io::Error| format!("папка отчётов недоступна: {e}");
    std::fs::create_dir_all(folder).map_err(problem)?;
    let probe = folder.join(format!("{PROBE_NAME}-{}", std::process::id()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map(drop)
        .or_else(|e| match e.kind() {
            // A probe left by a run that was killed: the folder took it then.
            std::io::ErrorKind::AlreadyExists => Ok(()),
            _ => Err(problem(e)),
        })?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Writes the report as a new file and returns its path. A taken name gets
/// ` (2)`, ` (3)` and so on. The file is opened as a new one, so nothing that
/// already has the name is ever written over, whatever appeared there since
/// the name was picked.
///
/// Synchronous on purpose: there is no await between the last Stop check and
/// the finished file, so Stop never leaves a part of a report.
pub fn save(folder: &Path, name: &str, text: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    let problem = |e: std::io::Error| format!("не удалось записать отчёт: {e}");
    std::fs::create_dir_all(folder).map_err(problem)?;
    for n in 1..=NAME_TRIES {
        let path = folder.join(numbered(name, n));
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match opened {
            Ok(mut file) => {
                if let Err(e) = file.write_all(text.as_bytes()).and_then(|()| file.flush()) {
                    drop(file);
                    // A part of a report is worse than none.
                    let _ = std::fs::remove_file(&path);
                    return Err(problem(e));
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            // Windows names a folder in the way as a refusal, not as a taken name.
            Err(_) if path.symlink_metadata().is_ok() => {}
            Err(e) => return Err(problem(e)),
        }
    }
    Err("не удалось записать отчёт: в папке слишком много отчётов с таким именем".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(n: usize, title: &str, url: &str) -> Source {
        Source {
            n,
            url: url.into(),
            title: title.into(),
            facts: vec!["Факт.".into()],
        }
    }

    fn limits() -> ResearchLimits {
        ResearchLimits {
            searches: 5,
            pages: 8,
            minutes: 5,
            tokens: 50_000,
        }
    }

    fn stats() -> Stats {
        Stats {
            queries: vec!["тепловой насос цена".into(), "heat pump cost".into()],
            searches: 2,
            pages: 3,
            llm_calls: 5,
            tokens: 21_400,
            seconds: 143,
            limits_hit: Vec::new(),
            refused: 0,
        }
    }

    fn body() -> Body {
        Body {
            markdown:
                "## Коротко\n\nДешевле воздушный [1].\n\n## Подробно\n\nГрунтовый экономнее [2]."
                    .into(),
            unsourced: 0,
        }
    }

    #[test]
    fn markdown_signs_are_escaped() {
        assert_eq!(
            md_escape(r"a\b [c] <d> *e* _f_ `g`"),
            r"a\\b \[c\] \<d\> \*e\* \_f\_ \`g\`"
        );
        assert_eq!(
            md_escape("Обычный заголовок — 2026"),
            "Обычный заголовок — 2026"
        );
    }

    #[test]
    fn report_is_laid_out_in_order() {
        let sources = [
            source(1, "Насосы: обзор", "https://a.example.org/x?y=1"),
            source(2, "", "https://b.example.org/page"),
        ];
        let text = render(
            "Чем насос\n«воздух-вода» лучше?",
            "14.11.2026 12:30",
            "mistralai/mistral-nemo",
            &body(),
            &sources,
            &[],
            &stats(),
            limits(),
        );
        assert_eq!(
            text,
            "# Чем насос «воздух-вода» лучше?\n\n\
*Глубокий поиск Мики, 14.11.2026 12:30. Собрано моделью mistralai/mistral-nemo по страницам из интернета: источников 2. Важное проверяй по ссылкам.*\n\n\
## Коротко\n\nДешевле воздушный [1].\n\n## Подробно\n\nГрунтовый экономнее [2].\n\n\
## Источники\n\n\
1. Насосы: обзор — <https://a.example.org/x?y=1>\n\
2. b.example.org — <https://b.example.org/page>\n\n\
## Как собрано\n\n\
- Запросы: «тепловой насос цена»; «heat pump cost».\n\
- Поисков: 2 из 5. Страниц прочитано: 3 из 8. Обращений к модели: 5 из 10.\n\
- Токенов, оценка: 21400 из 50000. Время: 2 мин 23 с из 5 мин.\n"
        );
        assert!(!text.contains('\r'));
    }

    #[test]
    fn sources_come_from_the_data_of_the_code() {
        let sources = [source(
            1,
            "Заголовок](x) [y]: <script> *жирный* `код`",
            "https://a.example.org/a b<c>d\ne",
        )];
        let text = render("в", "д", "", &body(), &sources, &[], &stats(), limits());
        assert!(text.contains(
            "1. Заголовок\\](x) \\[y\\]: \\<script\\> \\*жирный\\* \\`код\\` — <https://a.example.org/a%20b%3Cc%3Ed%0Ae>\n"
        ), "{text}");
        // Without a model id the header names none.
        assert!(text.contains("Собрано моделью по страницам из интернета: источников 1."));
        // The heading is escaped too.
        let odd = render(
            "<b>*вопрос*</b>",
            "д",
            "m",
            &body(),
            &sources,
            &[],
            &stats(),
            limits(),
        );
        assert!(
            odd.starts_with("# \\<b\\>\\*вопрос\\*\\</b\\>\n\n"),
            "{odd}"
        );
    }

    #[test]
    fn unread_pages_and_refused_addresses_show_only_when_there_are_some() {
        let sources = [source(1, "А", "https://a.example.org/")];
        let skipped = [
            Skipped {
                url: "https://b.example.org/login".into(),
                reason: "страница входа, банка или оплаты".into(),
            },
            Skipped {
                url: "https://c.example.org/".into(),
                reason: "сайт ответил 404".into(),
            },
        ];
        let none = render("в", "д", "m", &body(), &sources, &[], &stats(), limits());
        assert!(!none.contains("Не прочитано") && !none.contains("отклонено"));

        let listed = render(
            "в",
            "д",
            "m",
            &body(),
            &sources,
            &skipped,
            &stats(),
            limits(),
        );
        assert!(listed.contains(
            "\n## Не прочитано или не пригодилось\n\n- <https://b.example.org/login> — страница входа, банка или оплаты\n- <https://c.example.org/> — сайт ответил 404\n\n## Как собрано"
        ), "{listed}");
        assert!(!listed.contains("отклонено"));

        let refused = Stats {
            refused: 3,
            ..stats()
        };
        let both = render(
            "в",
            "д",
            "m",
            &body(),
            &sources,
            &skipped,
            &refused,
            limits(),
        );
        assert!(both.contains(
            "— сайт ответил 404\n\nЕщё адресов отклонено сетевой проверкой: 3.\n\n## Как собрано"
        ), "{both}");
        let only = render("в", "д", "m", &body(), &sources, &[], &refused, limits());
        assert!(only.contains(
            "\n## Не прочитано или не пригодилось\n\nЕщё адресов отклонено сетевой проверкой: 3.\n\n## Как собрано"
        ), "{only}");
    }

    #[test]
    fn incomplete_report_says_which_limit_ended_it() {
        let sources = [source(1, "А", "https://a.example.org/")];
        let with = |hits: Vec<LimitHit>| {
            let s = Stats {
                limits_hit: hits,
                ..stats()
            };
            render("в", "д", "m", &body(), &sources, &[], &s, limits())
        };
        assert!(!with(vec![]).contains("Отчёт неполный"));
        assert!(with(vec![LimitHit::Time]).contains(
            "ссылкам.*\n\n*Отчёт неполный: кончилось время раньше, чем всё прочитано.*\n\n## Коротко"
        ));
        assert!(with(vec![LimitHit::Tokens])
            .contains("*Отчёт неполный: кончился лимит токенов раньше, чем всё прочитано.*"));
        assert!(with(vec![LimitHit::Tokens, LimitHit::Time]).contains(
            "*Отчёт неполный: кончились время и лимит токенов раньше, чем всё прочитано.*"
        ));
        assert_eq!(LimitHit::Time.as_str(), "time");
        assert_eq!(LimitHit::Tokens.as_str(), "tokens");
    }

    #[test]
    fn file_name_keeps_only_letters_digits_and_spaces() {
        assert_eq!(
            file_name(
                "Чем тепловой насос «воздух-вода» отличается от «грунт-вода» для дома 120 м², и что выгоднее",
                "2026-11-14 12-30"
            ),
            "2026-11-14 12-30 Чем тепловой насос воздух вода отличается от грунт.md"
        );
        assert_eq!(
            file_name("..\\..\\Windows\\system32/evil:*?\"<>|.exe", "s"),
            "s Windows system32 evil exe.md"
        );
        assert_eq!(
            file_name("?!… —", "2026-11-14 12-30"),
            "2026-11-14 12-30 поиск.md"
        );
        let long = file_name(&"оченьдлинноеслово ".repeat(20), "s");
        assert!(long.chars().count() <= 2 + SLUG_CHARS + 3, "{long}");
        assert!(!long.contains(" .md"), "{long}");
    }

    #[test]
    fn report_is_a_new_file_and_a_taken_name_counts_up() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Mika").join("Research");
        let first = save(&folder, "отчёт.md", "раз\n").unwrap();
        assert_eq!(first, folder.join("отчёт.md"));
        let second = save(&folder, "отчёт.md", "два\n").unwrap();
        assert_eq!(second, folder.join("отчёт (2).md"));
        assert_eq!(std::fs::read(&first).unwrap(), "раз\n".as_bytes());
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "два\n");
        // A folder that cannot be made is an error with a plain start.
        let blocked = dir.path().join("file");
        std::fs::write(&blocked, b"x").unwrap();
        let err = save(&blocked.join("sub"), "отчёт.md", "три").unwrap_err();
        assert!(err.starts_with("не удалось записать отчёт: "), "{err}");
    }

    #[test]
    fn report_never_replaces_what_has_its_name() {
        let dir = tempfile::tempdir().unwrap();
        // The names are taken by things of every kind: a file, a folder.
        std::fs::write(dir.path().join("отчёт.md"), "чужой файл").unwrap();
        std::fs::create_dir(dir.path().join("отчёт (2).md")).unwrap();
        std::fs::write(dir.path().join("отчёт (3).md"), "ещё один").unwrap();
        let path = save(dir.path(), "отчёт.md", "новый\n").unwrap();
        assert_eq!(path, dir.path().join("отчёт (4).md"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("отчёт.md")).unwrap(),
            "чужой файл"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("отчёт (3).md")).unwrap(),
            "ещё один"
        );
        assert!(dir.path().join("отчёт (2).md").is_dir());
        assert_eq!(numbered("отчёт.md", 1), "отчёт.md");
        assert_eq!(numbered("a.b.md", 12), "a.b (12).md");
        assert_eq!(numbered("без расширения", 2), "без расширения (2)");
        assert_eq!(numbered(".md", 2), ".md (2)");
    }

    #[test]
    fn folder_is_made_and_checked_before_the_work() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Mika").join("Research");
        assert_eq!(ready_folder(&folder), Ok(()));
        assert!(folder.is_dir());
        // The check leaves nothing behind and can be repeated.
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
        assert_eq!(ready_folder(&folder), Ok(()));
        // A file where the folder should be.
        let blocked = dir.path().join("file");
        std::fs::write(&blocked, b"x").unwrap();
        for bad in [blocked.clone(), blocked.join("sub")] {
            let err = ready_folder(&bad).unwrap_err();
            assert!(err.starts_with("папка отчётов недоступна: "), "{err}");
        }
        assert_eq!(std::fs::read(&blocked).unwrap(), b"x");
    }
}
