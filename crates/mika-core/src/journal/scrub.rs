//! Keeps secrets out of the journal. Every message passes through here on its
//! way into a record, and there is no switch that turns it off.
//!
//! Two nets. The first knows the values: `Secret::new` remembers each one, and
//! any occurrence becomes `***`. The second knows the shapes: bearer tokens,
//! `Authorization` headers, `sk-…` keys, Telegram bot tokens, `key=value`
//! pairs with a telling name, the name and password inside an address.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::OnceLock;

use parking_lot::RwLock;
use regex::Regex;

/// Longest message of a record, in characters, the cut mark included.
pub const MAX_MESSAGE_CHARS: usize = 2000;
/// A value shorter than this is not remembered: it would match ordinary words.
pub const MIN_SECRET_CHARS: usize = 8;
/// How many values are remembered. When there are more, the one that was
/// remembered longest ago leaves; a value in use is remembered again and again.
pub const MAX_SECRETS: usize = 256;

const MASK: &str = "***";
/// Room kept for the cut mark inside `MAX_MESSAGE_CHARS`.
const MARK_ROOM: usize = 32;
/// A longer message is cut to this before the scrub, so that one huge message
/// costs a bounded amount of work.
const SCRUB_CHARS: usize = MAX_MESSAGE_CHARS * 4;

/// The values that must not reach a record, the latest remembered last.
struct Known {
    values: VecDeque<String>,
}

static SECRETS: RwLock<Known> = RwLock::new(Known::new());

impl Known {
    const fn new() -> Self {
        Self {
            values: VecDeque::new(),
        }
    }

    fn remember(&mut self, value: &str) {
        let value = value.trim();
        if value.chars().count() < MIN_SECRET_CHARS {
            return;
        }
        match self.values.iter().position(|known| known == value) {
            Some(at) if at + 1 == self.values.len() => {}
            Some(at) => {
                if let Some(known) = self.values.remove(at) {
                    self.values.push_back(known);
                }
            }
            None => {
                if self.values.len() >= MAX_SECRETS {
                    self.values.pop_front();
                }
                self.values.push_back(value.to_string());
            }
        }
    }

    /// Where the values stand in a text: byte ranges, merged where they
    /// overlap, so that no part of a value is left between two masks.
    fn find(&self, text: &str) -> Vec<(usize, usize)> {
        let mut found: Vec<(usize, usize)> = Vec::new();
        for value in &self.values {
            let step = value.chars().next().map_or(1, char::len_utf8);
            let mut from = 0;
            while let Some(at) = text.get(from..).and_then(|rest| rest.find(value.as_str())) {
                let at = from + at;
                found.push((at, at + value.len()));
                from = at + step;
            }
        }
        found.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(found.len());
        for (start, end) in found {
            match merged.last_mut() {
                Some((_, until)) if start <= *until => *until = (*until).max(end),
                _ => merged.push((start, end)),
            }
        }
        merged
    }

