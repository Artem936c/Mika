//! The payload of a backup: one gzip member holding UTF-8 JSON lines.
//!
//! ```text
//! {"t":"begin","schema":7}
//! {"t":"config","toml":"..."}
//! {"t":"secret","key":"llm.openai.api_key","value":"..."}
//! {"t":"table","name":"notes","columns":["id","created_at","title","body"]}
//! {"t":"row","v":[1,"2026-11-10T10:00:00+05:00","Заголовок","Текст"]}
//! {"t":"end","lines":5}
//! ```
//!
//! This is the only place where a secret value is read out of its wrapper:
//! one line written into the stream, which is encrypted when keys are there.

use std::io::{self, BufRead, Read, Write};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Map, Value};

use super::{BackupError, Limits};
use crate::secrets::Secret;

/// One value of a table row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    /// Something a format 1 file cannot hold; fits no column.
    Bad,
}

impl Cell {
    fn to_json(&self) -> Value {
        match self {
            Cell::Null | Cell::Bad => Value::Null,
            Cell::Int(n) => json!(n),
            // A number that is not finite has no JSON form.
            Cell::Real(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            Cell::Text(s) => json!(s),
            Cell::Blob(b) => json!({ "b": BASE64.encode(b) }),
        }
    }

    fn from_json(value: Value) -> Cell {
        match value {
            Value::Null => Cell::Null,
            Value::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) => Cell::Int(i),
                (None, Some(f)) => Cell::Real(f),
                (None, None) => Cell::Bad,
            },
            Value::String(s) => Cell::Text(s),
            Value::Object(map) if map.len() == 1 => map
                .get("b")
                .and_then(Value::as_str)
                .and_then(|s| BASE64.decode(s).ok())
                .map_or(Cell::Bad, Cell::Blob),
            _ => Cell::Bad,
        }
    }
}

/// Writes the lines and counts them for the closing one.
pub(crate) struct PayloadWriter<W: Write> {
    gz: GzEncoder<W>,
    lines: u64,
}

impl<W: Write> PayloadWriter<W> {
    pub(crate) fn new(out: W) -> Self {
        Self {
            gz: GzEncoder::new(out, Compression::default()),
            lines: 0,
        }
    }

    fn line(&mut self, value: Value) -> io::Result<()> {
        if self.lines > 0 {
            self.gz.write_all(b"\n")?;
        }
        self.lines += 1;
        self.gz.write_all(value.to_string().as_bytes())
    }

    /// `schema` is the `user_version` of the database the rows come from.
    pub(crate) fn begin(&mut self, schema: i64) -> io::Result<()> {
        self.line(json!({ "t": "begin", "schema": schema }))
    }

    pub(crate) fn config(&mut self, toml: &str) -> io::Result<()> {
        self.line(json!({ "t": "config", "toml": toml }))
    }

    /// Only for a stream that is encrypted.
    pub(crate) fn secret(&mut self, key: &str, value: &Secret) -> io::Result<()> {
        self.line(json!({ "t": "secret", "key": key, "value": value.expose() }))
    }

    pub(crate) fn table(&mut self, name: &str, columns: &[String]) -> io::Result<()> {
        self.line(json!({ "t": "table", "name": name, "columns": columns }))
    }

    pub(crate) fn row(&mut self, cells: &[Cell]) -> io::Result<()> {
        let v: Vec<Value> = cells.iter().map(Cell::to_json).collect();
        self.line(json!({ "t": "row", "v": v }))
    }

    /// Writes the closing line and ends the gzip member.
    pub(crate) fn finish(mut self) -> io::Result<W> {
        let before = self.lines;
        self.line(json!({ "t": "end", "lines": before }))?;
        self.gz.finish()
    }
}

/// Marks an error as "over a size limit" on its way up through the readers.
#[derive(Debug)]
struct OverLimit;

impl std::fmt::Display for OverLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("backup is over the size limits")
    }
}

impl std::error::Error for OverLimit {}

/// What a failed read of the body means: over a limit, a damaged file, or a
/// real input error.
pub(crate) fn stream_error(e: io::Error) -> BackupError {
    if e.get_ref().is_some_and(|inner| inner.is::<OverLimit>()) {
        return BackupError::TooBig;
    }
    match e.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::UnexpectedEof => {
            BackupError::Damaged
        }
        _ => BackupError::Io(e),
    }
}

