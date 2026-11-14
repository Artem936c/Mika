//! One record on one line: time, level, category and message, separated by
//! tabs. The message is escaped, so a line is read back without guessing.

use std::borrow::Cow;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

/// Level of a record. `Error` is the most severe and the smallest.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Error, Level::Warn, Level::Info, Level::Debug];

    /// The name written to the file: `ERROR`, `WARN`, `INFO`, `DEBUG`.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }

    /// The name in settings and in the interface: `error`, `warn`, `info`, `debug`.
    pub fn name(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }

    /// Reads either spelling.
    pub fn parse(text: &str) -> Option<Level> {
        Level::ALL
            .into_iter()
            .find(|l| l.as_str() == text || l.name() == text)
    }

    /// `min` and every level more severe than it, the most severe first.
    pub fn at_least(min: Level) -> Vec<Level> {
        Level::ALL.into_iter().filter(|l| *l <= min).collect()
    }

    /// `None` for `Trace`: the journal has no such level.
    pub fn from_log(level: log::Level) -> Option<Level> {
        match level {
            log::Level::Error => Some(Level::Error),
            log::Level::Warn => Some(Level::Warn),
            log::Level::Info => Some(Level::Info),
            log::Level::Debug => Some(Level::Debug),
            log::Level::Trace => None,
        }
    }

    pub fn filter(self) -> log::LevelFilter {
        match self {
            Level::Error => log::LevelFilter::Error,
            Level::Warn => log::LevelFilter::Warn,
            Level::Info => log::LevelFilter::Info,
            Level::Debug => log::LevelFilter::Debug,
        }
    }

    pub(crate) fn from_u8(value: u8) -> Level {
        match value {
            0 => Level::Error,
            1 => Level::Warn,
            2 => Level::Info,
            _ => Level::Debug,
        }
    }
}

/// A record as the interface gets it. `seq` is the number of a record written
/// by this run and still held in memory, `None` for an older one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub seq: Option<u64>,
    /// RFC 3339 with milliseconds and the local offset, as written.
    pub t: String,
    pub level: Level,
    pub category: String,
    pub message: String,
}

/// Escapes what would break a line: backslash, tab, line feed, carriage
/// return and the other control characters.
pub fn escape(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c == '\\' || c.is_control()) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                out.push_str("\\u{");
                out.push_str(&format!("{:x}", c as u32));
                out.push('}');
            }
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// The reverse of `escape`. An escape it does not know stays as written.
pub fn unescape(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let mut chars = rest[at + 1..].chars();
        let known = match chars.next() {
            Some('\\') => Some('\\'),
            Some('t') => Some('\t'),
            Some('n') => Some('\n'),
            Some('r') => Some('\r'),
            Some('u') => match unicode_escape(chars.as_str()) {
                Some((c, used)) => {
                    out.push(c);
                    rest = &chars.as_str()[used..];
                    continue;
                }
                None => {
                    out.push_str("\\u");
                    None
                }
            },
            Some(other) => {
                out.push('\\');
                out.push(other);
                None
            }
            None => {
                out.push('\\');
                None
            }
        };
        if let Some(c) = known {
            out.push(c);
        }
        rest = chars.as_str();
    }
    out.push_str(rest);
    out
}

/// Reads `{hex}` after `\u`: the character and how many bytes it took.
fn unicode_escape(text: &str) -> Option<(char, usize)> {
    let body = text.strip_prefix('{')?;
    let end = body.find('}')?;
    if end == 0 || end > 6 {
        return None;
    }
    let code = u32::from_str_radix(&body[..end], 16).ok()?;
    Some((char::from_u32(code)?, end + 2))
}

/// The line of a record, with the line feed at the end.
pub fn format_line(t: &str, level: Level, category: &str, message: &str) -> String {
    let message = escape(message);
    let mut line = String::with_capacity(t.len() + category.len() + message.len() + 10);
    line.push_str(t);
    line.push('\t');
    line.push_str(level.as_str());
    line.push('\t');
    line.push_str(category);
    line.push('\t');
    line.push_str(&message);
    line.push('\n');
    line
}

/// The four fields of a line, the message still escaped.
pub(crate) struct Fields<'a> {
    pub t: &'a str,
    pub level: Level,
    pub category: &'a str,
    pub message: &'a str,
}

/// Splits a line without its line feed. `None` for a line that is not a
/// record: too few fields, an unknown level, a category that is not a name.
/// The time is checked by `Fields::time`.
pub(crate) fn split_line(line: &str) -> Option<Fields<'_>> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut parts = line.splitn(4, '\t');
    let t = parts.next()?;
    let level = parts.next()?;
    let category = parts.next()?;
    let message = parts.next()?;
    let level = Level::ALL.into_iter().find(|l| l.as_str() == level)?;
    let named = !category.is_empty()
        && category.len() <= 24
        && category
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if !named {
        return None;
    }
    Some(Fields {
        t,
        level,
        category,
        message,
    })
}

impl Fields<'_> {
    pub(crate) fn time(&self) -> Option<DateTime<FixedOffset>> {
        DateTime::parse_from_rfc3339(self.t).ok()
    }

    pub(crate) fn row(&self) -> Row {
        Row {
            seq: None,
            t: self.t.to_string(),
            level: self.level,
            category: self.category.to_string(),
            message: unescape(self.message),
        }
    }
}

