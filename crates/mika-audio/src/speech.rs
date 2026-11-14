//! Turns the reply stream into sentences and hands them to a voice.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use mika_core::emotion::Emotion;
use mika_core::events::{Event, EventBus};
use mika_core::marks::{
    code_open, in_code_block_line, is_direction, strip_marks, tokens, tokens_raw, MarkFilter, Token,
};
use mika_core::schedule::ReminderKind;
use mika_core::state::AssistantState;
use parking_lot::Mutex;
use regex::Regex;
use serde::Serialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Notify;

/// A sentence shorter than this waits for the next one: "т. е." is not a sentence.
const MIN_SENTENCE: usize = 12;
/// A reply without any sentence end is cut here, so speech does not wait for the whole text.
const MAX_SENTENCE: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VoiceInfo {
    pub name: String,
    /// Language tag: `ru-RU`.
    pub language: String,
}

/// Makes the sound of a sentence somewhere else, for example at a speech provider.
/// Blocks until the audio file is ready. Runs on its own thread, never on an async worker.
pub type RemoteSynth = Arc<dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync>;

/// Something that can speak. Sentences queue up; `stop` drops the queue at once.
pub trait Voice: Send + Sync {
    /// Says text that is not from the model: notifications, reminders,
    /// errors. A remote synthesizer that performs voice marks gets it with
    /// its brackets made harmless (decision 0026).
    fn speak(&self, text: &str);
    /// Says words of the model without voice marks: a remote synthesizer
    /// gets them as they are, as before decision 0026. A voice that wraps
    /// another must pass it on.
    fn speak_reply(&self, text: &str) {
        self.speak(text);
    }
    /// Says a phrase of a reply with voice marks (decision 0026): a remote
    /// synthesizer that performs them gets `marked`, any other voice `plain`,
    /// the same words without marks. A voice that wraps another must pass it
    /// on, or the marks are lost.
    fn speak_marked(&self, _marked: &str, plain: &str) {
        self.speak(plain);
    }
    fn stop(&self);
    fn voices(&self) -> Vec<VoiceInfo>;
    /// Picks the voice by name (empty: one of the language she replies in, if installed) and the rate, 0.5..=2.0.
    fn configure(&self, voice: &str, rate: f32);
    /// With a remote synthesizer the sound of each sentence comes from it; a sentence
    /// it fails on is read by the local voice. `None` returns to the local voice.
    fn set_remote(&self, _remote: Option<RemoteSynth>) {}
    /// Called with the milliseconds from "say this" to the first sound, once for
    /// the first sentence after each `stop`.
    fn set_observer(&self, _observer: Option<FirstSound>) {}
    /// True while queued sentences are still to be said.
    fn busy(&self) -> bool {
        false
    }
    /// Pitch and pace in percent for each new sentence of the local voice, so
    /// her tone follows her mood. `None`: always the usual voice.
    fn set_tone(&self, _tone: Option<ToneSource>) {}
    /// The direction a remote synthesizer gets before a sentence that has no
    /// voice marks of its own. `None`: never any. A voice that wraps another
    /// must pass it on.
    fn set_direction(&self, _direction: Option<DirectionSource>) {}
}

/// Pitch and pace in percent of the usual, asked for each sentence.
pub type ToneSource = Arc<dyn Fn() -> (i8, i8) + Send + Sync>;

/// Her mood as a direction for a voice that performs marks, asked for each
/// sentence (decision 0029). `None`: no direction now.
pub type DirectionSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// SSML that says `text` with the pitch and pace changed by these percents.
pub fn prosody_ssml(text: &str, language: &str, (pitch, rate): (i8, i8)) -> String {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;");
    let lang = if language.trim().is_empty() {
        "ru-RU"
    } else {
        language.trim()
    };
    format!(
        "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"{lang}\">\
<prosody pitch=\"{pitch:+}%\" rate=\"{rate:+}%\">{escaped}</prosody></speak>"
    )
}

/// Gets the time to the first sound of a spoken answer.
pub type FirstSound = Arc<dyn Fn(u64) + Send + Sync>;

/// Splits streamed text into sentences.
///
/// One made by [`SentenceSplitter::marked`] reads a reply with voice marks
/// (decision 0026). A phrase never ends inside a mark. A paired tag still
/// open at the end of a phrase is closed there and opened again in the next
/// one. A direction lasts to the end of its line, so a phrase of the same
/// line that has none of its own starts with it again, unless it is a sound
/// said once, like `[sigh]`. Lengths count only the characters of the text.
/// Marks with no words after them wait for the next phrase and are dropped
/// at the end. In its phrases square brackets of the text become round, its
/// angle brackets and backticks go: marks and code are found on the whole
/// reply, and a phrase read alone must give `tokens` the same marks and
/// nothing else.
#[derive(Debug, Default)]
pub struct SentenceSplitter {
    buf: String,
    /// Set for a reply with voice marks, which it keeps itself.
    marks: Option<Marked>,
}

/// What a splitter of a reply with voice marks knows.
#[derive(Debug, Default)]
struct Marked {
    /// The reply as it came, from its start or from a point where the marks
    /// are found as from a start: see `forget`.
    text: String,
    /// Tells where the marks end: what it still holds may become one.
    filter: MarkFilter,
    /// Bytes at the start of `text` whose marks are decided.
    decided: usize,
    /// Items at the start of `text` already given out in phrases.
    said: usize,
    /// Tags open where the next phrase starts.
    open: Vec<&'static str>,
    /// The last direction of the line where the next phrase starts.
    direction: Option<String>,
}

/// Sounds said once, not a manner of speech: a direction of them, `[sigh]`
/// or `[long pause]`, is not said again in the next phrase of its line.
const ONE_OFF: [&str; 25] = [
    "sigh",
    "sighs",
    "laugh",
    "laughs",
    "laughing",
    "chuckle",
    "chuckles",
    "giggle",
    "giggles",
    "pause",
    "cough",
    "coughs",
    "gasp",
    "gasps",
    "breath",
    "inhale",
    "exhale",
    "sob",
    "sobs",
    "sniff",
    "sniffs",
    "вздох",
    "вздыхает",
    "смеётся",
    "пауза",
];

/// Whether a direction goes on into the next phrase of its line: not a
/// sound said once and not an emotion tag like `[joy]`.
fn lasting(direction: &str) -> bool {
    let d = direction.trim().to_lowercase();
    let words: Vec<&str> = d
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphabetic()))
        .collect();
    let sound = words.len() <= 2 && words.last().is_some_and(|w| ONE_OFF.contains(w));
    !sound && Emotion::from_tag(&d).is_none()
}

/// A character of the text or a whole voice mark.
#[derive(Debug, Clone)]
enum Item {
    Char(char),
    /// A mark and its length in the text as it came, bytes.
    Mark(Token, usize),
}

impl Item {
    /// Length in the text as it came, bytes.
    fn raw_len(&self) -> usize {
        match self {
            Item::Char(c) => c.len_utf8(),
            Item::Mark(_, len) => *len,
        }
    }
}

/// `text` as characters and whole marks. The marks are found from the start
/// of `text`, so code blocks are known wherever a phrase begins.
fn items(text: &str) -> Vec<Item> {
    let mut out = Vec::new();
    for (token, len) in tokens_raw(text) {
        match token {
            Token::Text(t) => out.extend(t.chars().map(Item::Char)),
            mark => out.push(Item::Mark(mark, len)),
        }
    }
    out
}

/// End of the first phrase of `items` that has words and is long enough, by
/// the rules of `SentenceSplitter::boundary` with only the text counted.
/// Closing tags right after the end of a sentence belong to it.
fn marked_boundary(items: &[Item]) -> Option<usize> {
    let mut count = 0usize;
    let mut words = false;
    for (i, item) in items.iter().enumerate() {
        let &Item::Char(c) = item else { continue };
        count += 1;
        words |= c.is_alphanumeric();
        let mut end = i + 1;
        let stop = match c {
            '\n' => true,
            '.' | '!' | '?' | '…' => {
                while matches!(items.get(end), Some(Item::Mark(Token::Close(_), _))) {
                    end += 1;
                }
                matches!(items.get(end), Some(Item::Char(n)) if n.is_whitespace())
            }
            _ => false,
        };
        if words && stop && (count >= MIN_SENTENCE || c == '\n') {
            return Some(end);
        }
        if words && count >= MAX_SENTENCE && c.is_whitespace() {
            return Some(i + 1);
        }
    }
    None
}

impl Marked {
    fn push(&mut self, piece: &str) -> Vec<String> {
        self.text.push_str(piece);
        self.decided += self.filter.push(piece).spoken.len();
        let items = items(&self.text[..self.decided]);
        debug_assert_eq!(items.iter().map(Item::raw_len).sum::<usize>(), self.decided);
        let mut out = Vec::new();
        let start = self.said;
        while let Some(end) = marked_boundary(&items[self.said..]) {
            let end = self.said + end;
            out.extend(self.phrase(&items[self.said..end]));
            self.said = end;
        }
        if self.said > start {
            self.forget(&items[..self.said]);
        }
        out
    }

    /// The rest of the reply. Ready for a new reply afterwards.
    fn finish(&mut self) -> Option<String> {
        let items = items(&self.text);
        let phrase = self.phrase(items.get(self.said..).unwrap_or_default());
        *self = Self::default();
        phrase
    }

    /// Drops the text already said, so a long reply is not read again from
    /// its start at every piece. It is dropped only where no code is open:
    /// the marks of the rest are then found as from a start. A dropped line
    /// that has not ended leaves `.` in its place, so backticks that follow on
    /// that line are not taken for the start of a code block.
    fn forget(&mut self, said: &[Item]) {
        let cut: usize = said.iter().map(Item::raw_len).sum();
        let stand_in =
            if said.iter().any(|i| matches!(i, Item::Char('`'))) && code_open(&self.text[..cut]) {
                // Inside a code block at the start of a line, the block is
                // opened again in place of what was said; elsewhere in code
                // nothing is dropped.
                if !in_code_block_line(&self.text[..cut]) {
                    return;
                }
                "```\n"
            } else if matches!(said.last(), Some(Item::Char('\n'))) {
                ""
            } else {
                "."
            };
        self.text.replace_range(..cut, stand_in);
        self.decided = self.decided - cut + stand_in.len();
        self.said = stand_in.len();
    }

    /// A phrase of these items. Tags left open by the last phrase are opened
    /// again, tags open at its end are closed. The direction of its line
    /// comes first, unless a line break or a direction of its own comes
    /// before its words. `None` when it has no words.
    fn phrase(&mut self, items: &[Item]) -> Option<String> {
        let reopen: Vec<Token> = self.open.iter().copied().map(Token::Open).collect();
        let carried = self.direction.take().filter(|d| {
            lasting(d)
                && items
                    .iter()
                    .take_while(|i| !matches!(i, Item::Char(c) if c.is_alphanumeric()))
                    .all(|i| !matches!(i, Item::Char('\n') | Item::Mark(Token::Direction(_), _)))
        });
        self.direction.clone_from(&carried);
        let mut tags = Tags {
            open: std::mem::take(&mut self.open),
        };
        let mut body = Vec::new();
        let mut words = false;
        for item in items {
            match item {
                Item::Char('\n') => self.direction = None,
                Item::Mark(Token::Direction(d), _) => self.direction = Some(d.clone()),
                _ => {}
            }
            match item {
                Item::Char(c) => {
                    words |= c.is_alphanumeric();
                    // Code was found on the whole reply: its backticks have
                    // done their job and are never read out.
                    let Some(c) = defuse(*c).filter(|c| *c != '`') else {
                        continue;
                    };
                    match body.last_mut() {
                        Some(Token::Text(t)) => t.push(c),
                        _ => body.push(Token::Text(c.to_string())),
                    }
                }
                Item::Mark(mark, _) => tags.take(mark.clone(), &mut body),
            }
        }
        let close = tags.closers();
        self.open = tags.open;
        let lead = carried.map(|d| format!("[{d}] ")).unwrap_or_default();
        words.then(|| {
            format!(
                "{lead}{}{}{}",
                render(&reopen),
                render(&body).trim(),
                render(&close)
            )
        })
    }
}

