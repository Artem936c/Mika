//! Open confirmation requests. One at a time: tools run one after another.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::events::{Event, EventBus};
use crate::policy::forever_block;
use crate::state::{AssistantState, StateHandle};
use crate::tools::Assessment;
#[cfg(test)]
use crate::tools::RiskLevel;

pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Once,
    Always,
    Deny,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    Rule,
    Button,
    Chat,
    Timeout,
    Policy,
    Telegram,
    Voice,
}

impl DecidedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedBy::Rule => "rule",
            DecidedBy::Button => "button",
            DecidedBy::Chat => "chat",
            DecidedBy::Timeout => "timeout",
            DecidedBy::Policy => "policy",
            DecidedBy::Telegram => "telegram",
            DecidedBy::Voice => "voice",
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfirmError {
    #[error("этот запрос уже закрыт")]
    NotPending,
    #[error("{0}")]
    ForeverNotAllowed(&'static str),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PendingView {
    pub id: u64,
    pub tool: String,
    pub assessment: Assessment,
    pub allow_always: bool,
}

struct Pending {
    view: PendingView,
    tx: oneshot::Sender<(Answer, DecidedBy)>,
}

pub struct Confirmations {
    bus: EventBus,
    state: StateHandle,
    next_id: AtomicU64,
    pending: Mutex<Option<Pending>>,
}

/// Closes the request if the asking future is dropped (the reply was stopped).
struct DropGuard<'a> {
    owner: &'a Confirmations,
    id: u64,
    done: bool,
}

impl Drop for DropGuard<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut p = self.owner.pending.lock();
        if p.as_ref().is_some_and(|p| p.view.id == self.id) {
            *p = None;
            drop(p);
            self.owner.bus.publish(Event::ConfirmResolved {
                id: self.id,
                answer: Answer::Deny,
                by: DecidedBy::Policy,
            });
        }
    }
}

impl Confirmations {
    pub fn new(bus: EventBus, state: StateHandle) -> Arc<Self> {
        Arc::new(Self {
            bus,
            state,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(None),
        })
    }

    /// Opens a request and waits for an answer or the timeout.
    pub async fn ask(
        &self,
        tool: &str,
        assessment: &Assessment,
        timeout: Duration,
    ) -> (Answer, DecidedBy) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let view = PendingView {
            id,
            tool: tool.into(),
            assessment: assessment.clone(),
            allow_always: forever_block(assessment.level, &assessment.scope).is_none(),
        };
        *self.pending.lock() = Some(Pending {
            view: view.clone(),
            tx,
        });
        let mut guard = DropGuard {
            owner: self,
            id,
            done: false,
        };
        self.state.set(AssistantState::AwaitingConfirm);
        self.bus.publish(Event::ConfirmRequested {
            id,
            tool: tool.into(),
            assessment: assessment.clone(),
            allow_always: view.allow_always,
            timeout_s: timeout.as_secs(),
        });

        let result = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => (Answer::Deny, DecidedBy::Policy),
            Err(_) => (Answer::Timeout, DecidedBy::Timeout),
        };
        {
            let mut p = self.pending.lock();
            if p.as_ref().is_some_and(|p| p.view.id == id) {
                *p = None;
            }
        }
        guard.done = true;
        self.bus.publish(Event::ConfirmResolved {
            id,
            answer: result.0,
            by: result.1,
        });
        self.state.set(AssistantState::Thinking);
        result
    }

    pub fn answer(&self, id: u64, answer: Answer, by: DecidedBy) -> Result<(), ConfirmError> {
        let mut slot = self.pending.lock();
        let Some(p) = slot.as_ref().filter(|p| p.view.id == id) else {
            return Err(ConfirmError::NotPending);
        };
        if answer == Answer::Always {
            if let Some(why) = forever_block(p.view.assessment.level, &p.view.assessment.scope) {
                return Err(ConfirmError::ForeverNotAllowed(why));
            }
        }
        let p = slot.take().expect("checked above");
        p.tx.send((answer, by))
            .map_err(|_| ConfirmError::NotPending)
    }

    pub fn pending(&self) -> Option<PendingView> {
        self.pending.lock().as_ref().map(|p| p.view.clone())
    }

    /// Denies the open request, if any.
    pub fn deny_pending(&self, by: DecidedBy) {
        if let Some(p) = self.pending() {
            let _ = self.answer(p.id, Answer::Deny, by);
        }
    }
}