/// Lets through at most `left` bytes; one more is a size error.
pub(crate) struct Capped<R: Read> {
    inner: R,
    left: u64,
}

impl<R: Read> Capped<R> {
    pub(crate) fn new(inner: R, limit: u64) -> Self {
        Self { inner, left: limit }
    }
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            // Only an input that really has more is over the limit.
            let mut one = [0u8; 1];
            return match self.inner.read(&mut one)? {
                0 => Ok(0),
                _ => Err(io::Error::new(io::ErrorKind::InvalidData, OverLimit)),
            };
        }
        let take = out
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut out[..take])?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// One line of the payload between "begin" and "end".
#[derive(Debug)]
pub(crate) enum Line {
    /// The settings as TOML text.
    Config(String),
    Secret {
        key: String,
        value: Secret,
        /// Characters in the value.
        chars: usize,
    },
    Table {
        name: String,
        columns: Vec<String>,
    },
    Row(Vec<Cell>),
    /// A kind of line this version does not know.
    Unknown,
}

/// Bytes asked from the decompressor at a time.
const BLOCK: usize = 64 << 10;

/// A line parsed as a JSON object, and whether a line end closed it.
type JsonLine = (Map<String, Value>, bool);

/// Reads the lines back, holding every limit. `next` returns `None` only
/// after a closing line with the right count and the true end of the stream.
pub(crate) struct PayloadReader<'l, R: BufRead> {
    gz: GzDecoder<R>,
    limits: &'l Limits,
    buf: Vec<u8>,
    /// Start of the unread part of `buf`.
    pos: usize,
    /// How far `buf` was searched for a line end.
    scanned: usize,
    eof: bool,
    plain: u64,
    lines: u64,
    rows: u64,
    secrets: usize,
    /// Columns of the table the rows belong to.
    arity: Option<usize>,
    schema: i64,
    ended: bool,
}

fn field<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a Value, BackupError> {
    map.get(key).ok_or(BackupError::Damaged)
}

fn string(map: &mut Map<String, Value>, key: &str) -> Result<String, BackupError> {
    match map.remove(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(BackupError::Damaged),
    }
}

impl<'l, R: BufRead> PayloadReader<'l, R> {
    /// Starts the stream and reads the opening line.
    pub(crate) fn open(input: R, limits: &'l Limits) -> Result<Self, BackupError> {
        let mut reader = Self {
            gz: GzDecoder::new(input),
            limits,
            buf: Vec::new(),
            pos: 0,
            scanned: 0,
            eof: false,
            plain: 0,
            lines: 0,
            rows: 0,
            secrets: 0,
            arity: None,
            schema: 0,
            ended: false,
        };
        let (map, _) = reader.object()?.ok_or(BackupError::Damaged)?;
        if field(&map, "t")?.as_str() != Some("begin") {
            return Err(BackupError::Damaged);
        }
        reader.schema = field(&map, "schema")?
            .as_i64()
            .ok_or(BackupError::Damaged)?;
        Ok(reader)
    }

    /// Schema version of the database the backup was made from.
    pub(crate) fn schema(&self) -> i64 {
        self.schema
    }