/// Paired tags open in text with voice marks, kept balanced.
#[derive(Debug, Default)]
struct Tags {
    open: Vec<&'static str>,
}

impl Tags {
    /// Adds `token` to `out`. A closing tag with no opening one is dropped.
    /// One that closes an outer tag first closes the tags inside it and opens
    /// them again after it.
    fn take(&mut self, token: Token, out: &mut Vec<Token>) {
        match token {
            Token::Open(name) => {
                self.open.push(name);
                out.push(Token::Open(name));
            }
            Token::Close(name) => {
                let Some(at) = self.open.iter().rposition(|t| *t == name) else {
                    return;
                };
                let inner = self.open.split_off(at + 1);
                self.open.pop();
                out.extend(inner.iter().rev().copied().map(Token::Close));
                out.push(Token::Close(name));
                out.extend(inner.iter().copied().map(Token::Open));
                self.open.extend(inner);
            }
            other => out.push(other),
        }
    }

    /// Closing tags for all that are open, innermost first.
    fn closers(&self) -> Vec<Token> {
        self.open.iter().rev().copied().map(Token::Close).collect()
    }
}

/// Text with voice marks written from tokens, so that `tokens` finds the
/// same marks in it again: a direction never sticks to a word before it or
/// to a `(` after it.
fn render(tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut after_direction = false;
    for token in tokens {
        match token {
            Token::Text(t) if t.is_empty() => continue,
            Token::Text(t) => {
                if after_direction && t.starts_with('(') {
                    out.push(' ');
                }
                out.push_str(t);
            }
            Token::Direction(d) => {
                if out.ends_with(char::is_alphanumeric) {
                    out.push(' ');
                }
                out.push('[');
                out.push_str(d);
                out.push(']');
            }
            Token::Open(name) => {
                out.push('<');
                out.push_str(name);
                out.push('>');
            }
            Token::Close(name) => {
                out.push_str("</");
                out.push_str(name);
                out.push('>');
            }
        }
        after_direction = matches!(token, Token::Direction(_));
    }
    out
}

/// A character of text that is not a voice mark, as a synthesizer that
/// performs marks should get it: Fish S2 and Drama 3 would take words in
/// square brackets for a direction and swallow them, so the brackets become
/// round ones, and angle brackets go (decision 0026).
pub(crate) fn defuse(c: char) -> Option<char> {
    match c {
        '[' => Some('('),
        ']' => Some(')'),
        '<' | '>' => None,
        c => Some(c),
    }
}

/// A splitter for a reply with voice marks when `marked`, otherwise plain.
fn splitter(marked: bool) -> SentenceSplitter {
    if marked {
        SentenceSplitter::marked()
    } else {
        SentenceSplitter::default()
    }
}

impl SentenceSplitter {
    /// A splitter for a reply with voice marks.
    pub fn marked() -> Self {
        Self {
            buf: String::new(),
            marks: Some(Marked::default()),
        }
    }

    /// Whether it reads a reply with voice marks.
    pub fn is_marked(&self) -> bool {
        self.marks.is_some()
    }

    /// Byte index just past the first sentence that is long enough, if any.
    fn boundary(&self) -> Option<usize> {
        let mut chars = self.buf.char_indices().peekable();
        let mut count = 0usize;
        while let Some((i, c)) = chars.next() {
            count += 1;
            let end = i + c.len_utf8();
            let next = chars.peek().map(|(_, n)| *n);
            let stop = match c {
                '\n' => true,
                '.' | '!' | '?' | '…' => next.is_some_and(char::is_whitespace),
                _ => false,
            };
            if stop && (count >= MIN_SENTENCE || c == '\n') && !self.buf[..end].trim().is_empty() {
                return Some(end);
            }
            if count >= MAX_SENTENCE && c.is_whitespace() {
                return Some(end);
            }
        }
        None
    }

    /// Adds a piece of the reply and returns the sentences it completed.
    pub fn push(&mut self, piece: &str) -> Vec<String> {
        if let Some(marks) = self.marks.as_mut() {
            return marks.push(piece);
        }
        self.buf.push_str(piece);
        let mut out = Vec::new();
        while let Some(end) = self.boundary() {
            let sentence: String = self.buf.drain(..end).collect();
            let sentence = sentence.trim();
            if !sentence.is_empty() {
                out.push(sentence.to_string());
            }
        }
        out
    }

    /// The text left when the reply ends.
    pub fn finish(&mut self) -> Option<String> {
        if let Some(marks) = self.marks.as_mut() {
            return marks.finish();
        }
        let rest = std::mem::take(&mut self.buf);
        let rest = rest.trim();
        (!rest.is_empty()).then(|| rest.to_string())
    }
}

/// Emoji and pictographs: a synthesizer reads their names or stumbles.
fn emoji(c: char) -> bool {
    let u = c as u32;
    (0x1F000..=0x1FAFF).contains(&u)
        || (0x2600..=0x27BF).contains(&u)
        || (0xFE00..=0xFE0F).contains(&u)
        || u == 0x200D
}

/// Whitespace runs as one space, none at the ends.
fn collapse_spaces(text: &str) -> String {
    static SPACES: OnceLock<Regex> = OnceLock::new();
    let spaces = SPACES.get_or_init(|| Regex::new(r"\s+").unwrap());
    spaces.replace_all(text.trim(), " ").into_owned()
}

/// Whitespace runs as one space, or as one line break where they hold one,
/// none at the ends: a direction lasts to the end of its line (decision 0026).
fn collapse_lines(text: &str) -> String {
    static BREAKS: OnceLock<Regex> = OnceLock::new();
    static SPACES: OnceLock<Regex> = OnceLock::new();
    let breaks = BREAKS.get_or_init(|| Regex::new(r"\s*\n\s*").unwrap());
    let spaces = SPACES.get_or_init(|| Regex::new(r"[^\S\n]+").unwrap());
    let lines = breaks.replace_all(text.trim(), "\n");
    spaces.replace_all(&lines, " ").into_owned()
}

/// `clean_for_speech` with the spacing left as it is.
fn speech_chars(text: &str) -> String {
    static URL: OnceLock<Regex> = OnceLock::new();
    let url = URL.get_or_init(|| Regex::new(r"https?://\S+").unwrap());
    url.replace_all(text, "ссылка")
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '#' | '_' | '~' | '|' | '>' | '<'))
        .filter(|c| !emoji(*c))
        .collect()
}

/// Text as it should be read aloud: no markup, no links, no emoji.
pub fn clean_for_speech(text: &str) -> String {
    collapse_spaces(&speech_chars(text))
}

/// Text with voice marks as it should be said (decision 0026). The text
/// between the marks is cleaned as by `clean_for_speech`, and its own
/// brackets are made harmless; line breaks stay, since a direction ends with
/// its line. Directions keep their words, paired tags are balanced and empty
/// pairs dropped. Empty when no words are left outside the marks: marks
/// alone are not worth a request.
pub fn clean_marked_for_speech(text: &str) -> String {
    let mut tags = Tags::default();
    let mut balanced = Vec::new();
    for token in tokens(text) {
        let token = match token {
            Token::Text(t) => Token::Text(speech_chars(&t).chars().filter_map(defuse).collect()),
            Token::Direction(d) => {
                let d: String = d.chars().filter(|c| !emoji(*c)).collect();
                let d = collapse_spaces(&d);
                if d.is_empty() {
                    continue;
                }
                Token::Direction(d)
            }
            tag => tag,
        };
        tags.take(token, &mut balanced);
    }
    balanced.extend(tags.closers());
    // A pair with nothing but whitespace inside does nothing.
    let mut kept: Vec<Token> = Vec::new();
    for token in balanced {
        if let Token::Close(name) = token {
            let last = kept
                .iter()
                .rposition(|t| !matches!(t, Token::Text(s) if s.trim().is_empty()));
            if let Some(at) = last.filter(|at| kept[*at] == Token::Open(name)) {
                kept.remove(at);
                continue;
            }
        }
        kept.push(token);
    }
    let words = kept
        .iter()
        .any(|t| matches!(t, Token::Text(s) if s.chars().any(char::is_alphanumeric)));
    if !words {
        return String::new();
    }
    collapse_lines(&render(&kept))
}

/// A reply phrase with voice marks made ready for the voice: with the marks
/// and without them. `None` when it has no words to say.
fn marked_speech(text: &str) -> Option<(String, String)> {
    let marked = clean_marked_for_speech(text);
    let plain = clean_for_speech(&strip_marks(text));
    (!marked.is_empty() && !plain.is_empty()).then_some((marked, plain))
}

/// Whether anyone can hear her now: false while the system sound is muted.
pub type Audible = Arc<dyn Fn() -> bool + Send + Sync>;

/// Makes a short retelling of a long reply for the voice; `None` when it fails.
/// With `true` the reply carries voice marks (decision 0026), and the
/// retelling may keep its tone with the same marks.
pub type Summarize =
    Arc<dyn Fn(String, bool) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

/// A reply longer than this, characters, is not read out when retelling is on.
pub const LONG_REPLY: usize = 300;

/// The "speak replies" switch shared with the settings, and how long replies sound.
#[derive(Clone, Default)]
pub struct SpeakerSwitch {
    on: Arc<AtomicBool>,
    audible: Option<Audible>,
    /// Long replies are retold briefly instead of read out.
    brief: Arc<AtomicBool>,
    summarize: Option<Summarize>,
    /// A lock, so a stop never slips between the check and the queueing of a
    /// retelling.
    retellings: Arc<Mutex<Retellings>>,
    /// Wakes the retellings being made when they are dropped: they stop
    /// waiting for the model at once.
    dropped: Arc<Notify>,
}

/// Retellings being made and which of them still count.
#[derive(Default)]
struct Retellings {
    /// Grows on every stop and new message: a retelling asked for before it
    /// is dropped.
    generation: u64,
    /// Retellings of this generation not yet said.
    pending: usize,
}

impl SpeakerSwitch {
    pub fn new(on: bool) -> Self {
        Self {
            on: Arc::new(AtomicBool::new(on)),
            ..Default::default()
        }
    }

    /// Speech is not made while `audible` says nobody would hear it: a paid
    /// synthesizer is not asked for a reply that plays into silence.
    pub fn with_audible(mut self, audible: Audible) -> Self {
        self.audible = Some(audible);
        self
    }

    /// Who retells a long reply. Without it she says its first sentence.
    pub fn with_summarizer(mut self, summarize: Summarize) -> Self {
        self.summarize = Some(summarize);
        self
    }

    pub fn set(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
    }