/// Lower case, `ё` as `е`, words separated by single spaces, no punctuation.
pub fn clean(text: &str) -> String {
    text.to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Chat phrases that answer an open request. Only whole phrases count.
/// Russian and English phrases work in both interface languages.
pub fn parse_answer(text: &str) -> Option<Answer> {
    const ALWAYS: &[&str] = &[
        "всегда",
        "разреши всегда",
        "разрешаю всегда",
        "разрешать всегда",
        "всегда разрешай",
        "всегда разрешать",
        "да всегда",
        "always",
        "always allow",
        "allow always",
        "yes always",
    ];
    const ONCE: &[&str] = &[
        "да",
        "ага",
        "ок",
        "окей",
        "ok",
        "yes",
        "подтверждаю",
        "разреши",
        "разрешаю",
        "можно",
        "давай",
        "да давай",
        "да разрешаю",
        "выполняй",
        "делай",
        "один раз",
        "разреши один раз",
        "разрешить один раз",
        "okay",
        "yep",
        "sure",
        "allow",
        "allow once",
        "once",
        "go ahead",
        "do it",
        "confirm",
    ];
    const DENY: &[&str] = &[
        "нет",
        "no",
        "отмена",
        "отмени",
        "не надо",
        "не нужно",
        "отклони",
        "отклоняю",
        "запрещаю",
        "нельзя",
        "стоп",
        "нет не надо",
        "nope",
        "cancel",
        "deny",
        "stop",
        // «don't» as `clean` gives it.
        "don t",
        "do not",
        "never mind",
    ];
    let t = clean(text);
    if ALWAYS.contains(&t.as_str()) {
        Some(Answer::Always)
    } else if ONCE.contains(&t.as_str()) {
        Some(Answer::Once)
    } else if DENY.contains(&t.as_str()) {
        Some(Answer::Deny)
    } else {
        None
    }
}

/// Words that take a request back or question it. One anywhere in a spoken
/// answer denies the request: «Подожди, выключать компьютер не надо» is a no.
const STOP_WORDS: &[&str] = &[
    "не",
    "нет",
    "неа",
    "ненадо",
    "ненужно",
    "нельзя",
    "стой",
    "стоп",
    "подожди",
    "подождите",
    "погоди",
    "погодите",
    "отмена",
    "отмени",
    "отмените",
    "отменить",
    "отклони",
    "отклоняю",
    "запрещаю",
    "зачем",
    "почему",
    "разве",
    "неужели",
    "ни",
    "рано",
    "потом",
    "позже",
    "попозже",
    "отложи",
    "перестань",
    "хватит",
    "no",
    "stop",
    "cancel",
    "not",
    // «don't» as `clean` gives it, and written without the apostrophe.
    "don",
    "dont",
    "never",
    "nope",
    "deny",
    "wait",
    "later",
    "why",
];

/// The one English answer that allows an action forever in the chat but not
/// when spoken: a single common word. By voice a rule needs «always allow»,
/// «allow always» or «yes always».
const CHAT_ONLY_ALWAYS: &str = "always";

/// Prepositions and conjunctions: they need not be said and may be added.
const FILLERS: &[&str] = &[
    "в", "во", "на", "из", "с", "со", "к", "ко", "по", "о", "об", "у", "от", "до", "за", "и",
];

/// Words of agreement that may come with the name of a high-risk action.
const CONSENT: &[&str] = &[
    "да",
    "ну",
    "давай",
    "подтверждаю",
    "разрешаю",
    "ага",
    "ок",
    "окей",
    "хорошо",
    "пожалуйста",
];

/// Endings that change with case and number. A spoken word is a form of a word
/// of the name when both are one base with one of these endings.
const ENDINGS: &[&str] = &[
    "а", "я", "о", "е", "и", "ы", "у", "ю", "й", "ь", "ам", "ям", "ами", "ями",
];

/// Infinitive endings. The imperative of such a verb may change the last
/// consonant of the base: «напис-ать», «напиш-и».
const INFINITIVE: &[&str] = &["ть", "ти", "ать", "ять", "ить", "еть", "уть", "ыть"];

/// Endings of the imperative. Other forms of a verb are not agreement:
/// «выключу» is a promise, «выключать» comes with «рано» or «не надо».
const IMPERATIVE: &[&str] = &["и", "ите", "ь", "ьте", "й", "йте"];

/// Endings added to a man's name, «Валентин», «Евгений»: «Валентину»,
/// «Евгению». Not «Валентине», «Евгении»: those are a woman's name.
const HIS: &[&str] = &["у", "ю", "ом", "ем"];

/// Endings that replace the last letter of a woman's name, «Марина»,
/// «Мария»: «Марине», «Марии». Not «Марину»: «Валентину» is a man's name.
const HERS: &[&str] = &["е", "и", "ы", "ой", "ей", "ою", "ею"];

/// Last consonants of a verb base and what they become in other forms.
const ALTERNATIONS: &[(char, char)] = &[
    ('с', 'ш'),
    ('з', 'ж'),
    ('т', 'ч'),
    ('т', 'щ'),
    ('к', 'ч'),
    ('г', 'ж'),
    ('д', 'ж'),
    ('х', 'ш'),
];

/// Words of a cleaned phrase as they are compared, each with its readings:
/// Latin read the Russian way, `э` as `е`, number words as digits.
fn words(cleaned: &str) -> Vec<Vec<String>> {
    cleaned
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let readings = if w.is_ascii() {
                latin_readings(w)
            } else {
                vec![w.replace('э', "е")]
            };
            readings
                .into_iter()
                .map(|r| number(&r).map_or(r, String::from))
                .collect()
        })
        .collect()
}