    /// Pulls more of the stream into the buffer. `false` at its end.
    fn fill(&mut self) -> Result<bool, BackupError> {
        if self.eof {
            return Ok(false);
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.scanned -= self.pos;
            self.pos = 0;
        }
        let old = self.buf.len();
        self.buf.resize(old + BLOCK, 0);
        let got = loop {
            match self.gz.read(&mut self.buf[old..]) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.buf.truncate(old);
                    return Err(stream_error(e));
                }
            }
        };
        self.buf.truncate(old + got);
        self.plain += got as u64;
        if self.plain > self.limits.plain {
            return Err(BackupError::TooBig);
        }
        if got == 0 {
            self.eof = true;
            // The payload is one gzip member: nothing may follow it.
            let rest = self.gz.get_mut().fill_buf().map_err(stream_error)?;
            if !rest.is_empty() {
                return Err(BackupError::Damaged);
            }
        }
        Ok(got > 0)
    }

    /// The next line as a range of `buf`, and whether a line end closed it.
    fn raw_line(&mut self) -> Result<Option<(std::ops::Range<usize>, bool)>, BackupError> {
        loop {
            if let Some(at) = self.buf[self.scanned..].iter().position(|b| *b == b'\n') {
                let end = self.scanned + at;
                let line = self.pos..end;
                self.pos = end + 1;
                self.scanned = self.pos;
                if line.len() > self.limits.line {
                    return Err(BackupError::TooBig);
                }
                return Ok(Some((line, true)));
            }
            self.scanned = self.buf.len();
            if self.buf.len() - self.pos > self.limits.line {
                return Err(BackupError::TooBig);
            }
            if !self.fill()? {
                if self.pos == self.buf.len() {
                    return Ok(None);
                }
                let line = self.pos..self.buf.len();
                self.pos = self.buf.len();
                return Ok(Some((line, false)));
            }
        }
    }

    /// The next line as a JSON object.
    fn object(&mut self) -> Result<Option<JsonLine>, BackupError> {
        let Some((range, closed)) = self.raw_line()? else {
            return Ok(None);
        };
        self.lines += 1;
        match serde_json::from_slice(&self.buf[range]) {
            Ok(Value::Object(map)) => Ok(Some((map, closed))),
            _ => Err(BackupError::Damaged),
        }
    }

    pub(crate) fn next(&mut self) -> Result<Option<Line>, BackupError> {
        if self.ended {
            return Ok(None);
        }
        // The stream may not just stop: the closing line is required.
        let (mut map, closed) = self.object()?.ok_or(BackupError::Damaged)?;
        let kind = string(&mut map, "t")?;
        match kind.as_str() {
            "begin" => Err(BackupError::Damaged),
            "end" => {
                let before = self.lines - 1;
                if field(&map, "lines")?.as_u64() != Some(before) || closed {
                    return Err(BackupError::Damaged);
                }
                // Reads on to the true end: the gzip checksum and, for an
                // encrypted file, the last chunk are checked on the way.
                if self.raw_line()?.is_some() {
                    return Err(BackupError::Damaged);
                }
                self.ended = true;
                Ok(None)
            }
            "config" => {
                let toml = string(&mut map, "toml")?;
                if toml.len() > self.limits.config {
                    return Err(BackupError::TooBig);
                }
                Ok(Some(Line::Config(toml)))
            }
            "secret" => {
                self.secrets += 1;
                if self.secrets > self.limits.secrets {
                    return Err(BackupError::TooBig);
                }
                let key = string(&mut map, "key")?;
                let value = string(&mut map, "value")?;
                let chars = value.chars().count();
                Ok(Some(Line::Secret {
                    key,
                    value: Secret::new(value),
                    chars,
                }))
            }
            "table" => {
                let name = string(&mut map, "name")?;
                let columns = match map.remove("columns") {
                    Some(Value::Array(items)) => items
                        .into_iter()
                        .map(|c| match c {
                            Value::String(s) => Ok(s),
                            _ => Err(BackupError::Damaged),
                        })
                        .collect::<Result<Vec<String>, _>>()?,
                    _ => return Err(BackupError::Damaged),
                };
                self.arity = Some(columns.len());
                Ok(Some(Line::Table { name, columns }))
            }
            "row" => {
                self.rows += 1;
                if self.rows > self.limits.rows {
                    return Err(BackupError::TooBig);
                }
                let Some(Value::Array(values)) = map.remove("v") else {
                    return Err(BackupError::Damaged);
                };
                if self.arity != Some(values.len()) {
                    return Err(BackupError::Damaged);
                }
                Ok(Some(Line::Row(
                    values.into_iter().map(Cell::from_json).collect(),
                )))
            }
            _ => Ok(Some(Line::Unknown)),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A payload made by hand: the lines joined by line ends, gzipped.
    pub(crate) fn gz(lines: &[&str]) -> Vec<u8> {
        gz_bytes(lines.join("\n").as_bytes())
    }

    pub(crate) fn gz_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(bytes).unwrap();
        e.finish().unwrap()
    }

    pub(crate) fn gunzip(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        GzDecoder::new(bytes).read_to_end(&mut out).unwrap();
        out
    }

    fn read_all(bytes: &[u8], limits: &Limits) -> Result<Vec<Line>, BackupError> {
        let mut r = PayloadReader::open(bytes, limits)?;
        let mut lines = Vec::new();
        while let Some(line) = r.next()? {
            lines.push(line);
        }
        // Stays ended.
        assert!(r.next()?.is_none());
        Ok(lines)
    }

    fn read(bytes: &[u8]) -> Result<Vec<Line>, BackupError> {
        read_all(bytes, &Limits::default())
    }

    const BEGIN: &str = r#"{"t":"begin","schema":7}"#;

    #[test]
    fn lines_round_trip_with_every_kind_of_value() {
        let cells = vec![
            Cell::Null,
            Cell::Int(-42),
            Cell::Int(i64::MAX),
            Cell::Real(1.5),
            Cell::Text("Заголовок «с кавычками»\nи переводом строки".into()),
            Cell::Blob(vec![0, 1, 2, 255]),
            Cell::Blob(Vec::new()),
        ];
        let mut w = PayloadWriter::new(Vec::new());
        w.begin(7).unwrap();
        w.config("[assistant]\nname = \"Айри\"\n").unwrap();
        w.secret("speech.api_key", &Secret::new("sk-value"))
            .unwrap();
        w.table("notes", &["id".to_string(), "title".to_string()])
            .unwrap();
        w.table("wide", &vec!["c".to_string(); cells.len()])
            .unwrap();
        w.row(&cells).unwrap();
        w.row(&vec![Cell::Real(f64::NAN); 7]).unwrap();
        let bytes = w.finish().unwrap();

        let text = String::from_utf8(gunzip(&bytes)).unwrap();
        // No line end after the closing line.
        assert!(text.ends_with('}'));
        let written: Vec<Value> = text
            .split('\n')
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(written.len(), 8);
        assert_eq!(written[0], json!({ "t": "begin", "schema": 7 }));
        assert_eq!(written[7], json!({ "t": "end", "lines": 7 }));

        let limits = Limits::default();
        let mut r = PayloadReader::open(&bytes[..], &limits).unwrap();
        assert_eq!(r.schema(), 7);
        assert!(matches!(r.next().unwrap(), Some(Line::Config(t)) if t.contains("Айри")));
        match r.next().unwrap() {
            Some(Line::Secret { key, value, chars }) => {
                assert_eq!(key, "speech.api_key");
                assert_eq!(value, Secret::new("sk-value"));
                assert_eq!(chars, 8);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            r.next().unwrap(),
            Some(Line::Table { name, columns }) if name == "notes" && columns == ["id", "title"]
        ));
        assert!(matches!(r.next().unwrap(), Some(Line::Table { .. })));
        assert!(matches!(r.next().unwrap(), Some(Line::Row(got)) if got == cells));
        // A number that is not finite travels as null.
        assert!(matches!(r.next().unwrap(), Some(Line::Row(got)) if got == vec![Cell::Null; 7]));
        assert!(r.next().unwrap().is_none());
    }

    #[test]
    fn values_a_file_cannot_hold_fit_no_column() {
        let lines = read(&gz(&[
            BEGIN,
            r#"{"t":"table","name":"x","columns":["a","b","c","d","e","f"]}"#,
            r#"{"t":"row","v":[true,[1],{"b":"!!"},{"b":"AA==","c":1},{"x":1},18446744073709551615]}"#,
            r#"{"t":"end","lines":3}"#,
        ]))
        .unwrap();
        let Line::Row(cells) = &lines[1] else {
            panic!("{lines:?}")
        };
        assert_eq!(
            cells[..5],
            [Cell::Bad, Cell::Bad, Cell::Bad, Cell::Bad, Cell::Bad]
        );
        // Too big for an integer column: a real number.
        assert!(matches!(cells[5], Cell::Real(_)));
    }

    #[test]
    fn a_stream_that_does_not_close_properly_is_damaged() {
        let table = r#"{"t":"table","name":"notes","columns":["id","title"]}"#;
        let row = r#"{"t":"row","v":[1,"a"]}"#;
        let damaged: Vec<Vec<&str>> = vec![
            // No closing line.
            vec![BEGIN, table, row],
            vec![BEGIN],
            // The wrong count.
            vec![BEGIN, table, row, r#"{"t":"end","lines":2}"#],
            vec![BEGIN, table, row, r#"{"t":"end","lines":4}"#],
            vec![BEGIN, table, row, r#"{"t":"end"}"#],
            // A row before any table, and a row of another width.
            vec![BEGIN, row, r#"{"t":"end","lines":2}"#],
            vec![
                BEGIN,
                table,
                r#"{"t":"row","v":[1]}"#,
                r#"{"t":"end","lines":3}"#,
            ],
            vec![
                BEGIN,
                table,
                r#"{"t":"row","v":[1,"a","b"]}"#,
                r#"{"t":"end","lines":3}"#,
            ],
            // Not an object, no kind, a broken known line.
            vec![BEGIN, "[1,2]", r#"{"t":"end","lines":2}"#],
            vec![BEGIN, "", r#"{"t":"end","lines":2}"#],
            vec![BEGIN, r#"{"x":1}"#, r#"{"t":"end","lines":2}"#],
            vec![BEGIN, r#"{"t":7}"#, r#"{"t":"end","lines":2}"#],
            vec![
                BEGIN,
                r#"{"t":"table","name":"notes"}"#,
                r#"{"t":"end","lines":2}"#,
            ],
            vec![
                BEGIN,
                r#"{"t":"table","name":"notes","columns":[1]}"#,
                r#"{"t":"end","lines":2}"#,
            ],
            vec![BEGIN, r#"{"t":"config"}"#, r#"{"t":"end","lines":2}"#],
            vec![
                BEGIN,
                r#"{"t":"secret","key":"k"}"#,
                r#"{"t":"end","lines":2}"#,
            ],
            vec![BEGIN, table, r#"{"t":"row"}"#, r#"{"t":"end","lines":3}"#],
            // A second opening line, and anything after the closing one.
            vec![BEGIN, BEGIN, r#"{"t":"end","lines":2}"#],
            vec![BEGIN, r#"{"t":"end","lines":1}"#, row],
            vec![BEGIN, r#"{"t":"end","lines":1}"#, ""],
            vec![
                BEGIN,
                r#"{"t":"end","lines":1}"#,
                r#"{"t":"end","lines":2}"#,
            ],
        ];
        for lines in damaged {
            assert!(
                matches!(read(&gz(&lines)), Err(BackupError::Damaged)),
                "{lines:?}"
            );
        }
        // The first line must be the opening one.
        for first in [vec![table], vec![r#"{"t":"begin"}"#], vec![""], vec!["x"]] {
            assert!(
                matches!(
                    PayloadReader::open(&gz(&first)[..], &Limits::default()),
                    Err(BackupError::Damaged)
                ),
                "{first:?}"
            );
        }
        assert!(read(&gz(&[BEGIN, table, row, r#"{"t":"end","lines":3}"#])).is_ok());
        // An empty payload and bytes that are not gzip.
        assert!(matches!(
            PayloadReader::open(&gz_bytes(b"")[..], &Limits::default()),
            Err(BackupError::Damaged)
        ));
        assert!(matches!(
            PayloadReader::open(&b"plain text, not gzip"[..], &Limits::default()),
            Err(BackupError::Damaged)
        ));
    }

    #[test]
    fn a_cut_or_padded_gzip_member_is_damaged() {
        let good = gz(&[BEGIN, r#"{"t":"end","lines":1}"#]);
        assert!(read(&good).is_ok());
        // Bytes after the member.
        let mut padded = good.clone();
        padded.push(0);
        assert!(matches!(read(&padded), Err(BackupError::Damaged)));
        // A second member.
        let twice = [&good[..], &good[..]].concat();
        assert!(matches!(read(&twice), Err(BackupError::Damaged)));
        // The checksum at the end cut off or changed.
        assert!(matches!(
            read(&good[..good.len() - 1]),
            Err(BackupError::Damaged)
        ));
        let mut crc = good.clone();
        let at = crc.len() - 8;
        crc[at] ^= 1;
        assert!(matches!(read(&crc), Err(BackupError::Damaged)));
    }

    #[test]
    fn every_size_limit_is_held() {
        let table = r#"{"t":"table","name":"notes","columns":["id"]}"#;
        let small = |change: fn(&mut Limits)| {
            let mut l = Limits::default();
            change(&mut l);
            l
        };

        // A line longer than the limit, closed or not.
        let long = format!(r#"{{"t":"config","toml":"{}"}}"#, "a".repeat(5000));
        let bytes = gz(&[BEGIN, &long, r#"{"t":"end","lines":2}"#]);
        assert!(read(&bytes).is_ok());
        let limits = small(|l| l.line = 1000);
        assert!(matches!(
            read_all(&bytes, &limits),
            Err(BackupError::TooBig)
        ));
        let unclosed = gz(&[BEGIN, &"a".repeat(200_000)]);
        assert!(matches!(
            read_all(&unclosed, &limits),
            Err(BackupError::TooBig)
        ));

        // Too many rows.
        let rows = gz(&[
            BEGIN,
            table,
            r#"{"t":"row","v":[1]}"#,
            r#"{"t":"row","v":[2]}"#,
            r#"{"t":"row","v":[3]}"#,
            r#"{"t":"end","lines":5}"#,
        ]);
        assert!(read_all(&rows, &small(|l| l.rows = 3)).is_ok());
        assert!(matches!(
            read_all(&rows, &small(|l| l.rows = 2)),
            Err(BackupError::TooBig)
        ));

        // A gzip bomb: a few kilobytes that unpack into megabytes.
        let mut bomb = String::from(BEGIN);
        for _ in 0..4000 {
            bomb.push('\n');
            bomb.push_str(r#"{"t":"later","pad":"0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"}"#);
        }
        bomb.push_str("\n{\"t\":\"end\",\"lines\":4001}");
        let packed = gz_bytes(bomb.as_bytes());
        assert!(packed.len() < 20_000 && bomb.len() > 1_000_000);
        assert_eq!(read(&packed).unwrap().len(), 4000);
        assert!(matches!(
            read_all(&packed, &small(|l| l.plain = 100_000)),
            Err(BackupError::TooBig)
        ));

        // Settings longer than the limit.
        assert!(matches!(
            read_all(&bytes, &small(|l| l.config = 4999)),
            Err(BackupError::TooBig)
        ));
        assert!(read_all(&bytes, &small(|l| l.config = 5000)).is_ok());

        // Too many secret lines.
        let secret = r#"{"t":"secret","key":"k","value":"v"}"#;
        let secrets = gz(&[BEGIN, secret, secret, secret, r#"{"t":"end","lines":4}"#]);
        assert!(read_all(&secrets, &small(|l| l.secrets = 3)).is_ok());
        assert!(matches!(
            read_all(&secrets, &small(|l| l.secrets = 2)),
            Err(BackupError::TooBig)
        ));
    }

    #[test]
    fn unknown_kinds_of_lines_are_passed_over() {
        let lines = read(&gz(&[
            BEGIN,
            r#"{"t":"avatar","model":"x"}"#,
            r#"{"t":"table","name":"later_table","columns":["a"]}"#,
            r#"{"t":"row","v":[1]}"#,
            r#"{"t":"end","lines":4}"#,
        ]))
        .unwrap();
        assert!(matches!(lines[0], Line::Unknown));
        // The reader does not know table names: the caller skips this one.
        assert!(matches!(&lines[1], Line::Table { name, .. } if name == "later_table"));
        assert!(matches!(lines[2], Line::Row(_)));
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn a_capped_reader_stops_at_its_limit() {
        let data = [7u8; 100];
        let mut out = Vec::new();
        Capped::new(&data[..], 100).read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), 100);
        let e = Capped::new(&data[..], 99)
            .read_to_end(&mut Vec::new())
            .unwrap_err();
        assert!(matches!(stream_error(e), BackupError::TooBig));
        // The mark survives the readers stacked above it.
        let packed = gz(&[BEGIN, r#"{"t":"end","lines":1}"#]);
        let capped = io::BufReader::new(Capped::new(&packed[..], 10));
        assert!(matches!(
            PayloadReader::open(capped, &Limits::default()).and_then(|mut r| r.next()),
            Err(BackupError::TooBig)
        ));
        assert!(matches!(
            stream_error(io::Error::new(io::ErrorKind::UnexpectedEof, "cut")),
            BackupError::Damaged
        ));
        assert!(matches!(
            stream_error(io::Error::new(io::ErrorKind::PermissionDenied, "no")),
            BackupError::Io(_)
        ));
    }
}
