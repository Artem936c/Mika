//! Voice marks of Fish Audio Drama 3 in reply text (decision 0026): directions
//! in square brackets, `[softly, with a smile]`, and paired effect tags,
//! `<whisper>…</whisper>`. The voice performs them; the chat never shows them.
//!
//! `MarkFilter` splits a streamed reply into the text to show and the text to
//! speak. `strip_marks` and `tokens` do the same for a whole text. All three
//! share one scanner, so the rules are the same everywhere.

/// Paired tags Drama 3 knows: seven effects and seven languages.
pub const EFFECT_TAGS: [&str; 14] = [
    "whisper", "emphasis", "soft", "fast", "slow", "stress", "forceful", "french", "spanish",
    "german", "japanese", "chinese", "korean", "italian",
];

/// Longest text inside the brackets of a direction, characters.
pub const MAX_DIRECTION: usize = 80;

/// What one piece of a streamed reply gives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkPiece {
    /// Text without marks, for the chat and everywhere text is read.
    pub shown: String,
    /// The same text as it came, marks included, for the voice only.
    pub spoken: String,
    /// Text inside each direction cut in this piece, trimmed.
    pub directions: Vec<String>,
}

/// A piece of a whole text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Plain text as it came, spacing untouched.
    Text(String),
    /// Text inside the brackets of a direction, trimmed.
    Direction(String),
    /// `<name>` of a known tag, the name in lower case.
    Open(&'static str),
    /// `</name>` of a known tag, the name in lower case.
    Close(&'static str),
}

/// Characters a direction may hold besides letters and spaces.
fn fits_direction(ch: char) -> bool {
    ch.is_alphabetic()
        || (ch.is_whitespace() && ch != '\n' && ch != '\r')
        || matches!(
            ch,
            ',' | '.' | '!' | '?' | '…' | '\'' | '’' | '-' | '–' | '—' | '"' | '«' | '»' | ':'
        )
}

/// Whether the text inside `[...]` makes a direction (decision 0026): at
/// most `MAX_DIRECTION` characters that fit a direction, two letters in a row
/// somewhere, no dot right before a letter (`[файл.txt]`, `[ya.ru]` are
/// names) and no straight quote at the start (`["a", "b"]` is a list).
pub fn is_direction(inner: &str) -> bool {
    let mut run = 0;
    let mut word = false;
    let mut prev = None;
    for (n, ch) in inner.chars().enumerate() {
        if n >= MAX_DIRECTION || !fits_direction(ch) {
            return false;
        }
        if prev == Some('.') && ch.is_alphabetic() {
            return false;
        }
        run = if ch.is_alphabetic() { run + 1 } else { 0 };
        word |= run >= 2;
        prev = Some(ch);
    }
    word && !inner.trim_start().starts_with('"')
}

/// Characters of a list or quote marker at the start of a line: `- `, `* `,
/// `1. `, `2) `, `> `. `before` is the character before `ch`.
fn line_prefix(ch: char, before: Option<char>) -> bool {
    ch.is_whitespace()
        || matches!(ch, '-' | '*' | '+' | '>')
        || ch.is_ascii_digit()
        || (matches!(ch, '.' | ')') && before.is_some_and(|c| c.is_ascii_digit()))
}

/// Characters of the info string after the backticks that open a code block:
/// `python`, `c++`, `objective-c`.
fn info_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '-' | '+' | '#' | '.' | '_')
}

/// The known tag that `held` spells in full: `<whisper>` or `</Whisper>`.
fn known_tag(held: &str) -> Option<(&'static str, bool)> {
    let rest = held.strip_prefix('<')?.strip_suffix('>')?;
    let (name, close) = match rest.strip_prefix('/') {
        Some(name) => (name, true),
        None => (rest, false),
    };
    EFFECT_TAGS
        .iter()
        .find(|t| t.eq_ignore_ascii_case(name))
        .map(|t| (*t, close))
}

/// Whether `held` can still grow into a known tag: `<`, `</`, `<Whi`.
fn tag_prefix(held: &str) -> bool {
    let Some(rest) = held.strip_prefix('<') else {
        return false;
    };
    let name = rest.strip_prefix('/').unwrap_or(rest);
    EFFECT_TAGS
        .iter()
        .any(|t| t.len() >= name.len() && t[..name.len()].eq_ignore_ascii_case(name))
}