/// How recognition may write a Latin word in Cyrillic: «PowerShell» is
/// «пауэршелл» or «повершелл», and a doubled last letter may come single.
fn latin_readings(word: &str) -> Vec<String> {
    let mut out = vec![translit(word)];
    if word.contains("ow") {
        out.push(translit(&word.replace("ow", "ov")));
    }
    let singles: Vec<String> = out
        .iter()
        .filter_map(|r| single(r))
        .map(String::from)
        .collect();
    out.extend(singles);
    out
}

/// A word without its doubled last consonant: «телеграмм» is «телеграм».
fn single(word: &str) -> Option<&str> {
    let mut chars = word.chars();
    let last = chars.next_back()?;
    let head = chars.as_str();
    (head.ends_with(last) && consonant(last)).then_some(head)
}

/// A Russian consonant other than «й».
fn consonant(c: char) -> bool {
    "бвгджзклмнпрстфхцчшщ".contains(c)
}

/// A spoken number word as digits: recognition writes «маме два» for «Мама 2».
fn number(word: &str) -> Option<&'static str> {
    Some(match word {
        "ноль" | "нуль" => "0",
        "один" | "одна" | "одно" => "1",
        "два" | "две" => "2",
        "три" => "3",
        "четыре" => "4",
        "пять" => "5",
        "шесть" => "6",
        "семь" => "7",
        "восемь" => "8",
        "девять" => "9",
        "десять" => "10",
        _ => return None,
    })
}

/// Splits of a word into a base and one of `ENDINGS`, the whole word first.
fn bases(word: &str) -> impl Iterator<Item = (&str, &'static str)> + '_ {
    std::iter::once((word, "")).chain(
        ENDINGS
            .iter()
            .filter_map(move |e| word.strip_suffix(e).map(|base| (base, *e))),
    )
}

/// Stems of the imperative of a verb in the infinitive: «выключ» for
/// «выключить», «напиш» for «написать», «выйд» for «выйти». `None` for other
/// words.
fn imperative_stems(word: &str) -> Option<Vec<String>> {
    let mut stems = Vec::new();
    for ending in INFINITIVE {
        let Some(base) = word.strip_suffix(ending) else {
            continue;
        };
        if base.chars().count() < 3 {
            continue;
        }
        stems.push(base.to_string());
        let mut chars = base.chars();
        if let Some(last) = chars.next_back() {
            let head = chars.as_str();
            stems.extend(
                ALTERNATIONS
                    .iter()
                    .filter(|(from, _)| *from == last)
                    .map(|(_, to)| format!("{head}{to}")),
            );
        }
        match *ending {
            // «выйти»: «выйди».
            "ти" => stems.push(format!("{base}д")),
            // «открыть»: «открой».
            "ыть" => stems.push(format!("{base}о")),
            // «переименовать»: «переименуй».
            "ать" => {
                if let Some(head) = base.strip_suffix("ов") {
                    stems.push(format!("{head}у"));
                }
            }
            _ => {}
        }
    }
    (!stems.is_empty()).then_some(stems)
}

/// Whether a spoken word is a person's name said without turning it into
/// another person's: «Валентине» is not «Валентин», «Валентину» is not
/// «Валентина». `None` when the ending does not tell a man from a woman.
fn name_form(name: &str, spoken: &str) -> Option<bool> {
    let mut chars = name.chars();
    let last = chars.next_back()?;
    let (stem, endings) = match last {
        'а' | 'я' => (chars.as_str(), HERS),
        'й' => (chars.as_str(), HIS),
        c if consonant(c) => (name, HIS),
        _ => return None,
    };
    Some(
        spoken == name
            || spoken
                .strip_prefix(stem)
                .is_some_and(|ending| endings.contains(&ending)),
    )
}

/// Whether a spoken word is a form of a word of the action name. A word under
/// three letters, like «Ян» or «2», must be said as it is. A verb counts only
/// in the infinitive or the imperative. In `target`, whom the action is for, a
/// person's name keeps its gender. Other words must have the same base, not
/// just start alike: «перезагрузить» is not «переведи».
fn same_word(name: &str, target: bool, spoken: &str) -> bool {
    if name == spoken {
        return true;
    }
    if name.chars().count() < 3 {
        return false;
    }
    if !target {
        if let Some(stems) = imperative_stems(name) {
            return stems.iter().any(|stem| {
                spoken
                    .strip_prefix(stem.as_str())
                    .is_some_and(|ending| IMPERATIVE.contains(&ending))
            });
        }
    } else if let Some(same) = name_form(name, spoken) {
        return same;
    }
    bases(name).any(|(base, _)| {
        base.chars().count() >= 3
            && bases(spoken).any(|(said, _)| said == base || single(said) == Some(base))
    })
}

/// Latin letters read the Russian way: recognition writes «телеграм», the
/// action name says «Telegram». Letter pairs go first: «sh» is «ш», «ya» is «я».
/// Digits stay.
fn translit(word: &str) -> String {
    const PAIRS: &[(&str, &str)] = &[
        ("shch", "щ"),
        ("sh", "ш"),
        ("ch", "ч"),
        ("zh", "ж"),
        ("kh", "х"),
        ("ts", "ц"),
        ("ph", "ф"),
        ("ck", "к"),
        ("ya", "я"),
        ("yu", "ю"),
        ("yo", "е"),
        ("ye", "е"),
        ("ow", "ау"),
    ];
    let consonant =
        |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphabetic() && !"aeiouy".contains(c));
    let mut out = String::new();
    let mut rest = word;
    let mut prev = None;
    'next: while let Some(c) = rest.chars().next() {
        // «Maria», «Yulia»: a final «ia» is «ия».
        if rest == "ia" {
            out.push_str("ия");
            break;
        }
        for (latin, ru) in PAIRS {
            if let Some(after) = rest.strip_prefix(latin) {
                out.push_str(ru);
                rest = after;
                prev = latin.chars().last();
                continue 'next;
            }
        }
        rest = &rest[c.len_utf8()..];
        let after_consonant = consonant(prev);
        prev = Some(c);
        let ru = match c {
            // «Dmitry», «Evgeny»: a final «y» after a consonant is «ий», the
            // way these names are written in Russian. Inside a word it is «и».
            'y' if after_consonant && rest.is_empty() => "ий",
            'y' if after_consonant => "и",
            'a' => "а",
            'b' => "б",
            'c' | 'k' | 'q' => "к",
            'd' => "д",
            'e' => "е",
            'f' => "ф",
            'g' => "г",
            'h' => "х",
            'i' => "и",
            'j' => "дж",
            'l' => "л",
            'm' => "м",
            'n' => "н",
            'o' => "о",
            'p' => "п",
            'r' => "р",
            's' => "с",
            't' => "т",
            'u' => "у",
            'v' | 'w' => "в",
            'x' => "кс",
            'y' => "й",
            'z' => "з",
            other => {
                out.push(other);
                continue;
            }
        };
        out.push_str(ru);
    }
    out
}

