//! A check of the source for the tests of every crate: no record of the
//! journal writes a text of the user or of the model as it is.
//!
//! It reads the calls of the `log` macros and looks at what they format. A
//! variable with a name that means content (`text`, `title`, `query`, `args`,
//! `path` and the like) must go through `journal::content`, `guarded`,
//! `guarded_error`, `complaint` or `address`, or be turned into a size. A
//! secret is never exposed in a
//! record, and a whole request, answer or settings struct is never dumped.
//!
//! The check knows names, not meanings: content under another name passes
//! it. It is a tripwire for the usual mistake, not a proof.

use std::path::Path;

/// Names of variables that hold what the user or the model wrote, said or
/// looked at, or where the user's files are.
const CONTENT_NAMES: &[&str] = &[
    "text",
    "title",
    "question",
    "query",
    "prompt",
    "reply",
    "heard",
    "phrase",
    "spoken",
    "typed",
    "transcript",
    "selection",
    "clipboard",
    "args",
    "arguments",
    "url",
    "summary",
    "target",
    "fact",
    "facts",
    "shown",
    "body",
    "html",
    "content",
    "path",
    "file_name",
    "password",
    "token",
    "secret",
];

/// Calls that make a content value fit for a record.
const WRAPPERS: &[&str] = &[
    "content(",
    "content_if(",
    "guarded(",
    "guarded_if(",
    "guarded_error(",
    "guarded_error_if(",
    "complaint(",
    "complaint_if(",
    "address(",
    "address_if(",
    "logged_address(",
];

/// Ends of an expression that gives a size or a yes/no, not the value.
const SIZES: &[&str] = &[
    ".len()",
    ".count()",
    ".is_empty()",
    ".is_some()",
    ".is_none()",
];

/// Whole values that are never dumped with `{:?}`.
const DUMP_NAMES: &[&str] = &[
    "config", "cfg", "request", "req", "response", "resp", "headers", "body",
];

const MACROS: &[&str] = &["error", "warn", "info", "debug", "trace", "log"];

/// One call that breaks a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The line of the call, from 1.
    pub line: usize,
    pub why: String,
    /// The call as it is written, on one line.
    pub call: String,
}

/// The part of a source file that goes into the product: everything before
/// its test module.
fn production(source: &str) -> &str {
    match source.find("\n#[cfg(test)]\nmod ") {
        Some(at) => &source[..at],
        None => source,
    }
}

/// The arguments of the macro call that opens at `open` (the byte after the
/// bracket), and where the call ends.
fn arguments(source: &str, open: usize) -> (Vec<String>, usize) {
    let bytes = source.as_bytes();
    let mut args = Vec::new();
    let (mut depth, mut start, mut at) = (1usize, open, open);
    let (mut in_string, mut escaped) = (false, false);
    while at < bytes.len() {
        let b = bytes[at];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else {
            match b {
                b'"' => in_string = true,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                b',' if depth == 1 => {
                    args.push(source[start..at].to_string());
                    start = at + 1;
                }
                _ => {}
            }
        }
        at += 1;
    }
    let last = source[start..at.min(source.len())].trim();
    if !last.is_empty() {
        args.push(last.to_string());
    }
    let args = args
        .into_iter()
        .map(|a| a.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    (args, at)
}

/// An expression without what its string literals hold: a word in quotes is
/// a text of the code, not a variable.
fn without_strings(expression: &str) -> String {
    let mut out = String::with_capacity(expression.len());
    let (mut in_string, mut escaped) = (false, false);
    for c in expression.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
                out.push(c);
            }
        } else {
            in_string = c == '"';
            out.push(c);
        }
    }
    out
}

/// The identifiers of an expression.
fn identifiers(expression: &str) -> Vec<&str> {
    expression
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .collect()
}

/// The names a format string takes from its surroundings: `{name}`,
/// `{name:?}`. With each, whether it is printed with `Debug`.
fn captures(format: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = format;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        // `{{` is a brace of the text.
        if let Some(after) = rest.strip_prefix('{') {
            rest = after;
            continue;
        }
        let Some(close) = rest.find('}') else {
            break;
        };
        let inside = &rest[..close];
        let (name, spec) = inside.split_once(':').unwrap_or((inside, ""));
        let named = !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.chars().next().is_some_and(|c| c.is_ascii_digit());
        if named {
            out.push((name.to_string(), spec.contains('?')));
        }
        rest = &rest[close + 1..];
    }
    out
}

/// Why an argument of a record must not be written as it is, if so.
fn content_in(expression: &str) -> Option<String> {
    let code = without_strings(expression);
    let found = identifiers(&code)
        .into_iter()
        .find(|word| CONTENT_NAMES.contains(word))?
        .to_string();
    let plain = code.trim().trim_start_matches('&');
    let wrapped = WRAPPERS.iter().any(|w| {
        plain.starts_with(w)
            || plain.contains(&format!("::{w}"))
            || plain.contains(&format!(".{w}"))
    });
    let size = SIZES.iter().any(|s| plain.ends_with(s)) || plain.starts_with("chars(");
    (!wrapped && !size)
        .then(|| format!("`{found}` is content: wrap it in journal::content or write its size"))
}