/// Reads a line back. `None` for a line that is not a whole record.
pub fn parse_line(line: &str) -> Option<Row> {
    let fields = split_line(line.strip_suffix('\n').unwrap_or(line))?;
    fields.time()?;
    Some(fields.row())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_round_trips_every_awkward_text() {
        let texts = [
            "",
            "plain",
            "tab\there",
            "two\nlines\r\nand more",
            "back\\slash",
            "\\",
            "\\\\",
            "ends with backslash\\",
            "\\t is not a tab, \\n is not a line feed",
            "\\u{41} is five characters",
            "bell\u{7}, zero\u{0}, escape\u{1b}, del\u{7f}, c1\u{85}",
            "Привет, мир\tи\nснова\\",
            "emoji \u{1f600} stays",
            "\\u{zz} \\u{} \\u{1234567} \\u",
            "\\q unknown",
        ];
        for text in texts {
            let escaped = escape(text);
            assert!(
                !escaped.chars().any(|c| c.is_control()),
                "{text:?} -> {escaped:?}"
            );
            assert_eq!(unescape(&escaped), text, "{text:?} -> {escaped:?}");
        }
    }

    #[test]
    fn plain_text_is_not_copied_and_escapes_look_as_documented() {
        assert!(matches!(escape("nothing to do"), Cow::Borrowed(_)));
        assert_eq!(escape("a\tb\nc\\d\re"), "a\\tb\\nc\\\\d\\re");
        assert_eq!(escape("\u{1}"), "\\u{1}");
    }

    #[test]
    fn unknown_escapes_stay_as_written() {
        assert_eq!(unescape("a\\qb"), "a\\qb");
        assert_eq!(unescape("tail\\"), "tail\\");
        assert_eq!(unescape("\\u{zz}"), "\\u{zz}");
        assert_eq!(unescape("\\u{41}\\u{0}"), "A\u{0}");
        assert_eq!(unescape("\\\\t"), "\\t");
    }

    #[test]
    fn a_line_round_trips() {
        let line = format_line(
            "2026-11-14T10:20:30.456+03:00",
            Level::Warn,
            "tool",
            "failed:\tno such file\nC:\\x",
        );
        assert_eq!(
            line,
            "2026-11-14T10:20:30.456+03:00\tWARN\ttool\tfailed:\\tno such file\\nC:\\\\x\n"
        );
        assert_eq!(line.matches('\n').count(), 1);
        assert_eq!(line.matches('\t').count(), 3);
        let row = parse_line(&line).unwrap();
        assert_eq!(
            row,
            Row {
                seq: None,
                t: "2026-11-14T10:20:30.456+03:00".into(),
                level: Level::Warn,
                category: "tool".into(),
                message: "failed:\tno such file\nC:\\x".into(),
            }
        );
        // A file saved by an editor with CRLF reads the same.
        let crlf = line.replace('\n', "\r\n");
        assert_eq!(parse_line(&crlf), Some(row));
    }

    #[test]
    fn a_broken_line_is_not_a_record() {
        let broken = [
            "",
            "garbage",
            "[2026-11-14T10:00:00Z INFO  mika_gui] the old format",
            "\t\t\t",
            "2026-11-14T10:20:30.456+03:00\tINFO\tapp",
            "2026-11-14T10:20:30.456+03:00\tinfo\tapp\tlower-case level",
            "2026-11-14T10:20:30.456+03:00\tTRACE\tapp\tno such level",
            "2026-11-14T10:20:30.456+03:00\tINFO\t\tno category",
            "2026-11-14T10:20:30.456+03:00\tINFO\tApp Name\tnot a name",
            "yesterday\tINFO\tapp\tnot a time",
            "2026-11-14 10:20:30\tINFO\tapp\tnot RFC 3339",
        ];
        for line in broken {
            assert_eq!(parse_line(line), None, "{line:?}");
        }
        // An empty message is still a record.
        assert!(parse_line("2026-11-14T10:20:30.456+03:00\tINFO\tapp\t").is_some());
    }

    #[test]
    fn levels_have_two_spellings_and_an_order() {
        for level in Level::ALL {
            assert_eq!(Level::parse(level.as_str()), Some(level));
            assert_eq!(Level::parse(level.name()), Some(level));
            assert_eq!(Level::from_u8(level as u8), level);
        }
        assert_eq!(Level::parse("trace"), None);
        assert_eq!(Level::default(), Level::Info);
        assert_eq!(Level::at_least(Level::Warn), [Level::Error, Level::Warn]);
        assert_eq!(Level::at_least(Level::Debug), Level::ALL);
        assert_eq!(Level::from_log(log::Level::Trace), None);
        assert_eq!(Level::from_log(log::Level::Debug), Some(Level::Debug));
        assert_eq!(serde_json::to_string(&Level::Warn).unwrap(), "\"warn\"");
        assert_eq!(
            serde_json::from_str::<Level>("\"debug\"").unwrap(),
            Level::Debug
        );
    }
}