/// Whether the cleaned spoken phrase is the name of the action and nothing
/// more: every word of the name is there, endings and the alphabet aside, and
/// besides them only prepositions and agreement. Prepositions may be left out.
/// Any other word may change the meaning: «через час», «маме 2» for «Мама»,
/// «ты точно хочешь».
fn says_name(spoken: &str, name: &str) -> bool {
    // After a colon comes whom the action is for: «Написать в Telegram: Марина».
    let (action, target) = name.split_once(':').unwrap_or((name, ""));
    let want: Vec<(Vec<String>, bool)> = words(&clean(action))
        .into_iter()
        .map(|w| (w, false))
        .chain(words(&clean(target)).into_iter().map(|w| (w, true)))
        .filter(|(w, _)| !w.iter().any(|r| FILLERS.contains(&r.as_str())))
        .collect();
    let got = words(spoken);
    let names = |said: &[String], (word, target): &(Vec<String>, bool)| {
        word.iter()
            .any(|n| said.iter().any(|s| same_word(n, *target, s)))
    };
    let allowed = |said: &[String]| {
        said.iter()
            .any(|s| FILLERS.contains(&s.as_str()) || CONSENT.contains(&s.as_str()))
            || want.iter().any(|w| names(said, w))
    };
    !want.is_empty()
        && want.iter().all(|w| got.iter().any(|s| names(s, w)))
        && got.iter().all(|s| allowed(s))
}

/// What a spoken phrase means for an open request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceAnswer {
    Answer(Answer),
    /// High risk: the user must say the name of the action. Holds that name.
    NeedName(String),
    /// Not an answer at all.
    Unclear,
}