    /// Replaces every value with `***`.
    fn mask<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let found = self.find(text);
        if found.is_empty() {
            return Cow::Borrowed(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut done = 0;
        for (start, end) in found {
            out.push_str(&text[done..start]);
            out.push_str(MASK);
            done = end;
        }
        out.push_str(&text[done..]);
        Cow::Owned(out)
    }
}

/// Remembers a secret value, so that it never reaches a record. Called by
/// `Secret::new`. Short values are skipped; the set is bounded.
pub fn remember_secret(value: &str) {
    if value.trim().chars().count() >= MIN_SECRET_CHARS {
        SECRETS.write().remember(value);
    }
}

/// A name that says its value is a secret, as the last part of an identifier:
/// `key`, `api_key`, `x-api-key`, `accessToken`, `client_secret`, `password`.
/// `hotkey` and `max_tokens` are not such names.
const NAME: &str = r"(?:[A-Za-z0-9]+[_.\-])*(?:api[_\-]?key|[A-Za-z0-9]*(?:secret|access|private|auth)key|key|[A-Za-z0-9]*token|[A-Za-z0-9]*password|passwd|pwd|[A-Za-z0-9]*secret|signature|sig)";

/// The shapes, in the order they are applied: pattern and replacement.
fn rules() -> Vec<(String, &'static str)> {
    vec![
        // The name and password inside an address: `://name:password@`.
        (r#"://[^/\s@"'<>]+@"#.into(), "://***@"),
        // A Telegram bot token inside an address: `/bot<token>/`.
        (r"(?i)/bot[0-9]+:[A-Za-z0-9_\-]+".into(), "/bot***"),
        // A Telegram bot token on its own.
        (r"\b[0-9]{6,}:[A-Za-z0-9_\-]{30,}".into(), MASK),
        // `Authorization: Bearer …`, `"authorization": "Basic …"`.
        (
            r#"(?i)\b((?:proxy-)?authorization["']?[ \t]*[:=][ \t]*["']?)(?:(?:basic|bearer|token|digest|negotiate)[ \t]+)?[^\s"',;]+"#
                .into(),
            "${1}***",
        ),
        (
            r"(?i)\bbearer[ \t]+[A-Za-z0-9._~+/=\-]{8,}".into(),
            "Bearer ***",
        ),
        // Keys that say what they are by their first letters.
        (r"\bsk-[A-Za-z0-9_\-]{8,}".into(), "sk-***"),
        (
            r"\b(?:gsk_[A-Za-z0-9]{20,}|xai-[A-Za-z0-9]{20,}|AIza[A-Za-z0-9_\-]{30,}|gh[pousr]_[A-Za-z0-9]{30,}|xox[abprs]-[A-Za-z0-9\-]{10,}|eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,})"
                .into(),
            MASK,
        ),
        // JSON: `"api_key": "…"`, also inside an escaped string.
        (
            format!(r#"(?i)("{NAME}"\s*:\s*)"(?:[^"\\]|\\.)*""#),
            "${1}\"***\"",
        ),
        (
            format!(r#"(?i)(\\"{NAME}\\"\s*:\s*)\\"[^"\\]*\\""#),
            "${1}\\\"***\\\"",
        ),
        // `key=value` in an address or in text.
        (
            format!(
                r#"(?i)(^|[^A-Za-z0-9])({NAME}[ \t]*=[ \t]*)(?:"[^"]*"|'[^']*'|[^\s&"'<>#;,=][^\s&"'<>#;,]*)"#
            ),
            "${1}${2}***",
        ),
        // `name: value` for a header or a printed structure. A bare `key:` or
        // `token:` is ordinary text, so here the name must say more.
        (
            r#"(?i)(^|[^A-Za-z0-9])((?:(?:x-)?api[_\-]?key|(?:secret|private|access|auth)[_\-]?key|[A-Za-z0-9]+[_\-]token|[A-Za-z0-9]+[_\-]secret|password|passwd|secret)[ \t]*:[ \t]*(?:Some\()?)(?:"[^"]*"|'[^']*'|[^\s"',;)}\]]+)"#
                .into(),
            "${1}${2}***",
        ),
        // A printed structure: `token: "…"`, `key: Some("…")`.
        (
            r#"(?i)(^|[^A-Za-z0-9])((?:token|key)[ \t]*:[ \t]*(?:Some\()?)"[^"]*""#.into(),
            "${1}${2}\"***\"",
        ),
    ]
}

fn compiled() -> &'static [(Regex, &'static str)] {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    RULES.get_or_init(|| {
        rules()
            .into_iter()
            .filter_map(|(pattern, replacement)| {
                Regex::new(&pattern).ok().map(|re| (re, replacement))
            })
            .collect()
    })
}

/// A message without secrets: the remembered values and the known shapes are
/// replaced. Applying it twice changes nothing more.
pub fn scrub(text: &str) -> String {
    // A reader inside a reader must not wait for a writer in between: the
    // panic hook scrubs too, whatever its thread was doing.
    scrub_with(&SECRETS.read_recursive(), text)
}

/// `scrub` with the remembered values given.
fn scrub_with(known: &Known, text: &str) -> String {
    let mut out = known.mask(text).into_owned();
    for (pattern, replacement) in compiled() {
        if let Cow::Owned(changed) = pattern.replace_all(&out, *replacement) {
            out = changed;
        }
    }
    out
}

/// The first `max` characters of a text and how many were left out.
pub(crate) fn head(text: &str, max: usize) -> (&str, usize) {
    match text.char_indices().nth(max) {
        Some((at, _)) => (&text[..at], text[at..].chars().count()),
        None => (text, 0),
    }
}

/// What goes into a record: scrubbed, and no longer than `MAX_MESSAGE_CHARS`
/// with the cut mark.
pub(crate) fn clean(text: &str) -> String {
    clean_with(&SECRETS.read_recursive(), text)
}

/// `clean` with the remembered values given.
fn clean_with(known: &Known, text: &str) -> String {
    let (start, beyond) = if text.len() > SCRUB_CHARS {
        head(text, SCRUB_CHARS)
    } else {
        (text, 0)
    };
    let scrubbed = scrub_with(known, start);
    if beyond == 0 && scrubbed.len() <= MAX_MESSAGE_CHARS {
        return scrubbed;
    }
    let (kept, cut) = head(&scrubbed, MAX_MESSAGE_CHARS);
    if cut + beyond == 0 {
        return scrubbed;
    }
    let (kept, more) = head(kept, MAX_MESSAGE_CHARS - MARK_ROOM);
    format!("{kept} <cut {} chars>", cut + more + beyond)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_compiles() {
        assert_eq!(compiled().len(), rules().len());
    }

    /// A set of values of the test's own. The shared one holds whatever the
    /// other tests of the crate remembered, runs of one letter among them.
    fn known(values: &[&str]) -> Known {
        let mut known = Known::new();
        for value in values {
            known.remember(value);
        }
        known
    }

    #[test]
    fn a_remembered_value_becomes_stars_wherever_it_stands() {
        let known = known(&["zq-remembered-value-01", "  zq-padded-value-02\n"]);
        assert_eq!(
            scrub_with(
                &known,
                "failed with zq-remembered-value-01, twice: zq-remembered-value-01!"
            ),
            "failed with ***, twice: ***!"
        );
        // Inside other text and at both ends.
        assert_eq!(scrub_with(&known, "zq-remembered-value-01"), "***");
        assert_eq!(scrub_with(&known, "xzq-remembered-value-01x"), "x***x");
        // Spaces around a value are not a part of it.
        assert_eq!(scrub_with(&known, "[zq-padded-value-02]"), "[***]");
    }

    #[test]
    fn the_shared_set_is_what_scrub_uses() {
        remember_secret("zq-shared-value-03");
        assert_eq!(scrub("got zq-shared-value-03"), "got ***");
        assert_eq!(clean("got zq-shared-value-03"), "got ***");
    }

    #[test]
    fn a_short_value_is_not_remembered() {
        // Eight characters, not eight bytes.
        let known = known(&["zq-7chr", "", "   ", "zqсекрет"]);
        assert_eq!(known.values, ["zqсекрет"]);
        assert_eq!(scrub_with(&known, "zq-7chr stays"), "zq-7chr stays");
        assert_eq!(scrub_with(&known, "zqсекрет goes"), "*** goes");
    }

    #[test]
    fn overlapping_values_leave_no_part_behind() {
        let known = known(&[
            "zqoverlap-AAAA-BBBB",
            "AAAA-BBBB-zqtail-CCCC",
            "zqinner-0000",
            "xx-zqinner-0000-yy",
            // A value that overlaps itself.
            "zqzqzqzq",
        ]);
        assert_eq!(
            scrub_with(&known, "a zqoverlap-AAAA-BBBB-zqtail-CCCC b"),
            "a *** b"
        );
        assert_eq!(scrub_with(&known, "a xx-zqinner-0000-yy b"), "a *** b");
        assert_eq!(scrub_with(&known, "<zqzqzqzqzqzq>"), "<***>");
        // Side by side: nothing of either is left between the masks.
        assert_eq!(scrub_with(&known, "zqinner-0000zqinner-0000"), "***");
    }

    #[test]
    fn the_set_is_bounded_and_forgets_what_was_not_used_longest() {
        let mut known = Known::new();
        known.remember("zq-bounded-first-value");
        known.remember("zq-bounded-second-value");
        for n in 0..MAX_SECRETS - 2 {
            known.remember(&format!("zq-bounded-filler-{n:04}"));
        }
        assert_eq!(known.values.len(), MAX_SECRETS);
        // The first one is used again; the next new value pushes the second out.
        known.remember("zq-bounded-first-value");
        assert_eq!(known.values.len(), MAX_SECRETS);
        known.remember("zq-bounded-one-more");
        assert_eq!(known.values.len(), MAX_SECRETS);
        assert_eq!(known.mask("zq-bounded-first-value"), "***");
        assert_eq!(known.mask("zq-bounded-one-more"), "***");
        assert_eq!(
            known.mask("zq-bounded-second-value"),
            "zq-bounded-second-value"
        );
        // A value remembered twice takes one place.
        known.remember("zq-bounded-one-more");
        known.remember(" zq-bounded-one-more ");
        assert_eq!(known.values.len(), MAX_SECRETS);
    }

    #[test]
    fn known_shapes_are_replaced() {
        let cases = [
            (
                "GET https://api.telegram.org/bot123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw/getUpdates failed",
                "GET https://api.telegram.org/bot***/getUpdates failed",
            ),
            (
                "token 123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw rejected",
                "token *** rejected",
            ),
            (
                "Authorization: Bearer abc.DEF-ghi_123",
                "Authorization: ***",
            ),
            ("authorization=Basic dXNlcjpwYXNz", "authorization=***"),
            (
                r#"headers {"authorization": "Bearer abcdefgh", "accept": "*/*"}"#,
                r#"headers {"authorization": "***", "accept": "*/*"}"#,
            ),
            ("sent Bearer abcdefgh12345678 to host", "sent Bearer *** to host"),
            ("key sk-proj-abcDEF123456 is wrong", "key sk-*** is wrong"),
            ("sk-ant-api03-ABCDEFGH", "sk-***"),
            (
                "google AIzaSyA1234567890abcdefghijklmnopqrstuv done",
                "google *** done",
            ),
            (
                "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk ok",
                "jwt *** ok",
            ),
            (
                "https://host/v1/models?api_key=abcd1234&limit=5",
                "https://host/v1/models?api_key=***&limit=5",
            ),
            ("apikey=abcd1234", "apikey=***"),
            ("https://h/p?key=AbCd&token=xyz#frag", "https://h/p?key=***&token=***#frag"),
            ("password=hunter2 user=bob", "password=*** user=bob"),
            ("client_secret = \"two words\" next", "client_secret = *** next"),
            ("X-Api-Key: abcd1234", "X-Api-Key: ***"),
            ("accessToken=abc refresh_token=def", "accessToken=*** refresh_token=***"),
            (
                r#"body {"api_key":"abcd","model":"m","token": "t\"t"}"#,
                r#"body {"api_key":"***","model":"m","token": "***"}"#,
            ),
            (
                r#"raw "{\"api_key\":\"abcd\",\"n\":1}""#,
                r#"raw "{\"api_key\":\"***\",\"n\":1}""#,
            ),
            (
                r#"Config { api_key: "abcd", bot_token: Some("12:ab"), token: Some("zz") }"#,
                r#"Config { api_key: ***, bot_token: Some(***), token: Some("***") }"#,
            ),
            (
                "https://bob:hunter2@proxy.local:8080/path",
                "https://***@proxy.local:8080/path",
            ),
            ("ftp://ghp_token@host/x", "ftp://***@host/x"),
        ];
        let nothing = Known::new();
        for (text, clean) in cases {
            let scrubbed = scrub_with(&nothing, text);
            assert_eq!(scrubbed, clean, "{text}");
            assert_eq!(scrub_with(&nothing, &scrubbed), clean, "twice: {text}");
        }
    }

    #[test]
    fn ordinary_text_stays() {
        let texts = [
            "turn done in 1532 ms, 3 steps, 2 tool calls",
            "llm openai gpt-4o-mini 200 prompt_tokens=120 completion_tokens=45 max_tokens=800",
            "hotkey=Ctrl+Alt+D registered",
            "unexpected token: `}` at line 3",
            "missing key: model",
            "the bearer of bad news",
            "authorization failed with 401",
            "task-12345678 finished, risk-assessment-low",
            "see https://example.com/a/b and write to me@example.com",
            "C:\\Users\\name\\AppData\\Local\\Mika\\logs",
            "if key == other { skip }",
            "monkey=5 tokens=7",
        ];
        for text in texts {
            assert_eq!(scrub_with(&Known::new(), text), text);
        }
    }

    #[test]
    fn a_long_message_is_cut_with_a_mark() {
        let nothing = Known::new();
        let short = "я".repeat(MAX_MESSAGE_CHARS);
        assert_eq!(clean_with(&nothing, &short), short);

        let long = "я".repeat(MAX_MESSAGE_CHARS + 1);
        let cut = clean_with(&nothing, &long);
        assert!(cut.chars().count() <= MAX_MESSAGE_CHARS, "{}", cut.len());
        assert!(cut.ends_with(" <cut 33 chars>"), "{cut}");
        assert!(cut.starts_with("яяя"));

        let huge = "a".repeat(100_000);
        let cut = clean_with(&nothing, &huge);
        assert!(cut.chars().count() <= MAX_MESSAGE_CHARS);
        let kept = MAX_MESSAGE_CHARS - MARK_ROOM;
        assert!(cut.ends_with(&format!(" <cut {} chars>", 100_000 - kept)));
    }

    #[test]
    fn a_secret_across_the_cut_does_not_survive_in_part() {
        let known = known(&["zq-straddles-the-cut-0123456789"]);
        let at = MAX_MESSAGE_CHARS - MARK_ROOM - 10;
        let text = format!(
            "{}zq-straddles-the-cut-0123456789{}",
            "x".repeat(at),
            "y".repeat(500)
        );
        let cut = clean_with(&known, &text);
        assert!(!cut.contains("zq-straddles"), "{cut}");
        assert!(cut.contains("***"));
        // The same at the edge of the part that is scrubbed at all.
        let text = format!(
            "{}zq-straddles-the-cut-0123456789",
            "x".repeat(SCRUB_CHARS - 12)
        );
        assert!(!clean_with(&known, &text).contains("zq-straddles"));
    }

    #[test]
    fn head_counts_characters() {
        assert_eq!(head("привет", 3), ("при", 3));
        assert_eq!(head("привет", 6), ("привет", 0));
        assert_eq!(head("", 0), ("", 0));
    }
}