/// The calls of the `log` macros in the production part of `source` that
/// break a rule of the journal.
pub fn check_source(source: &str) -> Vec<Finding> {
    let source = production(source);
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = source[from..].find("log::") {
        let call = from + at;
        from = call + 5;
        let rest = &source[from..];
        let Some(name) = MACROS.iter().find(|m| rest.starts_with(&format!("{m}!("))) else {
            continue;
        };
        // A call inside a comment is an example.
        let line_start = source[..call].rfind('\n').map_or(0, |n| n + 1);
        if source[line_start..call].trim_start().starts_with("//") {
            continue;
        }
        let open = from + name.len() + 2;
        let (mut args, end) = arguments(source, open);
        from = end;
        let line = source[..call].matches('\n').count() + 1;
        let text = args.join(", ");
        let mut report = |why: String| {
            found.push(Finding {
                line,
                why,
                call: format!("log::{name}!({text})"),
            })
        };
        if text.contains(".expose()") {
            report("a secret is exposed in a record".into());
        }
        if args.first().is_some_and(|a| a.starts_with("target:")) {
            args.remove(0);
        }
        if *name == "log" && !args.is_empty() {
            // The level.
            args.remove(0);
        }
        let Some((format, values)) = args.split_first() else {
            continue;
        };
        let dumps = format.contains("{:?}") || format.contains("{:#?}");
        for (name, debug) in captures(format) {
            if CONTENT_NAMES.contains(&name.as_str()) {
                report(format!(
                    "`{{{name}}}` is content: wrap it in journal::content or write its size"
                ));
            }
            if debug && DUMP_NAMES.contains(&name.as_str()) {
                report(format!("`{name}` is dumped whole"));
            }
        }
        for value in values {
            if let Some(why) = content_in(value) {
                report(why);
            }
            let bare = value.trim().trim_start_matches('&');
            if dumps && DUMP_NAMES.contains(&bare) {
                report(format!("`{bare}` is dumped whole"));
            }
        }
    }
    found
}

fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

/// Checks every source file under `src`, the folder of a crate. Each line of
/// the answer is one call that breaks a rule: `file:line: why: call`.
pub fn check_crate(src: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(src, &mut files);
    files.sort();
    assert!(!files.is_empty(), "no source files under {}", src.display());
    let mut out = Vec::new();
    for file in files {
        // This file names the rules it checks.
        if file.ends_with("journal/lint.rs") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&file) else {
            continue;
        };
        let name = file
            .strip_prefix(src)
            .unwrap_or(&file)
            .display()
            .to_string();
        for finding in check_source(&source.replace("\r\n", "\n")) {
            out.push(format!(
                "{name}:{}: {}: {}",
                finding.line, finding.why, finding.call
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whys(source: &str) -> Vec<String> {
        check_source(source).into_iter().map(|f| f.why).collect()
    }

    #[test]
    fn sizes_wrapped_values_and_errors_pass() {
        let source = r#"
fn f() {
    log::info!("turn started: chars={}", text.chars().count());
    log::info!(target: "voice", "heard: {}", mika_core::journal::content(heard));
    log::warn!("rule added: pattern={}", crate::journal::guarded(pattern));
    log::debug!("page read: bytes={} address={}", page.html.len(), logged_address(&page.url));
    log::warn!("request failed: {e}");
    log::info!("empty={}", reply.trim().is_empty());
    log::log!(target: "llm", level, "{line}");
    log::info!("question_chars={}", chars(question));
    log::info!("kind={:?}", cfg.kind);
    log::info!("outcome={}", done.map_or("no text", |()| "phrase"));
    // log::info!("{text}") in a comment is an example.
}
"#;
        assert_eq!(whys(source), Vec::<String>::new());
    }

    #[test]
    fn content_written_as_it_is_is_found() {
        let source = r#"
fn f() {
    log::info!("user said {text}");
    log::info!(
        target: "tool",
        "opening {}",
        url
    );
    log::warn!("cannot remove {}", path.display());
    log::info!("expanded={}", shown != text);
    log::debug!("args: {}", args.to_string());
}
"#;
        let found = check_source(source);
        assert_eq!(found.len(), 5, "{found:#?}");
        assert_eq!(found[0].line, 3);
        assert!(found[0].why.contains("{text}"), "{}", found[0].why);
        assert_eq!(found[1].line, 4);
        assert!(found[1].why.contains("`url`"));
        assert_eq!(
            found[1].call,
            r#"log::info!(target: "tool", "opening {}", url)"#
        );
        assert!(found[2].why.contains("`path`"));
        assert!(found[3].why.contains("`shown`"));
        assert!(found[4].why.contains("`args`"));
    }

    #[test]
    fn an_error_helper_does_not_excuse_content() {
        let source = r#"fn f() { log::warn!("failed: {}", journal::error_text(&reply)); }"#;
        assert_eq!(whys(source).len(), 1);
    }

    #[test]
    fn a_secret_and_a_whole_struct_are_found() {
        let source = r#"
fn f() {
    log::debug!("key {}", key.expose());
    log::debug!("request {:?}", req);
    log::debug!("settings {config:?}");
    log::debug!("answer {:#?}", &resp);
}
"#;
        let found = whys(source);
        assert_eq!(found.len(), 4, "{found:#?}");
        assert!(found[0].contains("secret"));
        assert!(found[1].contains("`req` is dumped"));
        assert!(found[2].contains("`config` is dumped"));
        assert!(found[3].contains("`resp` is dumped"));
    }

    #[test]
    fn the_test_module_of_a_file_is_not_checked() {
        let source =
            "fn f() {}\n\n#[cfg(test)]\nmod tests {\n    fn g() { log::info!(\"{text}\"); }\n}\n";
        assert!(check_source(source).is_empty());
    }

    #[test]
    fn braces_and_strings_do_not_confuse_the_reader() {
        let source = r#"
fn f() {
    log::info!("a \"quoted\" ), {{literal}} text={}", match x { Some(t) => t.len(), None => 0 });
    log::info!("{}", title);
}
"#;
        let found = check_source(source);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].line, 4);
    }

    /// The guard of this crate: see the tests folder of the others.
    #[test]
    fn the_records_of_this_crate_carry_no_content() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let found = check_crate(&src);
        assert!(found.is_empty(), "\n{}", found.join("\n"));
    }
}