/// What the scanner decided about a stretch of the text.
enum Found<'a> {
    /// Plain text: it stays.
    Text(&'a str),
    /// `[...]`: the whole mark and the text inside the brackets.
    Direction { raw: &'a str, inner: &'a str },
    /// `<name>` or `</name>` of a known tag; `name` in lower case.
    Tag {
        raw: &'a str,
        name: &'static str,
        close: bool,
    },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Hold {
    #[default]
    Nothing,
    /// A run of `*` or `_`: emphasis that may wrap a direction, `*[sigh]*`.
    Emph,
    /// `[` and what came after it, not closed yet.
    Bracket,
    /// A closed `[...]` waiting for the next character: `(` makes it a link.
    Closed,
    /// `<` or `</` and the start of a known tag name.
    Tag,
}

/// Finds marks in text that comes piece by piece. A mark may be split at any
/// character, so only text that is still undecided is held back.
#[derive(Debug, Default)]
struct Scanner {
    /// Text that may still turn out to be a mark.
    held: String,
    hold: Hold,
    /// Characters inside the brackets of `held`.
    inner: usize,
    /// Length of the emphasis run before the `[` of `held`, bytes.
    emph: usize,
    /// Emphasis characters after the `]` of `held` that close that run.
    closing: usize,
    /// The last character seen.
    prev: Option<char>,
    /// Inside a fenced code block.
    fence: bool,
    /// The code block was opened on the current line: backticks later on
    /// this line show it was inline code after all, ```` ```x``` ````.
    fence_here: bool,
    /// Length of the backtick run that opened the code block.
    fence_ticks: usize,
    /// Length of the backtick run that opened inline code; 0 outside it.
    inline: usize,
    /// Only an info string followed the three or more backticks that opened
    /// inline code: at the end of the line they open a code block instead.
    info: bool,
    /// Backticks in the run being read.
    ticks: usize,
    /// Whether that run began its line.
    ticks_begin_line: bool,
    /// The current line has something besides whitespace and a list or
    /// quote marker.
    mid_line: bool,
}

impl Scanner {
    fn push(&mut self, chunk: &str, out: &mut dyn FnMut(Found<'_>)) {
        for ch in chunk.chars() {
            self.feed(ch, out);
        }
    }

    /// Decides what is still held: a complete direction is a mark, anything
    /// else is text. The scanner is ready for a new text afterwards.
    fn finish(&mut self, out: &mut dyn FnMut(Found<'_>)) {
        match self.hold {
            Hold::Closed => self.cut_direction(out),
            Hold::Emph | Hold::Bracket | Hold::Tag => self.release(out),
            Hold::Nothing => {}
        }
        *self = Self::default();
    }

    fn feed(&mut self, ch: char, out: &mut dyn FnMut(Found<'_>)) {
        let before = self.prev.replace(ch);
        match self.hold {
            Hold::Nothing => self.plain(ch, before, out),
            Hold::Emph => {
                let wrap = self.held.starts_with(ch);
                if ch == '[' || (wrap && self.held.len() < 3) {
                    self.track(ch, before);
                    if ch == '[' {
                        self.emph = self.held.len();
                        self.hold = Hold::Bracket;
                    }
                    self.held.push(ch);
                } else {
                    self.release(out);
                    self.plain(ch, before, out);
                }
            }
            Hold::Bracket if ch == ']' => {
                self.held.push(ch);
                let inner = &self.held[self.emph + 1..self.held.len() - 1];
                if is_direction(inner) {
                    self.hold = Hold::Closed;
                } else {
                    self.release(out);
                }
            }
            Hold::Bracket if self.inner < MAX_DIRECTION && fits_direction(ch) => {
                self.held.push(ch);
                self.inner += 1;
            }
            Hold::Bracket => {
                self.release(out);
                self.plain(ch, before, out);
            }
            Hold::Closed if self.emph > 0 && self.held.starts_with(ch) => {
                // The emphasis that opened before the `[` closes after the `]`.
                self.track(ch, before);
                self.held.push(ch);
                self.closing += 1;
                if self.closing == self.emph {
                    self.cut_direction(out);
                }
            }
            Hold::Closed => {
                if ch == '(' && self.closing == 0 {
                    // `[text](url)` is a link.
                    self.release(out);
                } else {
                    self.cut_direction(out);
                }
                self.plain(ch, before, out);
            }
            Hold::Tag => {
                self.held.push(ch);
                if let Some((name, close)) = known_tag(&self.held) {
                    out(Found::Tag {
                        raw: &self.held,
                        name,
                        close,
                    });
                    self.clear();
                } else if !tag_prefix(&self.held) {
                    self.held.pop();
                    self.release(out);
                    self.plain(ch, before, out);
                }
            }
        }
    }

    /// A character outside any held mark. `before` is the character before it.
    fn plain(&mut self, ch: char, before: Option<char>, out: &mut dyn FnMut(Found<'_>)) {
        if ch == '`' {
            if self.ticks == 0 {
                // A list or quote marker before the backticks still counts
                // as the start of the line: `- ```bash`.
                self.ticks_begin_line = !self.mid_line;
            }
            self.ticks += 1;
            self.mid_line = true;
            out(Found::Text("`"));
            return;
        }
        self.track(ch, before);
        if !self.fence && self.inline == 0 {
            let hold = match ch {
                // `a[i]` and `массив[0]` are never marks.
                '[' if !before.is_some_and(char::is_alphanumeric) => Hold::Bracket,
                '<' => Hold::Tag,
                '*' | '_' => Hold::Emph,
                _ => Hold::Nothing,
            };
            if hold != Hold::Nothing {
                self.held.push(ch);
                self.hold = hold;
                return;
            }
        }
        out(Found::Text(ch.encode_utf8(&mut [0; 4])));
    }

    /// Where code and lines are after a character that is not a backtick.
    fn track(&mut self, ch: char, before: Option<char>) {
        if self.ticks > 0 {
            self.end_ticks();
        }
        if ch == '\n' {
            // ```` Вот код: ```python ```` opens a block, though not at the
            // start of its line.
            if self.inline >= 3 && self.info {
                self.fence = true;
            }
            self.mid_line = false;
            self.inline = 0;
            self.info = false;
            self.fence_here = false;
            return;
        }
        if !line_prefix(ch, before) {
            self.mid_line = true;
        }
        if self.info && !info_char(ch) {
            self.info = false;
        }
    }

    /// A run of backticks ended: three or more at the start of a line open or
    /// close a code block, others open or close inline code.
    fn end_ticks(&mut self) {
        let n = std::mem::take(&mut self.ticks);
        if self.fence_here {
            // Backticks on the line that opened the block: it was inline code.
            self.fence = false;
            self.fence_here = false;
            self.inline = if n == self.fence_ticks {
                0
            } else {
                self.fence_ticks
            };
        } else if self.ticks_begin_line && n >= 3 {
            self.fence = !self.fence;
            self.fence_here = self.fence;
            self.fence_ticks = n;
            self.inline = 0;
            self.info = false;
        } else if !self.fence {
            if self.inline == 0 {
                self.inline = n;
                self.info = n >= 3;
            } else if self.inline == n {
                self.inline = 0;
                self.info = false;
            }
        }
    }

    /// The held text is not a mark.
    fn release(&mut self, out: &mut dyn FnMut(Found<'_>)) {
        out(Found::Text(&self.held));
        self.clear();
    }

    /// The held `[...]` is a direction. Emphasis around it goes with it when
    /// it closes as it opened, `*[sigh]*`; otherwise it stays as text.
    fn cut_direction(&mut self, out: &mut dyn FnMut(Found<'_>)) {
        let open = self.emph;
        let end = self.held.len() - self.closing;
        let inner = &self.held[open + 1..end - 1];
        if open > 0 && self.closing == open {
            out(Found::Direction {
                raw: &self.held,
                inner,
            });
        } else {
            if open > 0 {
                out(Found::Text(&self.held[..open]));
            }
            out(Found::Direction {
                raw: &self.held[open..end],
                inner,
            });
            if self.closing > 0 {
                out(Found::Text(&self.held[end..]));
            }
        }
        self.clear();
    }

    fn clear(&mut self) {
        self.held.clear();
        self.hold = Hold::Nothing;
        self.inner = 0;
        self.emph = 0;
        self.closing = 0;
    }
}

/// Spacing of the shown text where marks were cut.
#[derive(Debug, Default)]
struct View {
    /// Something visible was shown already.
    started: bool,
    /// Whitespace waiting for the next visible character.
    gap: String,
    /// A mark was cut since the last visible character.
    cut: bool,
    /// A mark was cut right after the last visible character, with no
    /// whitespace between them.
    glued: bool,
    /// The last visible character opens something: a bracket, a quote or
    /// emphasis, `«`, `(`, `"`, `**`.
    opens: bool,
    /// The last visible character.
    last: Option<char>,
}

impl View {
    fn take(&mut self, found: Found<'_>, piece: &mut MarkPiece) {
        match found {
            Found::Text(text) => {
                piece.spoken.push_str(text);
                for ch in text.chars() {
                    self.show(ch, &mut piece.shown);
                }
            }
            Found::Direction { raw, inner } => {
                piece.spoken.push_str(raw);
                piece.directions.push(inner.trim().to_string());
                self.cut();
            }
            Found::Tag { raw, .. } => {
                piece.spoken.push_str(raw);
                self.cut();
            }
        }
    }

    fn cut(&mut self) {
        self.glued |= self.started && self.gap.is_empty();
        self.cut = true;
    }

    /// No whitespace before the first visible character. Where a mark was cut,
    /// no double space, no space before punctuation and none after an opening
    /// bracket, quote or emphasis the mark stuck to; line breaks stay.
    fn show(&mut self, ch: char, shown: &mut String) {
        if ch.is_whitespace() {
            if self.started {
                self.gap.push(ch);
            }
            return;
        }
        let spaced = !self.started || !self.gap.is_empty();
        if !self.gap.is_empty() {
            if !self.cut {
                shown.push_str(&self.gap);
            } else if self.gap.contains('\n') {
                shown.extend(self.gap.chars().filter(|c| matches!(c, '\n' | '\r')));
            } else if self.glued && self.opens {
                // `«[sigh] Привет»` shows `«Привет»`.
            } else if !matches!(ch, ',' | '.' | '!' | '?' | '…' | ':' | ';' | ')') {
                shown.push(' ');
            }
            self.gap.clear();
        }
        shown.push(ch);
        // A straight quote or emphasis opens only after whitespace or at the
        // start: `Готово"` and `слово*` close.
        self.opens = match ch {
            '(' | '«' | '„' | '“' => true,
            '"' | '*' | '_' => spaced || (self.opens && self.last == Some(ch)),
            _ => false,
        };
        self.last = Some(ch);
        self.started = true;
        self.cut = false;
        self.glued = false;
    }
}

/// Cuts voice marks out of streamed reply text and keeps the text as it came
/// for the voice.
#[derive(Debug, Default)]
pub struct MarkFilter {
    scan: Scanner,
    view: View,
}

impl MarkFilter {
    /// The part of the reply decided with this chunk.
    pub fn push(&mut self, chunk: &str) -> MarkPiece {
        let mut piece = MarkPiece::default();
        let Self { scan, view } = self;
        scan.push(chunk, &mut |found| view.take(found, &mut piece));
        piece
    }

    /// What is still held when the stream ends. An unclosed `[` or tag is
    /// shown as text; whitespace at the very end is dropped from the shown text.
    pub fn finish(&mut self) -> MarkPiece {
        let mut piece = MarkPiece::default();
        let Self { scan, view } = self;
        scan.finish(&mut |found| view.take(found, &mut piece));
        *self = Self::default();
        piece
    }
}

/// A whole text without voice marks.
pub fn strip_marks(text: &str) -> String {
    let mut filter = MarkFilter::default();
    let mut shown = filter.push(text).shown;
    shown.push_str(&filter.finish().shown);
    shown
}

/// A whole text split into plain text and marks, by the rules of `MarkFilter`.
pub fn tokens(text: &str) -> Vec<Token> {
    tokens_raw(text).into_iter().map(|(t, _)| t).collect()
}

/// `tokens` with the length of each in the text as it came, bytes: a
/// direction may carry the emphasis around it, `*[sigh]*`.
pub fn tokens_raw(text: &str) -> Vec<(Token, usize)> {
    let mut found: Vec<(Token, usize)> = Vec::new();
    let mut take = |f: Found<'_>| match f {
        Found::Text(t) => match found.last_mut() {
            Some((Token::Text(last), len)) => {
                last.push_str(t);
                *len += t.len();
            }
            _ => found.push((Token::Text(t.to_string()), t.len())),
        },
        Found::Direction { raw, inner } => {
            found.push((Token::Direction(inner.trim().to_string()), raw.len()))
        }
        Found::Tag { raw, name, close } => {
            let tag = if close {
                Token::Close(name)
            } else {
                Token::Open(name)
            };
            found.push((tag, raw.len()));
        }
    };
    let mut scan = Scanner::default();
    scan.push(text, &mut take);
    scan.finish(&mut take);
    found
}

/// Whether code is still open at the end of `text`: a code block, inline
/// code or a run of backticks not ended yet. Where none is open, the text
/// after it gives the same marks read from the start of a line, or after a
/// `.` when the line goes on.
pub fn code_open(text: &str) -> bool {
    let mut scan = Scanner::default();
    scan.push(text, &mut |_| {});
    scan.fence || scan.inline > 0 || scan.ticks > 0
}

/// Whether `text` ends at the start of a line inside a code block. The text
/// after it then gives the same marks read after ```` ```\n ````: a long code
/// block can be let go line by line.
pub fn in_code_block_line(text: &str) -> bool {
    let mut scan = Scanner::default();
    scan.push(text, &mut |_| {});
    scan.fence
        && !scan.fence_here
        && !scan.mid_line
        && scan.inline == 0
        && scan.ticks == 0
        && scan.hold == Hold::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shown text, spoken text and directions of a reply streamed in `chunks`.
    fn run(chunks: &[&str]) -> (String, String, Vec<String>) {
        let mut f = MarkFilter::default();
        let mut all = MarkPiece::default();
        let mut pieces: Vec<MarkPiece> = chunks.iter().map(|c| f.push(c)).collect();
        pieces.push(f.finish());
        for p in pieces {
            all.shown.push_str(&p.shown);
            all.spoken.push_str(&p.spoken);
            all.directions.extend(p.directions);
        }
        (all.shown, all.spoken, all.directions)
    }

    #[test]
    fn directions_and_tags_are_cut_from_the_shown_text_only() {
        let reply = "[sigh] Привет, <whisper>иди без меня</whisper>.";
        assert_eq!(
            run(&[reply]),
            (
                "Привет, иди без меня.".into(),
                reply.into(),
                vec!["sigh".to_string()]
            )
        );
    }

    #[test]
    fn no_double_space_and_no_space_before_punctuation_where_a_mark_was() {
        for (reply, shown) in [
            ("Ну [sigh], ладно", "Ну, ладно"),
            ("Готово. [wink] Ещё", "Готово. Ещё"),
            ("[sigh] Привет", "Привет"),
            ("  [sigh]\n Привет", "Привет"),
            ("Стой [pause]!", "Стой!"),
            ("Конец. [sigh]", "Конец."),
            ("Конец. [sigh]  ", "Конец."),
            ("Привет.\n[sigh] Ну", "Привет.\nНу"),
            ("Привет. [sigh]\n\nНу", "Привет.\n\nНу"),
            ("Да [sigh]нет", "Да нет"),
            ("Скажу <whisper> тихо </whisper>.", "Скажу тихо."),
            ("(смеётся [laughs])", "(смеётся)"),
            (
                "Без меток:  два пробела ,  и запятая",
                "Без меток:  два пробела ,  и запятая",
            ),
            // No space after an opening bracket, quote or emphasis the mark stuck to.
            ("«[sigh] Привет»", "«Привет»"),
            ("([sigh] тихо)", "(тихо)"),
            ("Он сказал: \"[whispering] да\"", "Он сказал: \"да\""),
            ("**[sigh] Важно**", "**Важно**"),
            ("«[sigh] [laughs] Ну»", "«Ну»"),
            ("« [sigh] Ну»", "« Ну»"),
            ("Готово.[sigh] Ещё", "Готово. Ещё"),
            ("Сказал \"да\"[sigh] потом", "Сказал \"да\" потом"),
            ("* [sigh] пункт", "* пункт"),
        ] {
            assert_eq!(strip_marks(reply), shown, "{reply}");
        }
    }

    #[test]
    fn emphasis_around_a_direction_goes_with_it() {
        for (reply, shown) in [
            ("*[sighs]* Ну ладно, давай так.", "Ну ладно, давай так."),
            ("*[sigh]* Ну", "Ну"),
            ("**[sigh]** Ну", "Ну"),
            ("_[sigh]_ Ну", "Ну"),
            ("Ну *[sigh]*, ладно", "Ну, ладно"),
            ("**[sigh]** Ладно, **это** важно", "Ладно, **это** важно"),
            ("Ну *[sigh]*", "Ну"),
            // Emphasis that does not close as it opened stays.
            ("**[sigh]* Ну", "*** Ну"),
            ("*[sigh] Ну*", "*Ну*"),
        ] {
            assert_eq!(strip_marks(reply), shown, "{reply}");
            let chars: Vec<String> = reply.chars().map(String::from).collect();
            let refs: Vec<&str> = chars.iter().map(String::as_str).collect();
            assert_eq!(run(&refs).0, shown, "{reply}, one character at a time");
            assert_eq!(run(&refs).1, reply, "{reply}, spoken as it came");
        }
        for text in [
            "a*[i]*b",
            "*[x](url)*",
            "*[xy](https://x.ru)*",
            "**жирный** и _курсив_ и snake_case и ***три***",
            "* [ ] пункт\n* [x] готово",
            "2 * [1, 2] = [2, 4]",
        ] {
            assert_eq!(strip_marks(text), text, "{text}");
        }
        // The voice gets the direction with the length of the whole mark.
        assert_eq!(
            tokens_raw("*[sigh]* Ну"),
            vec![
                (Token::Direction("sigh".into()), "*[sigh]*".len()),
                (Token::Text(" Ну".into()), " Ну".len()),
            ]
        );
    }

    #[test]
    fn a_direction_needs_words_not_names_or_lists() {
        assert!(is_direction("sigh"));
        assert!(is_direction("тихо, с грустью"));
        assert!(is_direction("softly. then louder"));
        assert!(is_direction("says \"hi\""));
        for inner in [
            "a, b",
            "x, y",
            "a b c",
            "файл.txt",
            "ya.ru",
            "т.е. тихо",
            "\"яблоко\", \"груша\"",
            " \"a\"",
            "",
            "2 раза",
        ] {
            assert!(!is_direction(inner), "{inner}");
        }
        assert_eq!(strip_marks("[softly. then louder] Ну"), "Ну");
        assert_eq!(strip_marks("[ab] Ну"), "Ну");
    }

    #[test]
    fn russian_and_english_directions_are_marks() {
        assert_eq!(
            run(&["[тихо, с грустью] Я тут. [barely holding back tears] Правда."]),
            (
                "Я тут. Правда.".into(),
                "[тихо, с грустью] Я тут. [barely holding back tears] Правда.".into(),
                vec![
                    "тихо, с грустью".to_string(),
                    "barely holding back tears".to_string()
                ]
            )
        );
        assert_eq!(
            run(&["[ Joy ]Ура"]).2,
            vec!["Joy".to_string()],
            "the inside is trimmed"
        );
    }

    #[test]
    fn bracketed_words_look_like_directions_and_are_cut() {
        // The price of directions in any language (decision 0026): names go in «ёлочки».
        for (reply, shown) in [
            ("Нажми [Сохранить].", "Нажми."),
            ("Нажми [Файл] и [Сохранить как].", "Нажми и."),
            ("Нажми «Сохранить».", "Нажми «Сохранить»."),
        ] {
            assert_eq!(strip_marks(reply), shown, "{reply}");
        }
    }

    #[test]
    fn ordinary_brackets_stay() {
        let long = format!("[{}]", "очень длинный текст ".repeat(5));
        for text in [
            "Смотри [1] и сноску[^1], [^2].",
            "[1]: https://x.ru",
            "- [ ] купить\n- [x] готово\n* [X] тоже",
            "[—] и […] и [ ] и []",
            "a[i], x[n-1] и массив[0]",
            "Читай [документацию](https://x.ru), там всё.",
            "[{\"name\": \"echo\", \"arguments\": {}}]",
            "[🤔] и [😊😊]",
            "[a] и [б]",
            "[две\nстроки]",
            "[snake_case] [x=1] [тихо;] [2 раза] [C:\\temp] [a/b] [<b>]",
            "Ну[sigh] да",
            "массив[0] = [a, b] и [x, y]",
            "Смотри [файл.txt] и [ya.ru].",
            "Вот список: [\"яблоко\", \"груша\"] и всё.",
            long.as_str(),
        ] {
            assert_eq!(strip_marks(text), text, "{text}");
            assert_eq!(tokens(text), vec![Token::Text(text.into())], "{text}");
        }
        let longest = format!("[{}]", "а".repeat(MAX_DIRECTION));
        assert_eq!(strip_marks(&format!("{longest} да")), "да");
        let too_long = format!("[{}] да", "а".repeat(MAX_DIRECTION + 1));
        assert_eq!(strip_marks(&too_long), too_long);
    }

    #[test]
    fn code_is_never_marked() {
        for text in [
            "Пиши `[joy]`, `a[i]` и `<whisper>`.",
            "Код ``<soft>[sigh]`` тут",
            "```html\n<whisper>тихо</whisper>\n[sigh] <br>\n```",
            "Код:\n  ```\n[sigh]\n  ```",
        ] {
            assert_eq!(strip_marks(text), text, "{text}");
        }
        assert_eq!(
            strip_marks("```\n[sigh]\n```\n[sigh] Готово `[sigh]`"),
            "```\n[sigh]\n```\nГотово `[sigh]`"
        );
        // Inline code ends with its line; three backticks inside a line open
        // inline code, not a block.
        assert_eq!(strip_marks("`a\n[sigh] b"), "`a\nb");
        assert_eq!(
            strip_marks("так ``` [sigh]\n[sigh] да"),
            "так ``` [sigh]\nда"
        );
        // A fence split across chunks still opens and closes the block.
        assert_eq!(
            run(&[
                "``",
                "`ht",
                "ml\n<whis",
                "per>x</whisper>\n`",
                "``\n<whisper>да</whisper>"
            ])
            .0,
            "```html\n<whisper>x</whisper>\n```\nда"
        );
    }

    #[test]
    fn a_block_opened_mid_line_or_after_a_list_marker_is_still_a_block() {
        for (opener, closer) in [
            ("Вот код: ```python", "```"),
            ("Шаги:\n- ```bash", "```"),
            ("1. ```sh", "   ```"),
            ("> ```", "> ```"),
        ] {
            let reply = format!("{opener}\nx = ['тихо', 'громко']\n{closer}\n[sigh] Готово");
            let shown = format!("{opener}\nx = ['тихо', 'громко']\n{closer}\nГотово");
            assert_eq!(strip_marks(&reply), shown, "{reply}");
            assert_eq!(
                tokens(&reply)[1..],
                [
                    Token::Direction("sigh".into()),
                    Token::Text(" Готово".into())
                ],
                "{reply}"
            );
            let chars: Vec<String> = reply.chars().map(String::from).collect();
            let refs: Vec<&str> = chars.iter().map(String::as_str).collect();
            assert_eq!(run(&refs).0, shown, "{reply}, one character at a time");
        }
        // Backticks on the line that opened a block make it inline code.
        for reply in [
            "```x``` [sigh] Вторая.",
            "Первая. ```x``` [sigh] Вторая.",
            "- ```x``` [sigh] Вторая.",
        ] {
            assert!(
                !strip_marks(reply).contains("[sigh]"),
                "{reply}: {}",
                strip_marks(reply)
            );
        }
        // Words after the backticks are not a language: still inline code.
        assert_eq!(strip_marks("так ``` и всё\n[sigh] да"), "так ``` и всё\nда");
    }

    #[test]
    fn open_code_is_known_at_the_end_of_a_text() {
        for open in ["`a", "```\nкод", "Вот ```py", "x ``", "```x"] {
            assert!(code_open(open), "{open}");
        }
        for closed in [
            "",
            "Текст.",
            "`a` и всё",
            "```\nкод\n```\n",
            "`a\nb",
            "Вот ```x``` тут",
        ] {
            assert!(!code_open(closed), "{closed}");
        }
    }

    #[test]
    fn only_known_tags_without_attributes_are_marks() {
        for text in [
            "a < b и a<b",
            "<3 и <- и <<",
            "Используй <br> для переноса",
            "Vec<String> и Option<Strong>",
            "<break/> и <pause /> и <whisper/>",
            "<whisper volume=\"low\"> и <emphasis level='strong'>",
            "< whisper> и </ whisper> и <whisperx> и <шёпот>",
            "<https://ya.ru> и <user@mail.ru>",
        ] {
            assert_eq!(strip_marks(text), text, "{text}");
        }
        assert_eq!(strip_marks("<WHISPER>тихо</Whisper>"), "тихо");
        assert_eq!(strip_marks("<french>bonjour</french>!"), "bonjour!");
        assert_eq!(strip_marks("<<whisper>"), "<");
        assert_eq!(
            tokens("<Stress>да</STRESS>"),
            vec![
                Token::Open("stress"),
                Token::Text("да".into()),
                Token::Close("stress")
            ]
        );
    }

    #[test]
    fn nested_and_mixed_marks_keep_their_words() {
        for (reply, shown, directions) in [
            ("<whisper><emphasis>нет</emphasis></whisper>", "нет", vec![]),
            (
                "[sigh] <whisper>ну ладно</whisper>",
                "ну ладно",
                vec!["sigh"],
            ),
            ("<whisper>[sad] пока</whisper>", "пока", vec!["sad"]),
            ("[sigh][laughs] Ха", "Ха", vec!["sigh", "laughs"]),
            ("[[joy] да", "[ да", vec!["joy"]),
            ("[[joy", "[[joy", vec![]),
        ] {
            let (s, spoken, d) = run(&[reply]);
            assert_eq!((s.as_str(), spoken.as_str()), (shown, reply), "{reply}");
            assert_eq!(d, directions, "{reply}");
        }
    }

    #[test]
    fn unclosed_marks_at_the_end() {
        assert_eq!(run(&["итог: [jo"]).0, "итог: [jo");
        assert_eq!(run(&["Иди <whis"]).0, "Иди <whis");
        assert_eq!(run(&["<whisper>Иди без меня."]).0, "Иди без меня.");
        assert_eq!(run(&["Пока.</whisper>"]).0, "Пока.");
        // A closed direction waiting for the next character is a mark at the end.
        assert_eq!(
            run(&["Пока. [sigh]"]),
            (
                "Пока.".into(),
                "Пока. [sigh]".into(),
                vec!["sigh".to_string()]
            )
        );
    }

    #[test]
    fn a_reply_of_marks_only_shows_nothing() {
        for reply in ["[sigh]", "<whisper></whisper>", " [pause] \n", "[joy]"] {
            let (shown, spoken, _) = run(&[reply]);
            assert_eq!((shown.as_str(), spoken.as_str()), ("", reply), "{reply}");
        }
    }

    #[test]
    fn a_link_is_decided_by_the_next_chunk() {
        let mut f = MarkFilter::default();
        let first = f.push("Смотри [доку]");
        assert_eq!(
            (first.shown.as_str(), first.spoken.as_str()),
            ("Смотри", "Смотри ")
        );
        let second = f.push("(https://x.ru).");
        assert_eq!(second.shown, " [доку](https://x.ru).");
        assert_eq!(second.spoken, "[доку](https://x.ru).");
        assert!(second.directions.is_empty());

        let mut f = MarkFilter::default();
        f.push("Ну [sigh]");
        let next = f.push(", ладно");
        assert_eq!(next.shown, ", ладно");
        assert_eq!(next.spoken, "[sigh], ладно");
        assert_eq!(next.directions, vec!["sigh".to_string()]);
    }

    #[test]
    fn spoken_text_is_the_reply_as_it_came() {
        let mut f = MarkFilter::default();
        let a = f.push("[sigh] <whi");
        assert_eq!((a.shown.as_str(), a.spoken.as_str()), ("", "[sigh] "));
        let b = f.push("sper>Тсс</whisper> ");
        assert_eq!(b.shown, "Тсс");
        assert_eq!(b.spoken, "<whisper>Тсс</whisper> ");
        let c = f.finish();
        assert_eq!((c.shown.as_str(), c.spoken.as_str()), ("", ""));
    }

    #[test]
    fn a_reply_split_anywhere_gives_the_same_result() {
        let reply = "[joy] Привет! [тихо, с улыбкой] Вот <whisper>секрет</whisper>, смотри \
[документацию](https://x.ru) и [1].\n- [ ] пункт\n- [x] готово\n\
Код: `a[i] <soft>` и Vec<String>, a < b.\n```html\n<whisper>[sigh]</whisper>\n```\n\
Ну [sigh], ладно [🤔] <EMPHASIS>да</emphasis> [{\"name\": 1}] итог: [jo";
        let whole = run(&[reply]);
        assert_eq!(
            whole.0,
            "Привет! Вот секрет, смотри [документацию](https://x.ru) и [1].\n\
- [ ] пункт\n- [x] готово\nКод: `a[i] <soft>` и Vec<String>, a < b.\n\
```html\n<whisper>[sigh]</whisper>\n```\nНу, ладно [🤔] да [{\"name\": 1}] итог: [jo"
        );
        assert_eq!(whole.1, reply);
        assert_eq!(whole.2, vec!["joy", "тихо, с улыбкой", "sigh"]);

        let chars: Vec<char> = reply.chars().collect();
        let one_by_one: Vec<String> = chars.iter().map(char::to_string).collect();
        let refs: Vec<&str> = one_by_one.iter().map(String::as_str).collect();
        assert_eq!(run(&refs), whole, "one character at a time");

        let bounds: Vec<usize> = reply.char_indices().map(|(i, _)| i).collect();
        for &at in &bounds {
            assert_eq!(run(&[&reply[..at], &reply[at..]]), whole, "split at {at}");
        }

        // Chunks of uneven sizes, from a few starting points.
        for seed in 1..=20u32 {
            let mut state = seed;
            let mut chunks = Vec::new();
            let mut rest = reply;
            while !rest.is_empty() {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let n = (state >> 16) as usize % 9 + 1;
                let cut = rest.char_indices().nth(n).map_or(rest.len(), |(i, _)| i);
                chunks.push(&rest[..cut]);
                rest = &rest[cut..];
            }
            assert_eq!(run(&chunks), whole, "seed {seed}");
        }
    }

    #[test]
    fn tokens_follow_the_same_rules() {
        assert_eq!(
            tokens("[sigh] Ну <whisper>тихо</whisper> `[x]` [ закрой ]"),
            vec![
                Token::Direction("sigh".into()),
                Token::Text(" Ну ".into()),
                Token::Open("whisper"),
                Token::Text("тихо".into()),
                Token::Close("whisper"),
                Token::Text(" `[x]` ".into()),
                Token::Direction("закрой".into()),
            ]
        );
        assert_eq!(
            tokens("[{\"name\": \"echo\"}] [Ссылка](https://x.ru)"),
            vec![Token::Text(
                "[{\"name\": \"echo\"}] [Ссылка](https://x.ru)".into()
            )]
        );
        assert!(tokens("").is_empty());
    }

    #[test]
    fn the_filter_starts_anew_after_finish() {
        let mut f = MarkFilter::default();
        f.push("```\n<whisper>");
        f.finish();
        assert_eq!(f.push(" <whisper>да").shown, "да");
    }
}