/// Rules for answering by voice (design, section 8). Recognition adds words and
/// punctuation, so the match is looser than for typed text, except where it matters:
/// a high-risk action is approved only by saying its name, never by "yes", and
/// with no other words than prepositions and agreement.
/// A negation or a stop word anywhere denies at any risk; a question never
/// approves a high-risk action.
pub fn voice_answer(text: &str, assessment: &Assessment) -> VoiceAnswer {
    const ONCE_ANY: &[&str] = &["подтверждаю", "разрешаю", "разреши", "выполняй"];
    let t = clean(text);
    let said: Vec<&str> = t.split(' ').collect();
    if parse_answer(text) == Some(Answer::Deny) || said.iter().any(|w| STOP_WORDS.contains(w)) {
        return VoiceAnswer::Answer(Answer::Deny);
    }
    if assessment.level == crate::tools::RiskLevel::High {
        let question = text.contains(['?', '？']);
        return if !question && says_name(&t, &assessment.summary) {
            VoiceAnswer::Answer(Answer::Once)
        } else {
            VoiceAnswer::NeedName(assessment.summary.clone())
        };
    }
    match parse_answer(text) {
        // One English word said in passing must not write a permanent rule:
        // by voice a rule needs the phrase, «always allow». The chat takes the word.
        Some(Answer::Always) if t == CHAT_ONLY_ALWAYS => VoiceAnswer::Unclear,
        Some(answer) => VoiceAnswer::Answer(answer),
        None if said.iter().any(|w| ONCE_ANY.contains(w)) || said.first() == Some(&"да") => {
            VoiceAnswer::Answer(Answer::Once)
        }
        None => VoiceAnswer::Unclear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn risk(level: RiskLevel, summary: &str) -> Assessment {
        Assessment::new(level, summary, "цель", "причина")
    }

    #[test]
    fn voice_yes_approves_low_and_medium_risk() {
        let a = risk(RiskLevel::Medium, "Закрыть «Блокнот»");
        for phrase in ["Да.", "Подтверждаю!", "Да, подтверждаю.", "ну разрешаю"]
        {
            assert_eq!(
                voice_answer(phrase, &a),
                VoiceAnswer::Answer(Answer::Once),
                "{phrase}"
            );
        }
        assert_eq!(
            voice_answer("Разреши всегда", &a),
            VoiceAnswer::Answer(Answer::Always)
        );
        assert_eq!(voice_answer("какая погода", &a), VoiceAnswer::Unclear);
    }

    #[test]
    fn voice_high_risk_needs_the_action_name() {
        let a = risk(RiskLevel::High, "Выключить компьютер");
        for phrase in ["Да", "Подтверждаю", "Разреши всегда", "выключить"]
        {
            assert_eq!(
                voice_answer(phrase, &a),
                VoiceAnswer::NeedName("Выключить компьютер".into()),
                "{phrase}"
            );
        }
        assert_eq!(
            voice_answer("Да, выключить компьютер.", &a),
            VoiceAnswer::Answer(Answer::Once)
        );
    }

    #[test]
    fn the_name_matches_despite_endings_and_alphabet() {
        let a = risk(RiskLevel::High, "Написать в Telegram: алкогеймеры");
        for phrase in [
            "Написать в телеграм алкогеймеры.",
            "Да, напиши в Телеграме алкогеймерам",
            "написать в Telegram алкогеймеры",
        ] {
            assert_eq!(
                voice_answer(phrase, &a),
                VoiceAnswer::Answer(Answer::Once),
                "{phrase}"
            );
        }
        for phrase in ["Да", "написать в телеграм", "отправь алкогеймерам"]
        {
            assert!(
                matches!(voice_answer(phrase, &a), VoiceAnswer::NeedName(_)),
                "{phrase}"
            );
        }
        assert_eq!(translit("telegram2"), "телеграм2");
    }

    #[test]
    fn a_negation_or_a_question_never_approves_high_risk() {
        let a = risk(RiskLevel::High, "Выключить компьютер");
        for phrase in [
            "Подожди, выключать компьютер не надо",
            "Зачем выключать компьютер?",
            "Выключить компьютер не надо",
            "Выключить компьютер? Нет",
            "выключить компьютер, стой",
            "Погоди, выключить компьютер",
            "Не нужно выключить компьютер",
            "Отмени, выключить компьютер",
            "Перестань выключать компьютер",
            "Стоп, выключить компьютер",
        ] {
            assert_eq!(
                voice_answer(phrase, &a),
                VoiceAnswer::Answer(Answer::Deny),
                "{phrase}"
            );
        }
        // Putting off, refusing or asking: never agreement.
        for phrase in [
            "Рано ещё выключать компьютер",
            "Выключать компьютер пока рано",
            "Я потом сама выключу компьютер",
            "Ни в коем случае выключать компьютер",
            "Почему выключить компьютер",
            "Разве выключить компьютер",
            "Выключи компьютер позже",
        ] {
            assert_eq!(
                voice_answer(phrase, &a),
                VoiceAnswer::Answer(Answer::Deny),
                "{phrase}"
            );
        }
        // Another form of the verb or a word more: the name is not said.
        for phrase in [
            "Выключить компьютер?",
            "Выключить компьютер？",
            "Ты точно хочешь выключить компьютер",
            "Выключи компьютер через час",
            "Выключать компьютер",
            "Выключу компьютер",
            "Выключит компьютер",
            "Выключай компьютер",
            "Мика выключит компьютер",
        ] {
            assert_eq!(
                voice_answer(phrase, &a),
                VoiceAnswer::NeedName("Выключить компьютер".into()),
                "{phrase}"
            );
        }
        for phrase in [
            "Выключи компьютер",
            "Выключите компьютер",
            "Ну давай, выключи компьютер, пожалуйста",
            "Подтверждаю: выключить компьютер",
        ] {
            assert_eq!(
                voice_answer(phrase, &a),
                VoiceAnswer::Answer(Answer::Once),
                "{phrase}"
            );
        }
        let medium = risk(RiskLevel::Medium, "Закрыть «Блокнот»");
        assert_eq!(
            voice_answer("Да нет, не надо", &medium),
            VoiceAnswer::Answer(Answer::Deny)
        );
    }

    #[test]
    fn another_action_or_contact_does_not_match_the_name() {
        let restart = risk(RiskLevel::High, "Перезагрузить компьютер");
        assert_eq!(
            voice_answer("Переведи компьютер в спящий режим", &restart),
            VoiceAnswer::NeedName("Перезагрузить компьютер".into())
        );
        assert_eq!(
            voice_answer("Перезагрузи компьютер", &restart),
            VoiceAnswer::Answer(Answer::Once)
        );

        let cases = [
            // (contact chosen by the model, phrase, approves)
            ("Марина", "Напиши в телеграм Марии", false),
            ("Марина", "Напиши в телеграм Марине", true),
            ("Мария", "Напиши в телеграм Марии", true),
            ("Александр", "Напиши в телеграм Алексею", false),
            ("Алексей", "Напиши в телеграм Александру", false),
            ("Алексей", "Напиши в телеграм Алексею", true),
            ("Ян", "Напиши в телеграм Ивану", false),
            ("Ян", "Напиши в телеграм Яне", false),
            ("Ян", "Напиши в телеграм Ян", true),
            ("Мама 2", "Напиши в телеграм маме", false),
            ("Мама 2", "Напиши в телеграм маме 3", false),
            ("Мама 2", "Напиши в телеграм маме 2", true),
            ("Мама 2", "Напиши в телеграм маме два", true),
            // A man's name said as a woman's is another person, and back.
            ("Валентин", "Напиши в телеграм Валентине", false),
            ("Валентин", "Напиши в телеграм Валентину", true),
            ("Валентина", "Напиши в телеграм Валентину", false),
            ("Валентина", "Напиши в телеграм Валентине", true),
            ("Евгений", "Напиши в телеграм Евгении", false),
            ("Евгений", "Напиши в телеграм Евгению", true),
            ("Александр", "Напиши в телеграм Александре", false),
            ("Александр", "Напиши в телеграм Александру", true),
            ("Аня", "Напиши в телеграм Ане", true),
            ("Анна", "Напиши в телеграм Ане", false),
            // A word more is another contact.
            ("Мама", "Напиши в телеграм маме 2", false),
            ("Мама", "Напиши в телеграм маме и папе", false),
            ("Мама", "Напиши в телеграм маме", true),
        ];
        for (contact, phrase, approves) in cases {
            let a = risk(RiskLevel::High, &format!("Написать в Telegram: {contact}"));
            let got = voice_answer(phrase, &a);
            if approves {
                assert_eq!(
                    got,
                    VoiceAnswer::Answer(Answer::Once),
                    "{contact}: {phrase}"
                );
            } else {
                assert!(
                    matches!(got, VoiceAnswer::NeedName(_)),
                    "{contact}: {phrase}: {got:?}"
                );
            }
        }
    }

    #[test]
    fn high_risk_names_are_confirmed_by_the_imperative() {
        for (name, phrase) in [
            ("Выйти из учётной записи", "Выйди из учётной записи"),
            ("Выйти из учётной записи", "Выйти из учетной записи"),
            (
                "Принудительно завершить процесс",
                "Да, принудительно заверши процесс",
            ),
            ("Открыть", "Открой"),
            ("Удалить в корзину", "Удали в корзину"),
            ("Выполнить команду PowerShell", "Выполни команду PowerShell"),
            ("Переименовать", "Переименуй"),
        ] {
            assert_eq!(
                voice_answer(phrase, &risk(RiskLevel::High, name)),
                VoiceAnswer::Answer(Answer::Once),
                "{name}: {phrase}"
            );
        }
        for (name, phrase) in [
            ("Открыть", "Открою"),
            ("Удалить в корзину", "Удалю в корзину"),
            ("Удалить в корзину", "Удалять в корзину"),
        ] {
            assert!(
                matches!(
                    voice_answer(phrase, &risk(RiskLevel::High, name)),
                    VoiceAnswer::NeedName(_)
                ),
                "{name}: {phrase}"
            );
        }
    }

    #[test]
    fn latin_letter_pairs_read_the_russian_way() {
        for (latin, ru) in [
            ("sasha", "саша"),
            ("yulia", "юлия"),
            ("maria", "мария"),
            ("zhenya", "женя"),
            ("mikhail", "михаил"),
            ("shchukin", "щукин"),
            ("tsoy", "цой"),
            ("chat", "чат"),
            ("powershell", "пауершелл"),
            ("telegram2", "телеграм2"),
            ("dmitry", "дмитрий"),
            ("evgeny", "евгений"),
            ("yury", "юрий"),
            ("dmitriy", "дмитрий"),
            ("sergey", "сергей"),
            ("lynn", "линн"),
        ] {
            assert_eq!(translit(latin), ru, "{latin}");
        }
        for (name, phrase) in [
            ("Написать в Telegram: Sasha", "Напиши в телеграм Саше"),
            ("Написать в Telegram: Yulia", "написать в телеграм Юлии"),
            ("Написать в Telegram: Dmitry", "Напиши в телеграм Дмитрию"),
            ("Написать в Telegram: Evgeny", "Напиши в телеграм Евгению"),
            ("Написать в Telegram: Аня", "Написать в телеграмм Аня"),
            ("Написать в Telegram: Аня", "Напиши в телеграмме Ане"),
            (
                "Выполнить команду PowerShell",
                "Выполнить команду пауэршелл",
            ),
            (
                "Выполнить команду PowerShell",
                "Выполнить команду повершелл",
            ),
            ("Выполнить команду PowerShell", "Выполни команду пауэршел"),
        ] {
            assert_eq!(
                voice_answer(phrase, &risk(RiskLevel::High, name)),
                VoiceAnswer::Answer(Answer::Once),
                "{name}: {phrase}"
            );
        }
        // Read the Russian way, a Latin name keeps its gender too.
        assert!(matches!(
            voice_answer(
                "Напиши в телеграм Евгении",
                &risk(RiskLevel::High, "Написать в Telegram: Evgeny")
            ),
            VoiceAnswer::NeedName(_)
        ));
    }

    #[test]
    fn voice_no_denies_any_risk() {
        for level in [RiskLevel::Low, RiskLevel::High] {
            let a = risk(level, "Выключить компьютер");
            for phrase in ["Нет.", "Отмена", "Нет, не выключай компьютер", "Стоп!"]
            {
                assert_eq!(
                    voice_answer(phrase, &a),
                    VoiceAnswer::Answer(Answer::Deny),
                    "{phrase}"
                );
            }
        }
    }

    fn setup() -> (
        Arc<Confirmations>,
        StateHandle,
        tokio::sync::broadcast::Receiver<Event>,
    ) {
        let bus = EventBus::new(64);
        let rx = bus.subscribe();
        let state = StateHandle::new(bus.clone());
        (Confirmations::new(bus, state.clone()), state, rx)
    }

    fn assessment(level: RiskLevel) -> Assessment {
        Assessment::new(level, "Сделать", "цель", "причина")
    }

    #[test]
    fn parse_answer_phrases() {
        for t in ["да", "Да!", "подтверждаю", "разреши", "ок"] {
            assert_eq!(parse_answer(t), Some(Answer::Once), "{t}");
        }
        for t in ["разреши всегда", "Всегда."] {
            assert_eq!(parse_answer(t), Some(Answer::Always), "{t}");
        }
        for t in ["нет", "Отмена", "не надо", "отклони"] {
            assert_eq!(parse_answer(t), Some(Answer::Deny), "{t}");
        }
        for t in ["да нет наверное", "привет", "удали всё", ""] {
            assert_eq!(parse_answer(t), None, "{t}");
        }
        // The English phrases the confirmation card names, and a few close ones.
        for t in ["yes", "OK.", "Allow once", "go ahead"] {
            assert_eq!(parse_answer(t), Some(Answer::Once), "{t}");
        }
        for t in ["always", "Always allow!"] {
            assert_eq!(parse_answer(t), Some(Answer::Always), "{t}");
        }
        for t in ["no", "Cancel", "don't", "never mind"] {
            assert_eq!(parse_answer(t), Some(Answer::Deny), "{t}");
        }
        for t in ["yes no maybe", "allow everything", "hello"] {
            assert_eq!(parse_answer(t), None, "{t}");
        }
    }

    #[test]
    fn voice_in_english_denies_by_one_word_and_approves_by_a_whole_phrase() {
        let medium = risk(RiskLevel::Medium, "Закрыть «Блокнот»");
        let low = risk(RiskLevel::Low, "Закрыть «Блокнот»");
        for (phrase, a) in [
            ("no, wait", &medium),
            ("don't do that", &medium),
            ("why?", &low),
            ("Not now, later", &medium),
            ("yes, but dont close it", &medium),
        ] {
            assert_eq!(
                voice_answer(phrase, a),
                VoiceAnswer::Answer(Answer::Deny),
                "{phrase}"
            );
        }
        assert_eq!(
            voice_answer("yes", &medium),
            VoiceAnswer::Answer(Answer::Once)
        );
        // A permanent rule by voice needs the phrase. The single word, which
        // anyone in the room may say in passing, is an answer in the chat only.
        for phrase in ["Always allow.", "allow always", "Yes, always!"] {
            assert_eq!(
                voice_answer(phrase, &medium),
                VoiceAnswer::Answer(Answer::Always),
                "{phrase}"
            );
        }
        for phrase in ["Always.", "always"] {
            assert_eq!(voice_answer(phrase, &medium), VoiceAnswer::Unclear);
            assert_eq!(voice_answer(phrase, &low), VoiceAnswer::Unclear);
            assert_eq!(parse_answer(phrase), Some(Answer::Always), "{phrase}");
        }
        // Every English phrase that approves once by voice, and nothing longer.
        for phrase in [
            "yes",
            "ok",
            "Okay.",
            "yep",
            "Sure.",
            "allow",
            "Allow once",
            "once",
            "Go ahead!",
            "do it",
            "confirm",
        ] {
            assert_eq!(
                voice_answer(phrase, &medium),
                VoiceAnswer::Answer(Answer::Once),
                "{phrase}"
            );
        }
        for phrase in ["sure thing", "okay then go", "allow it all", "confirm that"] {
            assert_eq!(
                voice_answer(phrase, &medium),
                VoiceAnswer::Unclear,
                "{phrase}"
            );
        }
        // Agreement with other words around it is not an answer.
        assert_eq!(
            voice_answer("yes please do it now", &medium),
            VoiceAnswer::Unclear
        );
        // High risk: English agreement never approves, the name is still needed.
        let high = risk(RiskLevel::High, "Выключить компьютер");
        for phrase in ["yes", "always allow", "go ahead"] {
            assert_eq!(
                voice_answer(phrase, &high),
                VoiceAnswer::NeedName("Выключить компьютер".into()),
                "{phrase}"
            );
        }
        assert_eq!(
            voice_answer("never mind", &high),
            VoiceAnswer::Answer(Answer::Deny)
        );
    }

    #[tokio::test]
    async fn answer_resolves_and_publishes() {
        let (c, state, mut rx) = setup();
        let c2 = c.clone();
        let task = tokio::spawn(async move {
            c2.ask("t", &assessment(RiskLevel::Low), CONFIRM_TIMEOUT)
                .await
        });
        tokio::task::yield_now().await;
        while c.pending().is_none() {
            tokio::task::yield_now().await;
        }
        assert_eq!(state.get(), AssistantState::AwaitingConfirm);
        let id = c.pending().unwrap().id;
        c.answer(id, Answer::Once, DecidedBy::Button).unwrap();
        assert_eq!(task.await.unwrap(), (Answer::Once, DecidedBy::Button));
        assert!(c.pending().is_none());
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ConfirmRequested {
                allow_always: true,
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ConfirmResolved {
                answer: Answer::Once,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn high_risk_always_is_rejected_and_request_stays_open() {
        let (c, _state, _rx) = setup();
        let c2 = c.clone();
        let task = tokio::spawn(async move {
            c2.ask("t", &assessment(RiskLevel::High), CONFIRM_TIMEOUT)
                .await
        });
        while c.pending().is_none() {
            tokio::task::yield_now().await;
        }
        let p = c.pending().unwrap();
        assert!(!p.allow_always);
        assert_eq!(
            c.answer(p.id, Answer::Always, DecidedBy::Chat),
            Err(ConfirmError::ForeverNotAllowed(
                crate::policy::HIGH_RISK_FOREVER
            ))
        );
        assert!(c.pending().is_some());
        c.answer(p.id, Answer::Deny, DecidedBy::Chat).unwrap();
        assert_eq!(task.await.unwrap().0, Answer::Deny);
    }

    #[tokio::test]
    async fn wildcard_scope_disables_always() {
        let (c, _state, _rx) = setup();
        let c2 = c.clone();
        let task = tokio::spawn(async move {
            let a = assessment(RiskLevel::Medium).with_scope("c:/docs/*");
            c2.ask("write_file", &a, CONFIRM_TIMEOUT).await
        });
        while c.pending().is_none() {
            tokio::task::yield_now().await;
        }
        let p = c.pending().unwrap();
        assert!(!p.allow_always);
        assert!(matches!(
            c.answer(p.id, Answer::Always, DecidedBy::Button),
            Err(ConfirmError::ForeverNotAllowed(_))
        ));
        c.answer(p.id, Answer::Once, DecidedBy::Button).unwrap();
        assert_eq!(task.await.unwrap().0, Answer::Once);
    }

    #[tokio::test]
    async fn timeout_denies_and_late_answer_is_rejected() {
        let (c, _state, _rx) = setup();
        let (answer, by) = c
            .ask("t", &assessment(RiskLevel::Low), Duration::from_millis(20))
            .await;
        assert_eq!((answer, by), (Answer::Timeout, DecidedBy::Timeout));
        assert_eq!(
            c.answer(1, Answer::Once, DecidedBy::Button),
            Err(ConfirmError::NotPending)
        );
    }

    #[tokio::test]
    async fn dropped_request_is_closed() {
        let (c, _state, mut rx) = setup();
        let a = assessment(RiskLevel::Low);
        {
            let fut = c.ask("t", &a, CONFIRM_TIMEOUT);
            let _ = tokio::time::timeout(Duration::from_millis(10), fut).await;
        }
        assert!(c.pending().is_none());
        let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::ConfirmResolved {
                answer: Answer::Deny,
                by: DecidedBy::Policy,
                ..
            }
        )));
    }
}