    pub fn get(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    pub fn set_brief(&self, on: bool) {
        self.brief.store(on, Ordering::Relaxed);
    }

    pub fn brief(&self) -> bool {
        self.brief.load(Ordering::Relaxed)
    }

    /// Whether anyone would hear her now: false while the system sound is off.
    pub fn heard(&self) -> bool {
        self.audible.as_ref().is_none_or(|a| a())
    }

    /// Drops the retellings still being made: «Стоп», a new message, the
    /// microphone opening.
    /// They stop counting as speech at once, not when the model answers.
    pub fn cancel(&self) {
        {
            let mut r = self.retellings.lock();
            r.generation += 1;
            r.pending = 0;
        }
        self.dropped.notify_waiters();
    }

    /// True while a retelling is being made: she has not finished speaking.
    pub fn retelling(&self) -> bool {
        self.retellings.lock().pending > 0
    }

    /// `voice` as the rest of the app should use it: a retelling still being
    /// made counts as speech, so nobody listens or turns other programs up
    /// over it, and `stop` drops it too.
    pub fn wrap(&self, voice: Arc<dyn Voice>) -> Arc<dyn Voice> {
        Arc::new(Retold {
            inner: voice,
            switch: self.clone(),
        })
    }

    /// Says a short retelling of a long reply once it is made, unless she was
    /// stopped meanwhile. Until then she counts as speaking. A `marked` reply
    /// carries voice marks, and so may its retelling.
    fn retell(&self, voice: Arc<dyn Voice>, reply: String, marked: bool) {
        // Nobody would hear it: the model is not asked either.
        if !self.heard() {
            return;
        }
        let pending = Pending::start(&self.retellings);
        let switch = self.clone();
        tokio::spawn(async move {
            let dropped = switch.dropped.clone();
            let dropped = dropped.notified();
            tokio::pin!(dropped);
            // A stop before `notified` was made does not wake it.
            if !pending.current() {
                return;
            }
            let summary = async {
                match switch.summarize.clone() {
                    Some(s) => s(reply.clone(), marked).await,
                    None => None,
                }
            };
            let short = tokio::select! {
                short = summary => short,
                () = &mut dropped => return,
            };
            let said = match short {
                Some(s) if marked => marked_speech(&s).map(|(m, p)| Utterance::Marked(m, p)),
                Some(s) => Some(clean_for_speech(&s))
                    .filter(|s| !s.is_empty())
                    .map(Utterance::Plain),
                None => None,
            };
            let said = said.unwrap_or_else(|| {
                if marked {
                    let (m, p) = fallback_brief_marked(&reply);
                    Utterance::Marked(m, p)
                } else {
                    Utterance::Plain(fallback_brief(&reply))
                }
            });
            let wanted = switch.get() && switch.heard();
            let r = switch.retellings.lock();
            if wanted && r.generation == pending.generation {
                said.speak(voice.as_ref());
            }
            drop(r);
            // Let go only after the retelling is queued: the voice is busy by then.
            drop(pending);
        });
    }
}

/// A retelling ready for the voice.
enum Utterance {
    Plain(String),
    /// With voice marks, and the same words without them.
    Marked(String, String),
}

impl Utterance {
    fn speak(&self, voice: &dyn Voice) {
        match self {
            // Words of the model, retold or cut short.
            Utterance::Plain(text) => voice.speak_reply(text),
            Utterance::Marked(marked, plain) => voice.speak_marked(marked, plain),
        }
    }
}

/// Counts one retelling until it is said or dropped.
struct Pending {
    retellings: Arc<Mutex<Retellings>>,
    generation: u64,
}

impl Pending {
    fn start(retellings: &Arc<Mutex<Retellings>>) -> Self {
        let mut r = retellings.lock();
        r.pending += 1;
        Self {
            retellings: retellings.clone(),
            generation: r.generation,
        }
    }

    /// Whether no stop came since it was asked for.
    fn current(&self) -> bool {
        self.retellings.lock().generation == self.generation
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        let mut r = self.retellings.lock();
        // A stop has already let go of the retellings of older generations.
        if r.generation == self.generation {
            r.pending = r.pending.saturating_sub(1);
        }
    }
}

/// The voice made by [`SpeakerSwitch::wrap`].
struct Retold {
    inner: Arc<dyn Voice>,
    switch: SpeakerSwitch,
}

impl Voice for Retold {
    fn speak(&self, text: &str) {
        self.inner.speak(text);
    }

    fn speak_reply(&self, text: &str) {
        self.inner.speak_reply(text);
    }

    fn speak_marked(&self, marked: &str, plain: &str) {
        self.inner.speak_marked(marked, plain);
    }

    fn stop(&self) {
        // The retelling first: once dropped, it cannot reach the voice after the stop.
        self.switch.cancel();
        self.inner.stop();
    }

    fn voices(&self) -> Vec<VoiceInfo> {
        self.inner.voices()
    }

    fn configure(&self, voice: &str, rate: f32) {
        self.inner.configure(voice, rate);
    }

    fn set_remote(&self, remote: Option<RemoteSynth>) {
        self.inner.set_remote(remote);
    }

    fn set_observer(&self, observer: Option<FirstSound>) {
        self.inner.set_observer(observer);
    }

    fn busy(&self) -> bool {
        self.switch.retelling() || self.inner.busy()
    }

    fn set_tone(&self, tone: Option<ToneSource>) {
        self.inner.set_tone(tone);
    }

    fn set_direction(&self, direction: Option<DirectionSource>) {
        self.inner.set_direction(direction);
    }
}

/// What she says instead of a long reply when no retelling came: its first
/// sentence and a pointer to the chat.
pub fn fallback_brief(reply: &str) -> String {
    let mut s = SentenceSplitter::default();
    let first = s
        .push(reply)
        .into_iter()
        .map(|x| clean_for_speech(&x))
        .find(|x| !x.is_empty())
        .or_else(|| s.finish().map(|x| clean_for_speech(&x)))
        .unwrap_or_default();
    let first: String = first.chars().take(LONG_REPLY).collect();
    if first.is_empty() {
        "Ответ в чате.".into()
    } else {
        format!("{first} Подробности в чате.")
    }
}

/// `fallback_brief` of a reply with voice marks: its first phrase with the
/// marks, tags closed, and the same words without them.
pub fn fallback_brief_marked(reply: &str) -> (String, String) {
    let mut s = SentenceSplitter::marked();
    let first = s
        .push(reply)
        .into_iter()
        .chain(s.finish())
        .find_map(|p| marked_speech(&p));
    let Some((marked, plain)) = first else {
        return ("Ответ в чате.".into(), "Ответ в чате.".into());
    };
    let plain: String = plain.chars().take(LONG_REPLY).collect();
    (
        format!("{} Подробности в чате.", take_words(&marked, LONG_REPLY)),
        format!("{plain} Подробности в чате."),
    )
}

/// Text with voice marks cut after `n` characters of its text, tags closed.
fn take_words(marked: &str, n: usize) -> String {
    let mut left = n;
    let mut kept = Vec::new();
    for token in tokens(marked) {
        if let Token::Text(t) = &token {
            let len = t.chars().count();
            if len > left {
                kept.push(Token::Text(t.chars().take(left).collect()));
                break;
            }
            left -= len;
        }
        kept.push(token);
    }
    clean_marked_for_speech(&render(&kept))
}

/// `text` with `direction` put before it as `[direction] `, when its first
/// line has words and no voice mark of its own (no direction, no tag).
/// A `direction` that does not fit `marks::is_direction` is ignored.
pub fn with_direction(text: &str, direction: Option<&str>) -> String {
    let Some(direction) = direction.map(str::trim).filter(|d| is_direction(d)) else {
        return text.to_string();
    };
    let first = tokens(text.lines().next().unwrap_or_default());
    let own = first
        .iter()
        .any(|t| matches!(t, Token::Direction(_) | Token::Open(_) | Token::Close(_)));
    let words = first
        .iter()
        .any(|t| matches!(t, Token::Text(s) if s.chars().any(char::is_alphanumeric)));
    if own || !words {
        return text.to_string();
    }
    format!("[{direction}] {text}")
}

/// Longest voice message, characters of its words: about half a minute.
pub const VOICE_NOTE_CHARS: usize = 500;
/// Most characters of voice marks one voice message carries besides its
/// words. Marks are not heard, but the synthesizer is paid for them: without
/// a limit a reply of three words could order thousands of characters.
pub const VOICE_NOTE_MARKS: usize = 250;
/// Said last in a voice message that leaves part of the reply out.
pub const VOICE_NOTE_TAIL: &str = "Остальное в тексте.";

/// A reply as one text for one voice message (decision 0029). With
/// `spoken` it is the copy with voice marks: they stay, balanced, each
/// phrase on its own line. `None` when there are no words to say.
///
/// A short reply is said whole. A long one gives the whole phrases that fit
/// into `VOICE_NOTE_CHARS` and `VOICE_NOTE_TAIL`; a first phrase longer than
/// the limit is cut to it. The marks have a limit too, `VOICE_NOTE_MARKS`:
/// a phrase whose marks would pass it is said by its words alone.
/// `direction` leads the first line when that line has no mark of its own.
pub fn voice_note(text: &str, spoken: Option<&str>, direction: Option<&str>) -> Option<String> {
    let marked = spoken.is_some();
    let mut phrases = splitter(marked);
    let mut split = phrases.push(spoken.unwrap_or(text));
    split.extend(phrases.finish());
    // Each phrase as it is said and as its words are counted.
    let ready = split.iter().filter_map(|phrase| {
        if marked {
            marked_speech(phrase)
        } else {
            let said = clean_for_speech(phrase);
            said.chars()
                .any(char::is_alphanumeric)
                .then(|| (said.clone(), said))
        }
    });
    // The words of a phrase alone, safe for a voice that performs marks.
    let unmarked = |visible: &str| -> String { visible.chars().filter_map(defuse).collect() };
    let mut taken: Vec<String> = Vec::new();
    let mut chars = 0;
    // Characters of marks taken so far. The synthesizer is paid for all it
    // gets, so the marks have a limit of their own: a phrase that would pass
    // it is said without them.
    let mut marks = 0;
    let mut left_out = false;
    for (said, visible) in ready {
        let len = visible.chars().count();
        if chars + len > VOICE_NOTE_CHARS {
            left_out = true;
            if taken.is_empty() {
                let cut: String = if marked {
                    let cut = take_words(&said, VOICE_NOTE_CHARS);
                    if cut.chars().count() > VOICE_NOTE_CHARS + VOICE_NOTE_MARKS {
                        unmarked(&visible).chars().take(VOICE_NOTE_CHARS).collect()
                    } else {
                        cut
                    }
                } else {
                    said.chars().take(VOICE_NOTE_CHARS).collect()
                };
                taken.extend(Some(cut).filter(|c| !c.trim().is_empty()));
            }
            break;
        }
        chars += len;
        let own = said.chars().count().saturating_sub(len);
        if marks + own > VOICE_NOTE_MARKS {
            taken.push(unmarked(&visible));
        } else {
            marks += own;
            taken.push(said);
        }
    }
    if taken.is_empty() {
        return None;
    }
    if left_out {
        taken.push(VOICE_NOTE_TAIL.into());
    }
    // A direction ends with its line, as it does with a request for each phrase.
    let joined = taken.join(if marked { "\n" } else { " " });
    Some(with_direction(&joined, direction))
}

/// The reply being spoken and what is known about it so far.
#[derive(Default)]
struct Turn {
    splitter: SentenceSplitter,
    /// Sentences held back until it is clear the reply is short, each with
    /// whether it carries voice marks.
    held: Vec<(String, bool)>,
    /// Characters of the current piece of the reply, its marks not counted.
    chars: usize,
    /// The reply grew long: nothing of it is read out.
    long: bool,
    /// The model asked for this reply to be said whole (decision 0037): it
    /// is read out by sentences, however long, and not retold.
    whole: bool,
    /// A multi-step task runs: steps are not spoken, only the end.
    task: bool,
    /// How the task of this turn ended, `Some(ok)`, once it has.
    task_end: Option<bool>,
}

impl Turn {
    /// The sentences a piece of the reply completes, each with whether it
    /// carries voice marks. A piece of another kind than the text before it
    /// ends that text first.
    fn push(&mut self, piece: &str, marked: bool) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        if self.splitter.is_marked() != marked {
            out.extend(self.finish());
            self.splitter = splitter(marked);
        }
        out.extend(self.splitter.push(piece).into_iter().map(|s| (s, marked)));
        out
    }

    /// The rest of the reply, with whether it carries voice marks.
    fn finish(&mut self) -> Option<(String, bool)> {
        let marked = self.splitter.is_marked();
        self.splitter.finish().map(|s| (s, marked))
    }
}

/// Reads replies, questions about actions and reminders aloud until the bus closes.
///
/// With retelling on, a short reply is said whole once it ends, and a long one
/// is replaced by a short retelling, unless the model asked for it to be said
/// whole (`Event::SayWhole`): a joke or a poem is the thing asked for, and a
/// retelling of it is not. During a multi-step task nothing of the steps is
/// said: only the final reply, or why the task broke off.
pub async fn run_speaker(bus: EventBus, voice: Arc<dyn Voice>, switch: SpeakerSwitch) {
    let mut events = bus.subscribe();
    let mut turn = Turn::default();
    // True once a piece of the current reply has arrived as a delta.
    let mut streamed = false;
    // A question about an action is open: what the user says answers it, and
    // the reply goes on with its task.
    let mut confirming = false;
    let say = |text: &str| {
        let clean = clean_for_speech(text);
        if !clean.is_empty() && switch.heard() {
            voice.speak(&clean);
        }
    };
    // Words of the model. With voice marks the voice performs them, and a
    // voice that does not gets the same words without them. Without marks
    // they reach a remote synthesizer as they are.
    let say_reply = |text: &str, marked: bool| {
        if !marked {
            let clean = clean_for_speech(text);
            if !clean.is_empty() && switch.heard() {
                voice.speak_reply(&clean);
            }
        } else if let Some((with, without)) = marked_speech(text) {
            if switch.heard() {
                voice.speak_marked(&with, &without);
            }
        }
    };
    // A whole reply goes to the voice sentence by sentence: in one piece it
    // would start late and be cut off.
    let say_all = |text: &str, marked: bool| {
        let mut s = splitter(marked);
        for sentence in s.push(text).into_iter().chain(s.finish()) {
            say_reply(&sentence, marked);
        }
    };
    // Silence now, a retelling still being made included.
    let stop = || {
        switch.cancel();
        voice.stop();
    };
    loop {
        let event = match events.recv().await {
            Ok(e) => e,
            Err(RecvError::Lagged(n)) => {
                log::warn!("the speaker fell behind, part of a reply is not said: lost_events={n}");
                continue;
            }
            Err(RecvError::Closed) => break,
        };
        match &event {
            Event::ConfirmRequested { .. } => confirming = true,
            Event::ConfirmResolved { .. }
            | Event::StateChanged {
                to: AssistantState::Idle,
                ..
            } => confirming = false,
            _ => {}
        }
        // A reply the model wants said whole sounds as if retelling were off.
        let brief = switch.brief() && !turn.whole;
        match event {
            // Comes before the first piece of the reply it is about.
            Event::SayWhole => turn.whole = true,
            // An answer to the question: her voice stops, the turn goes on.
            Event::UserText { .. } if confirming => stop(),
            // A new message interrupts whatever she was saying.
            Event::UserText { .. } => {
                stop();
                turn = Turn::default();
                streamed = false;
            }
            // Not the error of the turn: the agent ends the task before it
            // reports one. The microphone or the selection failed meanwhile.
            Event::Error { .. } if turn.task && turn.task_end.is_none() => {}
            Event::Error { message } => {
                stop();
                let broke_off = turn.task_end == Some(false);
                turn = Turn::default();
                streamed = false;
                if broke_off && switch.get() {
                    say(&format!("Задача оборвалась. {message}"));
                }
            }
            // A task stopped by the user ends without a reply or an error:
            // a later error is not about it.
            Event::StateChanged {
                to: AssistantState::Idle,
                ..
            } if turn.task_end.is_some() => {
                turn = Turn::default();
                streamed = false;
            }
            // The turn is over. A reply stopped half-way leaves no request to
            // say the next one whole: a finished one has reset the turn already.
            Event::StateChanged {
                to: AssistantState::Idle,
                ..
            } => turn.whole = false,
            Event::TaskUpdated { task } => {
                if !task.finished && !turn.task {
                    // What was waiting to be said belongs to the steps now.
                    turn.held.clear();
                    turn.splitter = SentenceSplitter::default();
                }
                // Stays set to the end of the turn: the final reply comes whole.
                turn.task = true;
                turn.task_end = task.finished.then_some(task.ok);
            }
            _ if !switch.get() => {}
            Event::AssistantDelta { .. } if turn.task => streamed = true,
            Event::AssistantDelta { text, spoken } => {
                streamed = true;
                let sentences = turn.push(spoken.as_deref().unwrap_or(&text), spoken.is_some());
                if !brief {
                    for (sentence, marked) in sentences {
                        say_reply(&sentence, marked);
                    }
                    continue;
                }
                // Only the words count: marks are not read out.
                turn.chars += text.chars().count();
                if turn.chars > LONG_REPLY {
                    turn.long = true;
                    turn.held.clear();
                }
                if !turn.long {
                    turn.held.extend(sentences);
                }
            }
            // A later step of the same reply with no tool call before it: the
            // agent finds its marks anew, so the text before it ends here.
            Event::StepStarted if turn.task || !turn.splitter.is_marked() => {}
            Event::StepStarted => {
                if let Some((rest, marked)) = turn.finish() {
                    if !brief {
                        say_reply(&rest, marked);
                    } else if !turn.long {
                        turn.held.push((rest, marked));
                    }
                }
            }
            // How to answer the question: the turn goes on.
            Event::AssistantText { text, spoken } if confirming => {
                say_reply(spoken.as_deref().unwrap_or(&text), spoken.is_some())
            }
            Event::AssistantText { text, spoken } => {
                let whole = !streamed || turn.task;
                let marked = spoken.is_some();
                // Long or short by its words: marks are not read out.
                let long = text.chars().count() > LONG_REPLY;
                let said = spoken.unwrap_or(text);
                if !brief {
                    if whole {
                        say_all(&said, marked);
                    } else if let Some((rest, marked)) = turn.finish() {
                        say_reply(&rest, marked);
                    }
                } else if whole {
                    if long {
                        switch.retell(voice.clone(), said, marked);
                    } else {
                        say_reply(&said, marked);
                    }
                } else if turn.long {
                    switch.retell(voice.clone(), said, marked);
                } else {
                    for (sentence, marked) in turn.held.drain(..) {
                        say_reply(&sentence, marked);
                    }
                    if let Some((rest, marked)) = turn.finish() {
                        say_reply(&rest, marked);
                    }
                }
                turn = Turn::default();
                streamed = false;
            }
            Event::ToolStarted { .. } if turn.task => {}
            Event::ToolStarted { .. } => {
                // Words said before a tool call are a finished phrase, unless long.
                let rest = turn.finish();
                if !turn.long {
                    for (sentence, marked) in turn.held.drain(..) {
                        say_reply(&sentence, marked);
                    }
                    if let Some((rest, marked)) = rest {
                        say_reply(&rest, marked);
                    }
                }
                turn.held.clear();
                turn.chars = 0;
                turn.long = false;
                streamed = false;
            }
            Event::ConfirmRequested { assessment, .. } => {
                say(&format!("Нужно подтверждение. {}.", assessment.summary));
            }
            Event::ReminderFired {
                text,
                kind: ReminderKind::Remind,
                ..
            } => say(&format!("Напоминаю: {text}")),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::confirm::{Answer, DecidedBy};
    use mika_core::tools::{Assessment, RiskLevel};
    use std::time::Duration;

    #[derive(Default)]
    struct FakeVoice {
        said: Mutex<Vec<String>>,
        /// Phrases with voice marks, and the same words without them.
        marked: Mutex<Vec<(String, String)>>,
        stops: Mutex<usize>,
    }

    impl Voice for FakeVoice {
        fn speak(&self, text: &str) {
            self.said.lock().push(text.into());
        }
        fn speak_marked(&self, marked: &str, plain: &str) {
            self.marked.lock().push((marked.into(), plain.into()));
        }
        fn stop(&self) {
            *self.stops.lock() += 1;
        }
        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }
        fn configure(&self, _voice: &str, _rate: f32) {}
    }

    fn split(pieces: &[&str]) -> Vec<String> {
        let mut s = SentenceSplitter::default();
        let mut out: Vec<String> = pieces.iter().flat_map(|p| s.push(p)).collect();
        out.extend(s.finish());
        out
    }

    #[test]
    fn sentences_come_out_as_soon_as_they_end() {
        let mut s = SentenceSplitter::default();
        assert!(s.push("Сегодня минус десять").is_empty());
        assert_eq!(
            s.push(" и ветер. Оденься"),
            vec!["Сегодня минус десять и ветер."]
        );
        assert_eq!(s.push(" потеплее!\n"), vec!["Оденься потеплее!"]);
        assert_eq!(s.finish(), None);
    }

    #[test]
    fn short_pieces_and_numbers_do_not_split() {
        assert_eq!(
            split(&["Это т. е. пример на 3.5 балла. Конец"]),
            vec!["Это т. е. пример на 3.5 балла.", "Конец"]
        );
        assert_eq!(split(&["Да. Нет. Наверное, завтра получится."]).len(), 1);
    }

    #[test]
    fn a_line_break_ends_a_sentence_and_long_text_is_cut() {
        assert_eq!(
            split(&["Пункт один\nПункт два"]),
            vec!["Пункт один", "Пункт два"]
        );
        let long = "слово ".repeat(200);
        let parts = split(&[&long]);
        assert!(parts.len() >= 2);
        // The cut waits for the end of the word that crosses the limit.
        assert!(parts.iter().all(|p| p.chars().count() <= MAX_SENTENCE + 30));
    }

    #[test]
    fn tone_goes_into_ssml_and_text_is_escaped() {
        assert_eq!(
            prosody_ssml("A & <b>", "", (8, -6)),
            "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"ru-RU\">\
<prosody pitch=\"+8%\" rate=\"-6%\">A &amp; &lt;b&gt;</prosody></speak>"
        );
        assert!(prosody_ssml("x", "en-US", (0, 0)).contains("xml:lang=\"en-US\""));
    }

    #[test]
    fn markup_links_and_emoji_are_not_read() {
        assert_eq!(
            clean_for_speech("**Готово!** Файл `a.txt` тут: https://ya.ru/x?y=1 😀\n\n# Итог"),
            "Готово! Файл a.txt тут: ссылка Итог"
        );
        assert_eq!(clean_for_speech(" 🎉 "), "");
    }

    async fn run(events: Vec<Event>, on: bool) -> Arc<FakeVoice> {
        let bus = EventBus::new(64);
        let voice = Arc::new(FakeVoice::default());
        let job = tokio::spawn(run_speaker(
            bus.clone(),
            voice.clone(),
            SpeakerSwitch::new(on),
        ));
        tokio::time::sleep(Duration::from_millis(10)).await;
        for e in events {
            bus.publish(e);
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        job.abort();
        voice
    }

    fn delta(t: &str) -> Event {
        Event::AssistantDelta {
            text: t.into(),
            spoken: None,
        }
    }

    #[tokio::test]
    async fn reply_is_spoken_by_sentences_and_a_new_message_interrupts() {
        let v = run(
            vec![
                Event::UserText {
                    text: "погода".into(),
                },
                delta("Сейчас минус десять градусов. "),
                delta("Оденься **теплее**"),
                Event::AssistantText {
                    text: "Сейчас минус десять градусов. Оденься **теплее**".into(),
                    spoken: None,
                },
                Event::UserText {
                    text: "стоп".into(),
                },
            ],
            true,
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec!["Сейчас минус десять градусов.", "Оденься теплее"]
        );
        assert_eq!(*v.stops.lock(), 2);
    }

    #[tokio::test]
    async fn whole_reply_question_and_reminder_are_spoken() {
        let v = run(
            vec![
                Event::AssistantText {
                    text: "Сначала ответь на вопрос.".into(),
                    spoken: None,
                },
                Event::ConfirmRequested {
                    id: 1,
                    tool: "power".into(),
                    assessment: Assessment::new(RiskLevel::High, "Выключить компьютер", "x", "y"),
                    allow_always: false,
                    timeout_s: 60,
                },
                Event::ReminderFired {
                    id: 1,
                    text: "чайник".into(),
                    kind: ReminderKind::Remind,
                    late_s: 0,
                },
                Event::ReminderFired {
                    id: 2,
                    text: "скрытая задача".into(),
                    kind: ReminderKind::Task,
                    late_s: 0,
                },
            ],
            true,
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec![
                "Сначала ответь на вопрос.",
                "Нужно подтверждение. Выключить компьютер.",
                "Напоминаю: чайник"
            ]
        );
    }

    #[tokio::test]
    async fn switched_off_speaker_stays_silent_but_still_stops() {
        let v = run(
            vec![
                delta("Это не должно прозвучать. "),
                Event::AssistantText {
                    text: "Это не должно прозвучать.".into(),
                    spoken: None,
                },
                Event::UserText { text: "x".into() },
            ],
            false,
        )
        .await;
        assert!(v.said.lock().is_empty());
        assert_eq!(*v.stops.lock(), 1);
    }

    #[tokio::test]
    async fn nothing_is_synthesized_while_the_system_sound_is_muted() {
        let bus = EventBus::new(64);
        let voice = Arc::new(FakeVoice::default());
        let muted = Arc::new(AtomicBool::new(true));
        let m = muted.clone();
        let switch =
            SpeakerSwitch::new(true).with_audible(Arc::new(move || !m.load(Ordering::Relaxed)));
        let job = tokio::spawn(run_speaker(bus.clone(), voice.clone(), switch));
        tokio::time::sleep(Duration::from_millis(10)).await;
        bus.publish(Event::AssistantText {
            text: "В тишину.".into(),
            spoken: None,
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(voice.said.lock().is_empty());
        muted.store(false, Ordering::Relaxed);
        bus.publish(Event::AssistantText {
            text: "Теперь слышно.".into(),
            spoken: None,
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        job.abort();
        assert_eq!(*voice.said.lock(), ["Теперь слышно."]);
    }

    async fn run_with(events: Vec<Event>, switch: SpeakerSwitch) -> Arc<FakeVoice> {
        let bus = EventBus::new(64);
        let voice = Arc::new(FakeVoice::default());
        let job = tokio::spawn(run_speaker(bus.clone(), voice.clone(), switch));
        tokio::time::sleep(Duration::from_millis(10)).await;
        for e in events {
            bus.publish(e);
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        job.abort();
        voice
    }

    fn brief_switch() -> SpeakerSwitch {
        let s = SpeakerSwitch::new(true).with_summarizer(Arc::new(
            |reply: String, _expressive: bool| {
                Box::pin(async move {
                    Some(format!("Коротко: {} знаков.", reply.chars().count()))
                })
            },
        ));
        s.set_brief(true);
        s
    }

    fn task(finished: bool) -> Event {
        task_with(finished, finished)
    }

    fn task_with(finished: bool, ok: bool) -> Event {
        Event::TaskUpdated {
            task: mika_core::task::Task {
                title: "Задача".into(),
                steps: Vec::new(),
                finished,
                ok,
            },
        }
    }

    fn idle() -> Event {
        Event::StateChanged {
            from: AssistantState::Thinking,
            to: AssistantState::Idle,
        }
    }

    fn error(message: &str) -> Event {
        Event::Error {
            message: message.into(),
        }
    }

    fn confirm_requested() -> Event {
        Event::ConfirmRequested {
            id: 7,
            tool: "messenger_send".into(),
            assessment: Assessment::new(RiskLevel::High, "Написать Ане", "x", "y"),
            allow_always: false,
            timeout_s: 60,
        }
    }

    fn confirm_resolved() -> Event {
        Event::ConfirmResolved {
            id: 7,
            answer: Answer::Once,
            by: DecidedBy::Chat,
        }
    }

    /// Long-reply retelling that waits until the test lets it go.
    fn gated_switch() -> (SpeakerSwitch, tokio::sync::oneshot::Sender<()>) {
        let (go, wait) = tokio::sync::oneshot::channel::<()>();
        let wait = Arc::new(Mutex::new(Some(wait)));
        let s =
            SpeakerSwitch::new(true).with_summarizer(Arc::new(move |_reply: String, _: bool| {
                let wait = wait.lock().take();
                Box::pin(async move {
                    if let Some(w) = wait {
                        let _ = w.await;
                    }
                    Some("Коротко о главном.".into())
                })
            }));
        s.set_brief(true);
        (s, go)
    }

    /// What happened to a long reply whose retelling the model makes slowly.
    struct SlowRetelling {
        voice: Arc<FakeVoice>,
        /// She counted as speaking while the model was retelling.
        busy_meanwhile: bool,
        /// The retelling still waited for the model when it answered.
        model_waited: bool,
    }

    async fn long_reply_with_a_slow_retelling(stop_meanwhile: bool) -> SlowRetelling {
        let bus = EventBus::new(64);
        let inner = Arc::new(FakeVoice::default());
        let (switch, go) = gated_switch();
        let voice = switch.wrap(inner.clone());
        let job = tokio::spawn(run_speaker(bus.clone(), voice.clone(), switch));
        tokio::time::sleep(Duration::from_millis(10)).await;
        bus.publish(Event::AssistantText {
            text: "Это очень длинный ответ про погоду на неделю. ".repeat(10),
            spoken: None,
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        // The model is still retelling: she has not finished speaking.
        let busy_meanwhile = voice.busy();
        assert!(inner.said.lock().is_empty());
        if stop_meanwhile {
            // «Стоп», the stop word or the microphone opening.
            voice.stop();
            // Silent at once, not when the model answers.
            assert!(!voice.busy());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let model_waited = go.send(()).is_ok();
        tokio::time::sleep(Duration::from_millis(30)).await;
        job.abort();
        assert!(!voice.busy());
        SlowRetelling {
            voice: inner,
            busy_meanwhile,
            model_waited,
        }
    }

    /// A joke longer than `LONG_REPLY`, and its sentences as the voice gets them.
    fn long_joke() -> (String, Vec<String>) {
        let sentences: Vec<String> = (1..=8)
            .map(|n| format!("Приходит программист номер {n} в бар и заказывает ровно {n} кружек."))
            .collect();
        let joke = sentences.join(" ");
        assert!(joke.chars().count() > LONG_REPLY);
        (joke, sentences)
    }

    #[tokio::test]
    async fn a_long_reply_the_model_wants_whole_is_read_out_not_retold() {
        let (joke, sentences) = long_joke();
        // Streamed: the sentences sound as they come.
        let v = run_with(
            vec![
                Event::UserText {
                    text: "расскажи анекдот".into(),
                },
                Event::SayWhole,
                delta(&format!("{joke} ")),
                Event::AssistantText {
                    text: joke.clone(),
                    spoken: None,
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(*v.said.lock(), sentences);

        // In one piece.
        let v = run_with(
            vec![
                Event::SayWhole,
                Event::AssistantText {
                    text: joke.clone(),
                    spoken: None,
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(*v.said.lock(), sentences);

        // Without the request the same reply is retold, as before.
        let v = run_with(
            vec![Event::AssistantText {
                text: joke.clone(),
                spoken: None,
            }],
            brief_switch(),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            [format!("Коротко: {} знаков.", joke.chars().count())]
        );
    }

    #[tokio::test]
    async fn the_request_to_say_it_whole_ends_with_its_reply() {
        let (joke, sentences) = long_joke();
        let retold = format!("Коротко: {} знаков.", joke.chars().count());
        let reply = || Event::AssistantText {
            text: joke.clone(),
            spoken: None,
        };
        // The next reply, and a reply to a new message, are retold again.
        let v = run_with(vec![Event::SayWhole, reply(), reply()], brief_switch()).await;
        let mut expected = sentences.clone();
        expected.push(retold.clone());
        assert_eq!(*v.said.lock(), expected);

        let v = run_with(
            vec![
                Event::SayWhole,
                Event::UserText {
                    text: "стой".into(),
                },
                reply(),
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(*v.said.lock(), std::slice::from_ref(&retold));

        // A reply stopped half-way: the turn ends without its text.
        let v = run_with(vec![Event::SayWhole, idle(), reply()], brief_switch()).await;
        assert_eq!(*v.said.lock(), [retold]);
    }

    #[tokio::test]
    async fn a_retelling_being_made_counts_as_speech() {
        let r = long_reply_with_a_slow_retelling(false).await;
        assert!(r.busy_meanwhile);
        assert!(r.model_waited);
        assert_eq!(*r.voice.said.lock(), ["Коротко о главном."]);
    }

    #[tokio::test]
    async fn a_stop_drops_a_retelling_being_made() {
        let r = long_reply_with_a_slow_retelling(true).await;
        assert!(r.busy_meanwhile);
        // The model is not waited for after the stop.
        assert!(!r.model_waited);
        assert!(r.voice.said.lock().is_empty());
        assert_eq!(*r.voice.stops.lock(), 1);
    }

    #[tokio::test]
    async fn a_stop_between_two_retellings_lets_go_of_the_old_one_only() {
        let switch = gated_switch().0;
        let inner = Arc::new(FakeVoice::default());
        let voice = switch.wrap(inner.clone());
        switch.retell(inner.clone(), "Первый длинный ответ.".into(), false);
        assert!(voice.busy());
        voice.stop();
        assert!(!voice.busy());
        // The next reply is retold and counts as speech until it is said.
        switch.retell(inner.clone(), "Второй длинный ответ.".into(), false);
        assert!(voice.busy());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!voice.busy());
        assert_eq!(*inner.said.lock(), ["Коротко о главном."]);
    }

    #[tokio::test]
    async fn an_answer_to_a_question_keeps_the_task_silent() {
        // A step after the answer is not said.
        let v = run_with(
            vec![
                task(false),
                confirm_requested(),
                Event::UserText {
                    text: "эээ".into()
                },
                Event::AssistantText {
                    text: "Скажи «да» или «нет».".into(),
                    spoken: None,
                },
                Event::UserText {
                    text: "да".into()
                },
                confirm_resolved(),
                delta("Отправила. Теперь открою браузер. "),
                Event::ToolStarted {
                    id: "2".into(),
                    name: "open_app".into(),
                    summary: "Открыть".into(),
                },
                task(true),
                Event::AssistantText {
                    text: "Всё сделала.".into(),
                    spoken: None,
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            [
                "Нужно подтверждение. Написать Ане.",
                "Скажи «да» или «нет».",
                "Всё сделала."
            ]
        );

        // Without retelling the end right after the answer is said once.
        let end = "Сообщение Ане отправлено.";
        let v = run_with(
            vec![
                task(false),
                confirm_requested(),
                Event::UserText {
                    text: "да".into()
                },
                confirm_resolved(),
                delta(&format!("{end} ")),
                task(true),
                Event::AssistantText {
                    text: end.into(),
                    spoken: None,
                },
            ],
            SpeakerSwitch::new(true),
        )
        .await;
        assert_eq!(*v.said.lock(), ["Нужно подтверждение. Написать Ане.", end]);
    }

    #[tokio::test]
    async fn an_error_from_elsewhere_is_not_a_broken_task() {
        // The microphone fails while the task runs: the task goes on silently.
        let v = run_with(
            vec![
                task(false),
                error("не услышала речь"),
                delta("Шаг два готов. "),
                Event::ToolStarted {
                    id: "1".into(),
                    name: "open_app".into(),
                    summary: "Открыть".into(),
                },
                task(true),
                Event::AssistantText {
                    text: "Всё сделала.".into(),
                    spoken: None,
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(*v.said.lock(), ["Всё сделала."]);

        // A task stopped by the user; later the microphone hears nothing.
        let v = run_with(
            vec![
                task(false),
                task_with(true, false),
                idle(),
                error("не услышала речь"),
            ],
            brief_switch(),
        )
        .await;
        assert!(v.said.lock().is_empty());
    }

    #[tokio::test]
    async fn without_retelling_the_end_of_a_task_is_said_by_sentences() {
        let end = "Нашла три файла с отчётами. Самый новый — за октябрь. Остальные старше года.";
        let v = run_with(
            vec![
                task(false),
                task(true),
                Event::AssistantText {
                    text: end.into(),
                    spoken: None,
                },
            ],
            SpeakerSwitch::new(true),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            [
                "Нашла три файла с отчётами.",
                "Самый новый — за октябрь.",
                "Остальные старше года."
            ]
        );
    }

    #[tokio::test]
    async fn a_short_reply_is_said_whole_and_a_long_one_is_retold() {
        let short = "Сейчас минус десять градусов. Оденься теплее.";
        let v = run_with(
            vec![
                delta(short),
                Event::AssistantText {
                    text: short.into(),
                    spoken: None,
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec!["Сейчас минус десять градусов.", "Оденься теплее."]
        );

        let long = "Это очень длинный ответ про погоду на неделю. ".repeat(10);
        let v = run_with(
            vec![
                delta(&long),
                Event::AssistantText {
                    text: long.clone(),
                    spoken: None,
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec![format!("Коротко: {} знаков.", long.chars().count())]
        );
    }

    #[tokio::test]
    async fn steps_of_a_task_are_silent_and_only_the_end_is_said() {
        let v = run_with(
            vec![
                task(false),
                delta("Шаг один готов. "),
                Event::ToolStarted {
                    id: "1".into(),
                    name: "open_app".into(),
                    summary: "Открыть".into(),
                },
                delta("Шаг два готов. "),
                task(true),
                Event::AssistantText {
                    text: "Всё сделала.".into(),
                    spoken: None,
                },
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(*v.said.lock(), vec!["Всё сделала."]);

        // A task that broke off says why. The agent ends the task first.
        let v = run_with(
            vec![
                task(false),
                delta("Шаг один. "),
                task_with(true, false),
                error("слишком много шагов подряд"),
            ],
            brief_switch(),
        )
        .await;
        assert_eq!(
            *v.said.lock(),
            vec!["Задача оборвалась. слишком много шагов подряд"]
        );
    }

    #[test]
    fn without_a_retelling_the_first_sentence_points_to_the_chat() {
        assert_eq!(
            fallback_brief("**Готово!** Вот подробный план на неделю. Пункт первый."),
            "Готово! Вот подробный план на неделю. Подробности в чате."
        );
        assert_eq!(fallback_brief("```код```"), "код Подробности в чате.");
    }

    fn split_marked(pieces: &[&str]) -> Vec<String> {
        let mut s = SentenceSplitter::marked();
        let mut out: Vec<String> = pieces.iter().flat_map(|p| s.push(p)).collect();
        out.extend(s.finish());
        out
    }

    fn marked_delta(text: &str, spoken: &str) -> Event {
        Event::AssistantDelta {
            text: text.into(),
            spoken: Some(spoken.into()),
        }
    }

    fn marked_text(text: &str, spoken: &str) -> Event {
        Event::AssistantText {
            text: text.into(),
            spoken: Some(spoken.into()),
        }
    }

    fn pair(marked: &str, plain: &str) -> (String, String) {
        (marked.into(), plain.into())
    }

    #[test]
    fn a_tag_across_two_sentences_is_closed_and_opened_again() {
        let reply =
            "[sigh] <whisper>Иди без меня, я справлюсь. Я догоню тебя потом.</whisper> Пока!";
        let phrases = vec![
            "[sigh] <whisper>Иди без меня, я справлюсь.</whisper>",
            "<whisper>Я догоню тебя потом.</whisper>",
            "Пока!",
        ];
        assert_eq!(split_marked(&[reply]), phrases);
        // The same wherever the stream is cut.
        for (at, _) in reply.char_indices() {
            assert_eq!(
                split_marked(&[&reply[..at], &reply[at..]]),
                phrases,
                "split at {at}"
            );
        }
        let chars: Vec<String> = reply.chars().map(String::from).collect();
        let refs: Vec<&str> = chars.iter().map(String::as_str).collect();
        assert_eq!(split_marked(&refs), phrases, "one character at a time");
    }

    #[test]
    fn a_long_marked_reply_is_split_the_same_however_it_comes() {
        let reply = "Пункт первый\nПервая фраза готова. ```x``` [sigh] Второе предложение тут.\n\
<whisper>Третья фраза шёпотом. Четвёртая</whisper> тоже.\n\
Код: `[sigh]` и [1] конец. [laughs] Всё.";
        let phrases = vec![
            "Пункт первый",
            "Первая фраза готова.",
            // Backticks inside a line open inline code, not a block.
            "x [sigh] Второе предложение тут.",
            "<whisper>Третья фраза шёпотом.</whisper>",
            "<whisper>Четвёртая</whisper> тоже.",
            "Код: (sigh) и (1) конец.",
            "[laughs] Всё.",
        ];
        assert_eq!(split_marked(&[reply]), phrases);
        for (at, _) in reply.char_indices() {
            assert_eq!(
                split_marked(&[&reply[..at], &reply[at..]]),
                phrases,
                "split at {at}"
            );
        }
        let chars: Vec<String> = reply.chars().map(String::from).collect();
        let refs: Vec<&str> = chars.iter().map(String::as_str).collect();
        assert_eq!(split_marked(&refs), phrases, "one character at a time");
        // Said text is let go, also by the second piece of a sentence.
        let pieces: Vec<&str> = reply.split_inclusive(' ').collect();
        assert_eq!(split_marked(&pieces), phrases, "word by word");
    }

    #[test]
    fn marks_without_words_wait_for_the_next_phrase_and_go_at_the_end() {
        assert_eq!(
            split_marked(&["Первая фраза готова.\n[sigh]\nВторая фраза тоже. [laughs]"]),
            vec!["Первая фраза готова.", "[sigh]\nВторая фраза тоже."]
        );
        assert_eq!(
            split_marked(&["Конец разговора на сегодня. [sigh]"]),
            vec!["Конец разговора на сегодня."]
        );
        assert!(split_marked(&[" [sigh] <whisper></whisper>\n"]).is_empty());
        // A new reply starts with no tag open.
        let mut s = SentenceSplitter::marked();
        s.push("<whisper>Открытый тег без конца");
        assert_eq!(
            s.finish().as_deref(),
            Some("<whisper>Открытый тег без конца</whisper>")
        );
        assert_eq!(
            s.push("Новый ответ готов, да. "),
            ["Новый ответ готов, да."]
        );
    }

    #[test]
    fn marked_lengths_count_only_the_words() {
        // Marks make no sentence long enough to stand alone...
        assert_eq!(
            split_marked(&["[barely holding back tears] Да. Нет, потом."]),
            vec!["[barely holding back tears] Да. Нет, потом."]
        );
        // ...and no text long enough to be cut.
        let reply = "[sigh] слово ".repeat(50);
        assert!(reply.chars().count() > MAX_SENTENCE);
        assert_eq!(split_marked(&[&reply]).len(), 1);
        assert!(split(&[&reply]).len() > 1, "plain text is cut");
    }

    #[test]
    fn brackets_and_code_of_the_text_never_turn_into_marks() {
        let phrases =
            split_marked(&["Смотри [1] и `a[i]`, потом <b>.\n```\n[sigh] тут\n```\nГотово."]);
        assert_eq!(
            phrases,
            vec!["Смотри (1) и a(i), потом b.", "(sigh) тут", "Готово."]
        );
        for p in &phrases {
            assert!(tokens(p).iter().all(|t| matches!(t, Token::Text(_))), "{p}");
        }
    }

    #[test]
    fn speech_with_marks_cleans_only_the_text_between_them() {
        for (text, said) in [
            (
                "[sigh] **Ну** <whisper>иди https://ya.ru/x</whisper> 😀",
                "[sigh] Ну <whisper>иди ссылка</whisper>",
            ),
            (
                "Ну, <whisper>тихо</whisper>, да",
                "Ну, <whisper>тихо</whisper>, да",
            ),
            (
                "<whisper>Иди <SOFT>без меня",
                "<whisper>Иди <soft>без меня</soft></whisper>",
            ),
            (
                "Пока.</whisper> [  softly   now ] да",
                "Пока. [softly now] да",
            ),
            ("<whisper></whisper> Да <soft> </soft>", "Да"),
            (
                "<whisper><soft>а</whisper>б</soft>",
                "<whisper><soft>а</soft></whisper><soft>б</soft>",
            ),
            ("Смотри [1] и `[sigh]`, a < b", "Смотри (1) и (sigh), a b"),
            ("Да [sigh][1]", "Да [sigh] (1)"),
            // A direction ends with its line, so line breaks stay.
            ("[angry] Нет!\nНо да.", "[angry] Нет!\nНо да."),
            ("[sigh]  Ну \n\n  да  ", "[sigh] Ну\nда"),
            ("[sigh]", ""),
            ("<whisper>[sigh]</whisper> 🎉", ""),
            ("", ""),
        ] {
            assert_eq!(clean_marked_for_speech(text), said, "{text}");
        }
    }

    #[test]
    fn a_wrapped_voice_passes_the_marks_on() {
        let inner = Arc::new(FakeVoice::default());
        let voice = SpeakerSwitch::new(true).wrap(inner.clone());
        voice.speak_marked("[sigh] Ну да.", "Ну да.");
        assert_eq!(*inner.marked.lock(), [pair("[sigh] Ну да.", "Ну да.")]);
        assert!(inner.said.lock().is_empty());
    }

    #[tokio::test]
    async fn marks_reach_only_the_voice_that_performs_them() {
        let v = run(
            vec![
                marked_delta(
                    "Иди без меня, я справлюсь. ",
                    "[sigh] <whisper>Иди без меня, я справлюсь. ",
                ),
                marked_delta("Я догоню тебя потом.", "Я догоню тебя потом.</whisper>"),
                marked_text(
                    "Иди без меня, я справлюсь. Я догоню тебя потом.",
                    "[sigh] <whisper>Иди без меня, я справлюсь. Я догоню тебя потом.</whisper> [laughs]",
                ),
            ],
            true,
        )
        .await;
        assert!(v.said.lock().is_empty(), "no marks for a plain voice");
        let marked = v.marked.lock();
        assert_eq!(
            *marked,
            [
                pair(
                    "[sigh] <whisper>Иди без меня, я справлюсь.</whisper>",
                    "Иди без меня, я справлюсь."
                ),
                pair(
                    "<whisper>Я догоню тебя потом.</whisper>",
                    "Я догоню тебя потом."
                ),
            ]
        );
        // The Windows voice gets the words only.
        assert!(marked
            .iter()
            .all(|(_, plain)| !plain.contains(['[', ']', '<', '>'])));
    }

    #[tokio::test]
    async fn a_whole_marked_reply_is_said_by_phrases_without_a_lone_mark_at_the_end() {
        let v = run(
            vec![marked_text(
                "Хорошо, договорились на завтра. До встречи утром!",
                "[warmly] Хорошо, договорились на завтра. <soft>До встречи утром!</soft> [sigh]",
            )],
            true,
        )
        .await;
        assert!(v.said.lock().is_empty());
        assert_eq!(
            *v.marked.lock(),
            [
                pair(
                    "[warmly] Хорошо, договорились на завтра.",
                    "Хорошо, договорились на завтра."
                ),
                // The direction lasts to the end of its line.
                pair(
                    "[warmly] <soft>До встречи утром!</soft>",
                    "До встречи утром!"
                ),
            ]
        );
    }

    /// Phrases of `reply`, the same however the stream is cut.
    fn split_marked_anyhow(reply: &str) -> Vec<String> {
        let phrases = split_marked(&[reply]);
        for (at, _) in reply.char_indices() {
            assert_eq!(
                split_marked(&[&reply[..at], &reply[at..]]),
                phrases,
                "{reply}: split at {at}"
            );
        }
        let chars: Vec<String> = reply.chars().map(String::from).collect();
        let refs: Vec<&str> = chars.iter().map(String::as_str).collect();
        assert_eq!(
            split_marked(&refs),
            phrases,
            "{reply}: one character at a time"
        );
        phrases
    }

    #[test]
    fn a_direction_lasts_to_the_end_of_its_line() {
        for (reply, phrases) in [
            (
                "[sadly, barely holding back tears] Я не смогла найти файл. Попробую ещё раз позже, ладно?",
                vec![
                    "[sadly, barely holding back tears] Я не смогла найти файл.",
                    "[sadly, barely holding back tears] Попробую ещё раз позже, ладно?",
                ],
            ),
            // Not past the end of the line, whether the break ends a phrase
            // or starts the next one.
            (
                "[sadly] Я не смогла найти файл. Попробую позже, ладно?\nА пока отдохни, я рядом.",
                vec![
                    "[sadly] Я не смогла найти файл.",
                    "[sadly] Попробую позже, ладно?",
                    "А пока отдохни, я рядом.",
                ],
            ),
            (
                "[sadly] Строка без точки в конце\nНовая строка тут.",
                vec!["[sadly] Строка без точки в конце", "Новая строка тут."],
            ),
            // A new direction takes over.
            (
                "[sadly] Я не смогла найти файл. [cheerfully] Но нашла другой! И он даже лучше.",
                vec![
                    "[sadly] Я не смогла найти файл.",
                    "[cheerfully] Но нашла другой!",
                    "[cheerfully] И он даже лучше.",
                ],
            ),
            // It comes before the tags opened again.
            (
                "[warmly] <soft>Хорошо, договорились на завтра. До встречи утром!</soft>",
                vec![
                    "[warmly] <soft>Хорошо, договорились на завтра.</soft>",
                    "[warmly] <soft>До встречи утром!</soft>",
                ],
            ),
            // A sound is said once, and so is an emotion tag.
            (
                "[sigh] Я не смогла найти файл. Попробую ещё раз позже.",
                vec!["[sigh] Я не смогла найти файл.", "Попробую ещё раз позже."],
            ),
            (
                "[long pause] Я не смогла найти файл. Попробую ещё раз позже.",
                vec!["[long pause] Я не смогла найти файл.", "Попробую ещё раз позже."],
            ),
            (
                "[joy] Я нашла этот файл, ура! Он лежал в загрузках.",
                vec!["[joy] Я нашла этот файл, ура!", "Он лежал в загрузках."],
            ),
            (
                "[sadly] Я не смогла найти файл. [sigh] Попробую позже. И ещё раз завтра.",
                vec![
                    "[sadly] Я не смогла найти файл.",
                    "[sigh] Попробую позже.",
                    "И ещё раз завтра.",
                ],
            ),
        ] {
            assert_eq!(split_marked_anyhow(reply), phrases, "{reply}");
        }
        // A new reply starts with no direction.
        let mut s = SentenceSplitter::marked();
        s.push("[sadly] Открытая строка без конца");
        assert!(s.finish().is_some());
        assert_eq!(
            s.push("Новый ответ готов, да. "),
            ["Новый ответ готов, да."]
        );
    }

    #[test]
    fn said_text_is_let_go_once_its_code_is_closed() {
        let mut s = SentenceSplitter::marked();
        s.push("Файл `a.txt` сохранён, всё хорошо. ");
        for _ in 0..50 {
            s.push("Ещё одна фраза про этот файл. ");
        }
        let held = s.marks.as_ref().map_or(0, |m| m.text.len());
        assert!(held < 200, "{held} bytes held");
        // Inside a code block, said lines are let go too: the block is
        // opened again in their place.
        let mut s = SentenceSplitter::marked();
        let mut whole = String::from("Код:\n```\n");
        s.push("Код:\n```\n");
        for _ in 0..2000 {
            s.push("let [x] = <y>;\n");
            whole.push_str("let [x] = <y>;\n");
        }
        let held = s.marks.as_ref().map_or(0, |m| m.text.len());
        assert!(held < 200, "{held} bytes held");
        assert!(s
            .marks
            .as_ref()
            .is_some_and(|m| m.text.starts_with("```\n")));
        // After the block, marks are found again, as in the whole text.
        let tail = "```\n[sigh] Готово, код выше работает.\n";
        let mut out = s.push(tail);
        out.extend(s.finish());
        whole.push_str(tail);
        assert_eq!(out.last(), split_marked(&[&whole]).last());
        assert_eq!(
            out.last().map(String::as_str),
            Some("[sigh] Готово, код выше работает.")
        );
    }

    #[tokio::test]
    async fn a_new_step_ends_the_marked_text_before_it() {
        for (first, second, phrases) in [
            (
                "Минутку, сейчас я посмотрю",
                "[cheerfully] Завтра будет солнечно и тепло.",
                [
                    pair("Минутку, сейчас я посмотрю", "Минутку, сейчас я посмотрю"),
                    pair(
                        "[cheerfully] Завтра будет солнечно и тепло.",
                        "Завтра будет солнечно и тепло.",
                    ),
                ],
            ),
            (
                "Сейчас запущу `npm",
                "[sigh] Готово, всё работает как надо.",
                [
                    pair("Сейчас запущу npm", "Сейчас запущу npm"),
                    pair(
                        "[sigh] Готово, всё работает как надо.",
                        "Готово, всё работает как надо.",
                    ),
                ],
            ),
        ] {
            for switch in [SpeakerSwitch::new(true), brief_switch()] {
                let v = run_with(
                    vec![
                        marked_delta(&strip_marks(first), first),
                        // The agent nudged the model: a new step, no tool between.
                        Event::StepStarted,
                        marked_delta(&strip_marks(second), second),
                        marked_text(&strip_marks(second), second),
                    ],
                    switch,
                )
                .await;
                assert!(v.said.lock().is_empty());
                assert_eq!(*v.marked.lock(), phrases);
            }
        }
    }

    #[tokio::test]
    async fn a_direction_after_the_last_words_reaches_the_voice_when_streamed() {
        let spoken = "Всё готово, файл сохранён. Удачи! [laughs]";
        let streamed = vec![
            marked_delta("Всё готово, файл сохранён.", "Всё готово, файл сохранён."),
            marked_delta(" Удачи!", " Удачи!"),
            // What the agent sends for marks after the last words.
            marked_delta("", " [laughs]"),
            marked_text("Всё готово, файл сохранён. Удачи!", spoken),
        ];
        let whole = vec![marked_text("Всё готово, файл сохранён. Удачи!", spoken)];
        for events in [streamed, whole] {
            for switch in [SpeakerSwitch::new(true), brief_switch()] {
                let v = run_with(events.clone(), switch).await;
                let marked = v.marked.lock();
                let (with, without) = marked.last().cloned().unwrap_or_default();
                assert!(
                    with.ends_with("Удачи! [laughs]") && without.ends_with("Удачи!"),
                    "{marked:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_whole_reply_keeps_its_lines_for_the_directions() {
        let spoken = "[angry] Нет, так нельзя делать!\nНо я помогу, конечно, давай вместе.";
        let shown = "Нет, так нельзя делать!\nНо я помогу, конечно, давай вместе.";
        let v = run_with(vec![marked_text(shown, spoken)], brief_switch()).await;
        assert_eq!(
            *v.marked.lock(),
            [pair(
                spoken,
                "Нет, так нельзя делать! Но я помогу, конечно, давай вместе."
            )]
        );
    }

    /// Tells the words of the model from other text.
    #[derive(Default)]
    struct KindVoice {
        said: Mutex<Vec<(&'static str, String)>>,
    }

    impl Voice for KindVoice {
        fn speak(&self, text: &str) {
            self.said.lock().push(("other", text.into()));
        }
        fn speak_reply(&self, text: &str) {
            self.said.lock().push(("reply", text.into()));
        }
        fn stop(&self) {}
        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }
        fn configure(&self, _voice: &str, _rate: f32) {}
    }

    #[tokio::test]
    async fn words_of_the_model_without_marks_go_to_the_voice_as_they_are() {
        let long = "Это очень длинный ответ про погоду на неделю. ".repeat(10);
        for (switch, reply) in [
            (SpeakerSwitch::new(true), "Нажми [Сохранить] и всё."),
            (brief_switch(), long.trim_end()),
        ] {
            let bus = EventBus::new(64);
            let inner = Arc::new(KindVoice::default());
            // The voice the app uses wraps the real one.
            let voice = switch.wrap(inner.clone());
            let job = tokio::spawn(run_speaker(bus.clone(), voice, switch));
            tokio::time::sleep(Duration::from_millis(10)).await;
            for e in [
                delta(&format!("{reply} ")),
                Event::AssistantText {
                    text: reply.into(),
                    spoken: None,
                },
                Event::ReminderFired {
                    id: 1,
                    text: "чайник".into(),
                    kind: ReminderKind::Remind,
                    late_s: 0,
                },
            ] {
                bus.publish(e);
            }
            tokio::time::sleep(Duration::from_millis(60)).await;
            job.abort();
            let said = inner.said.lock();
            let replies: Vec<&str> = said
                .iter()
                .filter(|(kind, _)| *kind == "reply")
                .map(|(_, t)| t.as_str())
                .collect();
            if reply.chars().count() <= LONG_REPLY {
                assert_eq!(replies, [reply]);
            } else {
                // The retelling is words of the model too.
                assert_eq!(replies.len(), 1, "{said:?}");
                assert!(replies[0].starts_with("Коротко:"), "{said:?}");
            }
            assert!(
                said.contains(&("other", "Напоминаю: чайник".into())),
                "{said:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_reply_without_marks_never_reaches_the_marked_voice() {
        let v = run(
            vec![
                delta("Смотри [1] и <b>жирный</b> текст. "),
                Event::AssistantText {
                    text: "Смотри [1] и <b>жирный</b> текст.".into(),
                    spoken: None,
                },
            ],
            true,
        )
        .await;
        assert!(v.marked.lock().is_empty());
        assert_eq!(*v.said.lock(), ["Смотри [1] и bжирный/b текст."]);
    }

    #[tokio::test]
    async fn an_answer_to_a_question_keeps_its_marks_and_the_question_has_none() {
        let v = run(
            vec![
                confirm_requested(),
                marked_text("Скажи «да» или «нет».", "[gently] Скажи «да» или «нет»."),
            ],
            true,
        )
        .await;
        assert_eq!(*v.said.lock(), ["Нужно подтверждение. Написать Ане."]);
        assert_eq!(
            *v.marked.lock(),
            [pair(
                "[gently] Скажи «да» или «нет».",
                "Скажи «да» или «нет»."
            )]
        );
    }

    #[tokio::test]
    async fn marks_do_not_make_a_reply_long() {
        let words = "Сегодня тепло и солнечно, можно гулять. ".repeat(6);
        let marks = "[cheerfully] Сегодня тепло и солнечно, можно гулять. ".repeat(6);
        assert!(words.chars().count() <= LONG_REPLY);
        assert!(marks.chars().count() > LONG_REPLY);
        let phrase = pair(
            "[cheerfully] Сегодня тепло и солнечно, можно гулять.",
            "Сегодня тепло и солнечно, можно гулять.",
        );

        // Streamed: said phrase by phrase, not retold.
        let v = run_with(
            vec![
                marked_delta(&words, &marks),
                marked_text(words.trim_end(), marks.trim_end()),
            ],
            brief_switch(),
        )
        .await;
        assert!(v.said.lock().is_empty());
        assert_eq!(*v.marked.lock(), vec![phrase.clone(); 6]);

        // Whole: said in one piece, not retold.
        let v = run_with(
            vec![marked_text(words.trim_end(), marks.trim_end())],
            brief_switch(),
        )
        .await;
        let said = v.marked.lock();
        assert_eq!(said.len(), 1);
        assert!(said[0].0.starts_with(&phrase.0), "{said:?}");
    }

    #[tokio::test]
    async fn a_long_marked_reply_is_retold_with_its_marks() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let switch = SpeakerSwitch::new(true).with_summarizer(Arc::new(
            move |reply: String, expressive: bool| {
                seen.lock().push((reply, expressive));
                Box::pin(async {
                    Some("[softly] <whisper>Коротко</whisper> о главном. 😀".to_string())
                })
            },
        ));
        switch.set_brief(true);
        let long = "Это очень длинный ответ про погоду на неделю. ".repeat(10);
        let spoken = format!("[warmly] {long}");
        let v = run_with(vec![marked_text(&long, &spoken)], switch).await;
        assert_eq!(*asked.lock(), [(spoken, true)]);
        assert!(v.said.lock().is_empty());
        assert_eq!(
            *v.marked.lock(),
            [pair(
                "[softly] <whisper>Коротко</whisper> о главном.",
                "Коротко о главном."
            )]
        );
    }

    #[test]
    fn without_a_retelling_the_first_marked_phrase_keeps_its_tags_closed() {
        assert_eq!(
            fallback_brief_marked("<whisper>Иди без меня, я догоню. Потом</whisper> расскажу всё."),
            pair(
                "<whisper>Иди без меня, я догоню.</whisper> Подробности в чате.",
                "Иди без меня, я догоню. Подробности в чате."
            )
        );
        assert_eq!(
            fallback_brief_marked("[sigh]"),
            pair("Ответ в чате.", "Ответ в чате.")
        );
        // A long first phrase is cut by its words, its tag still closed.
        let (marked, plain) =
            fallback_brief_marked(&format!("<soft>{}</soft>", "тишина ".repeat(80)));
        assert!(marked.starts_with("<soft>тишина"), "{marked}");
        assert!(marked.ends_with("</soft> Подробности в чате."), "{marked}");
        assert_eq!(strip_marks(&marked), plain);
    }

    #[test]
    fn a_mood_direction_leads_a_phrase_without_marks_of_its_own() {
        let sad = Some("sad, quietly");
        assert_eq!(
            with_direction("Ну вот, всё готово.", sad),
            "[sad, quietly] Ну вот, всё готово."
        );
        // A direction of its own, wherever it stands in the line.
        assert_eq!(with_direction("[sigh] Ну вот.", sad), "[sigh] Ну вот.");
        assert_eq!(
            with_direction("Ну вот. [sigh] Да.", sad),
            "Ну вот. [sigh] Да."
        );
        // A tag, opened or only closed here.
        assert_eq!(
            with_direction("<whisper>Тихо.</whisper>", sad),
            "<whisper>Тихо.</whisper>"
        );
        assert_eq!(
            with_direction("Иди без меня.</whisper> Пока.", sad),
            "Иди без меня.</whisper> Пока."
        );
        // Brackets that are no direction are words of the text.
        assert_eq!(
            with_direction("Смотри (1) тут.", sad),
            "[sad, quietly] Смотри (1) тут."
        );
        assert_eq!(
            with_direction("Смотри [1] тут.", sad),
            "[sad, quietly] Смотри [1] тут."
        );
        // Only the first line is looked at, and only it gets the direction.
        assert_eq!(
            with_direction("Первая строка.\n[sigh] Вторая строка.", sad),
            "[sad, quietly] Первая строка.\n[sigh] Вторая строка."
        );
        assert_eq!(
            with_direction("[sigh] Первая строка.\nВторая строка.", sad),
            "[sigh] Первая строка.\nВторая строка."
        );
        // A first line without words gets none.
        for silent in ["", "...", "\nВторая строка.", " — "] {
            assert_eq!(with_direction(silent, sad), silent, "{silent:?}");
        }
        // No direction, or one the voice would read aloud.
        assert_eq!(with_direction("Ну вот.", None), "Ну вот.");
        for bad in ["level 2", "a", "", "  ", "sad]", "файл.txt", "sad\nquietly"] {
            assert_eq!(with_direction("Ну вот.", Some(bad)), "Ну вот.", "{bad:?}");
        }
        assert_eq!(
            with_direction("Ну вот.", Some("  sad, quietly ")),
            "[sad, quietly] Ну вот."
        );
    }

    #[test]
    fn every_mood_direction_is_read_back_as_a_direction() {
        for emotion in Emotion::ALL {
            let Some(direction) = emotion.direction() else {
                continue;
            };
            let said = with_direction("Привет, как дела?", Some(direction));
            let read = tokens(&said);
            assert_eq!(read[0], Token::Direction(direction.to_string()), "{said}");
            assert_eq!(read.len(), 2, "{said}");
            // The words are the same, and nothing of the direction is shown.
            assert_eq!(strip_marks(&said).trim(), "Привет, как дела?");
            // Said twice, it is not added twice.
            assert_eq!(with_direction(&said, Some(direction)), said);
        }
    }

    #[test]
    fn a_short_reply_is_one_voice_note_whole() {
        assert_eq!(
            voice_note("Завтра ясно, до плюс пяти. Зонт не нужен.", None, None).as_deref(),
            Some("Завтра ясно, до плюс пяти. Зонт не нужен.")
        );
        // Markdown, links and emoji are not said; the lines become one.
        assert_eq!(
            voice_note(
                "**Готово!** Файл `a.txt` тут: https://ya.ru/x?y=1 😀\n\n# Итог\nВсё на месте.",
                None,
                None
            )
            .as_deref(),
            Some("Готово! Файл a.txt тут: ссылка Итог Всё на месте.")
        );
        // No words: nothing to say, so nothing is ordered.
        for silent in ["", "  \n ", "😀 🎉", "```\n```", "...", "—"] {
            assert_eq!(voice_note(silent, None, None), None, "{silent:?}");
        }
        assert_eq!(voice_note("Ответ.", Some("[sigh]"), None), None);
        assert_eq!(
            voice_note("Ответ.", Some("<whisper></whisper> 🎉"), None),
            None
        );
    }

    #[test]
    fn a_long_reply_gives_its_first_whole_phrases_and_points_to_the_text() {
        let phrase = "Это предложение про погоду на завтра.";
        let len = phrase.chars().count();
        let reply = vec![phrase; 30].join(" ");
        let fit = VOICE_NOTE_CHARS / len;
        assert!(fit > 3 && fit < 30);
        let mut expected = vec![phrase; fit];
        expected.push(VOICE_NOTE_TAIL);
        assert_eq!(
            voice_note(&reply, None, None).as_deref(),
            Some(expected.join(" ").as_str())
        );
        // A reply of exactly the limit is said whole, without the tail.
        let exact = format!("{}.", "я".repeat(VOICE_NOTE_CHARS - 1));
        assert_eq!(voice_note(&exact, None, None), Some(exact.clone()));

        // One phrase longer than the limit is cut to it.
        let endless = format!("{} конец.", "а".repeat(600));
        assert_eq!(
            voice_note(&endless, None, None),
            Some(format!(
                "{} {VOICE_NOTE_TAIL}",
                "а".repeat(VOICE_NOTE_CHARS)
            ))
        );
    }

    #[test]
    fn a_marked_voice_note_keeps_its_marks_balanced_and_a_phrase_per_line() {
        let shown = "Иди без меня, я справлюсь. Я догоню тебя потом. Пока!";
        let spoken =
            "[sigh] <whisper>Иди без меня, я справлюсь. Я догоню тебя потом.</whisper> Пока!";
        assert_eq!(
            voice_note(shown, Some(spoken), None).as_deref(),
            Some(
                "[sigh] <whisper>Иди без меня, я справлюсь.</whisper>\n\
<whisper>Я догоню тебя потом.</whisper>\n\
Пока!"
            )
        );
        // The text between the marks is cleaned as for the speaker.
        assert_eq!(
            voice_note(
                "x",
                Some("[softly] **Готово!** Смотри https://ya.ru/x 😀"),
                None
            )
            .as_deref(),
            Some("[softly] Готово! Смотри ссылка")
        );

        // A long one: whole phrases by the count of their words, then the tail.
        let phrase = "<soft>Это предложение про погоду на завтра.</soft>";
        let words = "Это предложение про погоду на завтра.".chars().count();
        let spoken = vec![phrase; 30].join(" ");
        let fit = VOICE_NOTE_CHARS / words;
        let mut expected = vec![phrase; fit];
        expected.push(VOICE_NOTE_TAIL);
        let note = voice_note("x", Some(&spoken), None).unwrap();
        assert_eq!(note, expected.join("\n"));
        assert_eq!(
            note.matches("<soft>").count(),
            note.matches("</soft>").count()
        );

        // One marked phrase longer than the limit is cut by its words, its tag closed.
        let endless = format!("<soft>{} конец.</soft>", "а".repeat(600));
        assert_eq!(
            voice_note("x", Some(&endless), None),
            Some(format!(
                "<soft>{}</soft>\n{VOICE_NOTE_TAIL}",
                "а".repeat(VOICE_NOTE_CHARS)
            ))
        );
    }

    #[test]
    fn the_marks_of_a_voice_note_have_a_limit_of_their_own() {
        let direction = "[softly, with a long and heavy sigh] ";
        // Three words under thousands of characters of marks: the words alone.
        let heavy = format!("{}Да.", direction.repeat(300));
        assert!(heavy.chars().count() > 10_000);
        assert_eq!(
            voice_note("Да.", Some(&heavy), None).as_deref(),
            Some("Да.")
        );

        // Many short phrases, each with a direction: the first keep theirs,
        // the rest are said plainly, and nothing of the words is lost.
        let many = format!("{direction}Вот так вот. ").repeat(30);
        let note = voice_note("x", Some(&many), None).unwrap();
        let kept = note.matches("[softly").count();
        assert_eq!(kept, VOICE_NOTE_MARKS / direction.chars().count());
        assert_eq!(note.matches("Вот так вот.").count(), 30);
        assert!(
            note.chars().count() <= VOICE_NOTE_CHARS + VOICE_NOTE_MARKS + 30,
            "{}",
            note.chars().count()
        );

        // One endless phrase whose marks alone pass the limit: its words, cut.
        let endless = format!("{}{} конец.", direction.repeat(40), "а".repeat(600));
        assert_eq!(
            voice_note("x", Some(&endless), None),
            Some(format!(
                "{}\n{VOICE_NOTE_TAIL}",
                "а".repeat(VOICE_NOTE_CHARS)
            ))
        );

        // Words that only look like a mark stay harmless without the marks.
        let tricky = format!("{}Смотри [1] и <2>.", direction.repeat(40));
        assert_eq!(
            voice_note("x", Some(&tricky), None).as_deref(),
            Some("Смотри (1) и 2.")
        );
    }

    #[test]
    fn the_mood_direction_leads_the_first_line_of_a_voice_note() {
        let joy = Some("cheerful, with a smile");
        assert_eq!(
            voice_note("Ну вот. Всё готово, смотри.", None, joy).as_deref(),
            Some("[cheerful, with a smile] Ну вот. Всё готово, смотри.")
        );
        // A first line the model marked itself keeps its own direction.
        assert_eq!(
            voice_note("x", Some("[sigh] Ну вот, всё готово."), joy).as_deref(),
            Some("[sigh] Ну вот, всё готово.")
        );
        // Only the first line gets it.
        assert_eq!(
            voice_note(
                "x",
                Some("Ну вот, всё готово. [sigh] Смотри сам, пожалуйста."),
                joy
            )
            .as_deref(),
            Some("[cheerful, with a smile] Ну вот, всё готово.\n[sigh] Смотри сам, пожалуйста.")
        );
        // Nothing to say stays nothing.
        assert_eq!(voice_note("😀", None, joy), None);
    }

    /// A voice that only remembers the direction source it was given.
    #[derive(Default)]
    struct DirectedVoice {
        direction: Mutex<Option<DirectionSource>>,
    }

    impl Voice for DirectedVoice {
        fn speak(&self, _text: &str) {}
        fn stop(&self) {}
        fn voices(&self) -> Vec<VoiceInfo> {
            Vec::new()
        }
        fn configure(&self, _voice: &str, _rate: f32) {}
        fn set_direction(&self, direction: Option<DirectionSource>) {
            *self.direction.lock() = direction;
        }
    }

    #[test]
    fn a_wrapped_voice_passes_the_direction_source_on() {
        let inner = Arc::new(DirectedVoice::default());
        let voice = SpeakerSwitch::new(true).wrap(inner.clone());
        voice.set_direction(Some(Arc::new(|| Some("sad, quietly".to_string()))));
        let got = inner.direction.lock().clone().expect("passed on");
        assert_eq!(got().as_deref(), Some("sad, quietly"));
        voice.set_direction(None);
        assert!(inner.direction.lock().is_none());
    }

    #[tokio::test]
    async fn the_speaker_itself_adds_no_direction() {
        // The direction is the business of the voice that asks the remote
        // synthesizer: what the speaker hands over is the same as before.
        let bus = EventBus::new(64);
        let inner = Arc::new(FakeVoice::default());
        let voice = SpeakerSwitch::new(true).wrap(inner.clone());
        voice.set_direction(Some(Arc::new(|| Some("sad, quietly".to_string()))));
        let job = tokio::spawn(run_speaker(bus.clone(), voice, SpeakerSwitch::new(true)));
        tokio::time::sleep(Duration::from_millis(10)).await;
        bus.publish(marked_text(
            "Хорошо, договорились на завтра. До встречи утром!",
            "[warmly] Хорошо, договорились на завтра. <soft>До встречи утром!</soft> [sigh]",
        ));
        bus.publish(Event::AssistantText {
            text: "Завтра ясно.".into(),
            spoken: None,
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        job.abort();
        assert_eq!(
            *inner.marked.lock(),
            [
                pair(
                    "[warmly] Хорошо, договорились на завтра.",
                    "Хорошо, договорились на завтра."
                ),
                pair(
                    "[warmly] <soft>До встречи утром!</soft>",
                    "До встречи утром!"
                ),
            ]
        );
        assert_eq!(*inner.said.lock(), ["Завтра ясно."]);
    }
}
