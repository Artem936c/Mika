//! Quick commands (decision 0050): short frequent requests that run at once,
//! without the model: louder, pause, what time is it, a timer, open the
//! notepad, go to sleep.
//!
//! Only parsing lives here, and the short reply made from what the tool
//! gave: no side effects. A phrase counts only whole, after punctuation is
//! dropped and the name at its start is cut: «Мика, тише» is quick,
//! «сделай потише музыку в браузере» goes to the model. The phrases are of
//! the language of the interface; the sleep words are heard in both.

use serde_json::{json, Value};

use crate::i18n::{tr_args_in, tr_in, Lang};

/// A program the quick layer opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    /// The default browser, at a start page: Windows has no command that
    /// opens «the browser» as such.
    Browser,
    Explorer,
    Notepad,
    Calculator,
    TaskManager,
}

/// A site of the short list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    YouTube,
    Mail,
    Maps,
}

/// What a quick phrase asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickAction {
    VolumeUp,
    VolumeDown,
    /// The volume in percent, 0–100.
    VolumeSet(u8),
    Mute,
    Unmute,
    /// Pause the player. The key toggles: «пауза» and «продолжи» press the same key.
    MediaPause,
    MediaResume,
    MediaNext,
    MediaPrevious,
    Time,
    Date,
    /// A timer: a reminder in this many seconds.
    Timer {
        seconds: u32,
    },
    OpenApp(App),
    OpenSite(Site),
    /// She goes to sleep (decision 0050); no tool is called.
    Sleep,
    /// «Посмотри, как я играю»: the companion in games on (decision 0054),
    /// through `settings_set`, which asks every time and never from Telegram.
    CompanionOn,
    /// «Хватит комментировать»: the companion off.
    CompanionOff,
}

/// The longest timer the quick layer sets; a longer one is a reminder for the model.
pub const MAX_TIMER_SECONDS: u32 = 24 * 3600;

impl QuickAction {
    /// The name of the command, for the journal.
    pub fn name(self) -> &'static str {
        match self {
            QuickAction::VolumeUp => "volume_up",
            QuickAction::VolumeDown => "volume_down",
            QuickAction::VolumeSet(_) => "volume_set",
            QuickAction::Mute => "mute",
            QuickAction::Unmute => "unmute",
            QuickAction::MediaPause => "media_pause",
            QuickAction::MediaResume => "media_resume",
            QuickAction::MediaNext => "media_next",
            QuickAction::MediaPrevious => "media_previous",
            QuickAction::Time => "time",
            QuickAction::Date => "date",
            QuickAction::Timer { .. } => "timer",
            QuickAction::OpenApp(_) => "open_app",
            QuickAction::OpenSite(_) => "open_site",
            QuickAction::Sleep => "sleep",
            QuickAction::CompanionOn => "companion_on",
            QuickAction::CompanionOff => "companion_off",
        }
    }

    /// The tool call it makes: the tool name and its arguments, as the
    /// tool's schema asks for them. `None` for the sleep, which calls no tool.
    pub fn call(self, lang: Lang) -> Option<(&'static str, Value)> {
        let volume = |action: &str| Some(("volume", json!({ "action": action })));
        let media = |action: &str| Some(("media_control", json!({ "action": action })));
        match self {
            QuickAction::VolumeUp => volume("up"),
            QuickAction::VolumeDown => volume("down"),
            QuickAction::VolumeSet(level) => {
                Some(("volume", json!({ "action": "set", "level": level })))
            }
            QuickAction::Mute => volume("mute"),
            QuickAction::Unmute => volume("unmute"),
            QuickAction::MediaPause | QuickAction::MediaResume => media("play_pause"),
            QuickAction::MediaNext => media("next"),
            QuickAction::MediaPrevious => media("previous"),
            QuickAction::Time | QuickAction::Date => Some(("get_time", json!({}))),
            QuickAction::Timer { seconds } => {
                let text = tr_in(lang, "core.quick.timer_text");
                let args = if seconds % 60 == 0 {
                    json!({ "text": text, "in_minutes": seconds / 60 })
                } else {
                    json!({ "text": text, "in_seconds": seconds })
                };
                Some(("reminder_set", args))
            }
            QuickAction::OpenApp(App::Browser) => {
                Some(("open_url", json!({ "url": start_page(lang) })))
            }
            QuickAction::OpenApp(app) => Some(("open_app", json!({ "name": app_exe(app) }))),
            QuickAction::OpenSite(site) => {
                Some(("open_url", json!({ "url": site_url(site, lang) })))
            }
            QuickAction::Sleep => None,
            QuickAction::CompanionOn => Some((
                "settings_set",
                json!({ "key": "companion.enabled", "value": "on" }),
            )),
            QuickAction::CompanionOff => Some((
                "settings_set",
                json!({ "key": "companion.enabled", "value": "off" }),
            )),
        }
    }

    /// The short reply in `lang` once the call has ended. `ok`, `output` and
    /// `data` are what the tool gave; a call that failed or was refused is
    /// told in the tool's own words.
    pub fn reply(self, lang: Lang, ok: bool, output: &str, data: Option<&Value>) -> String {
        if !ok {
            return output.trim().to_string();
        }
        let key = |k: &str| tr_in(lang, k);
        match self {
            QuickAction::VolumeUp | QuickAction::VolumeDown | QuickAction::VolumeSet(_) => {
                match data.and_then(|d| d.get("level")).and_then(Value::as_u64) {
                    Some(level) => {
                        tr_args_in(lang, "core.quick.volume", &[("level", &level.to_string())])
                    }
                    None => output.trim().to_string(),
                }
            }
            QuickAction::Mute => key("core.quick.muted"),
            QuickAction::Unmute => key("core.quick.unmuted"),
            QuickAction::MediaPause => key("core.quick.paused"),
            QuickAction::MediaResume => key("core.quick.resumed"),
            QuickAction::MediaNext => key("core.quick.next"),
            QuickAction::MediaPrevious => key("core.quick.previous"),
            QuickAction::Time | QuickAction::Date => {
                time_reply(self == QuickAction::Date, lang, output)
                    .unwrap_or_else(|| output.trim().to_string())
            }
            QuickAction::Timer { seconds } => {
                tr_args_in(lang, "core.quick.timer", &[("span", &span(seconds, lang))])
            }
            QuickAction::OpenApp(_) | QuickAction::OpenSite(_) => key("core.quick.opening"),
            QuickAction::Sleep => key("core.quick.sleep"),
            QuickAction::CompanionOn => key("core.quick.companion_on"),
            QuickAction::CompanionOff => key("core.quick.companion_off"),
        }
    }
}

/// What `open_app` gets: the file name, which it knows in any language.
fn app_exe(app: App) -> &'static str {
    match app {
        App::Explorer => "explorer.exe",
        App::Notepad => "notepad.exe",
        App::Calculator => "calc.exe",
        App::TaskManager => "taskmgr.exe",
        App::Browser => "",
    }
}

/// The page the browser opens at, by the language of the interface.
fn start_page(lang: Lang) -> &'static str {
    match lang {
        Lang::Ru => "https://ya.ru",
        Lang::En => "https://www.google.com",
    }
}

fn site_url(site: Site, lang: Lang) -> &'static str {
    match (site, lang) {
        (Site::YouTube, _) => "https://www.youtube.com",
        (Site::Mail, Lang::Ru) => "https://mail.yandex.ru",
        (Site::Mail, Lang::En) => "https://mail.google.com",
        (Site::Maps, Lang::Ru) => "https://yandex.ru/maps",
        (Site::Maps, Lang::En) => "https://www.google.com/maps",
    }
}

/// «5 мин», «1 ч 30 мин», «30 с».
fn span(seconds: u32, lang: Lang) -> String {
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut parts = Vec::new();
    for (n, unit) in [
        (h, "core.quick.unit_h"),
        (m, "core.quick.unit_min"),
        (s, "core.quick.unit_s"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {}", tr_in(lang, unit)));
        }
    }
    parts.join(" ")
}

/// The time or the date from what `get_time` gave: `2026-11-22 14:05:09, …`.
fn time_reply(date: bool, lang: Lang, output: &str) -> Option<String> {
    use chrono::Datelike;
    let at = chrono::NaiveDateTime::parse_from_str(output.get(..19)?, "%Y-%m-%d %H:%M:%S").ok()?;
    if !date {
        let time = at.format("%H:%M").to_string();
        return Some(tr_args_in(lang, "core.quick.time", &[("time", &time)]));
    }
    // The names come from the locale file, comma separated, from January
    // and from Monday.
    let months = tr_in(lang, "core.quick.months");
    let days = tr_in(lang, "core.quick.weekdays");
    let month = months
        .split(',')
        .nth(at.month0() as usize)?
        .trim()
        .to_string();
    let weekday = days
        .split(',')
        .nth(at.weekday().num_days_from_monday() as usize)?
        .trim()
        .to_string();
    Some(tr_args_in(
        lang,
        "core.quick.date",
        &[
            ("day", &at.day().to_string()),
            ("month", &month),
            ("weekday", &weekday),
        ],
    ))
}

/// The words of a phrase: lower case, `ё` as `е`, without punctuation and
/// apostrophes (`what's` is `whats`).
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('ё', "е")
        .replace(['\'', '’'], "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && *w != "unk")
        .map(str::to_string)
        .collect()
}

/// Polite words that may stand at the start or the end.
const POLITE: &[&str] = &["пожалуйста", "please"];
/// Words that may stand before the name: «эй, Мика».
const CALLS: &[&str] = &["эй", "hey"];

/// The phrase without the name at its start and polite words at its ends.
fn trimmed(text: &str, names: &[&str]) -> Vec<String> {
    let mut w = words(text);
    for name in names {
        let name = words(name);
        if name.is_empty() {
            continue;
        }
        let skip = if w.first().is_some_and(|f| CALLS.contains(&f.as_str())) {
            1
        } else {
            0
        };
        if w.len() >= skip + name.len() && w[skip..skip + name.len()] == name[..] {
            w.drain(..skip + name.len());
            break;
        }
    }
    while w.first().is_some_and(|f| POLITE.contains(&f.as_str())) {
        w.remove(0);
    }
    while w.last().is_some_and(|l| POLITE.contains(&l.as_str())) {
        w.pop();
    }
    w
}

/// What the phrase asks for, if it is a quick command of `lang` and nothing
/// else. `names` are her names: one at the start is cut. The sleep words of
/// both languages count whatever the language.
pub fn match_quick(text: &str, lang: Lang, names: &[&str]) -> Option<QuickAction> {
    let w = trimmed(text, names);
    if w.is_empty() {
        return None;
    }
    let phrase = w.join(" ");
    if crate::pause::only_sleep_words(&phrase) || EN_SLEEP.contains(&phrase.as_str()) {
        return Some(QuickAction::Sleep);
    }
    let table = match lang {
        Lang::Ru => &RU,
        Lang::En => &EN,
    };
    for (phrases, action) in table.fixed {
        if phrases.contains(&phrase.as_str()) {
            return Some(*action);
        }
    }
    let words: Vec<&str> = w.iter().map(String::as_str).collect();
    if let Some(level) =
        after_any(&words, table.volume_set).and_then(|rest| percent(rest, lang, table.percent))
    {
        return Some(QuickAction::VolumeSet(level));
    }
    if let Some(seconds) = after_any(&words, table.timer).and_then(|rest| duration(rest, lang)) {
        return Some(QuickAction::Timer { seconds });
    }
    if let Some(rest) = after_any(&words, table.open) {
        let target = rest.join(" ");
        for (names, app) in table.apps {
            if names.contains(&target.as_str()) {
                return Some(QuickAction::OpenApp(*app));
            }
        }
        for (names, site) in table.sites {
            if names.contains(&target.as_str()) {
                return Some(QuickAction::OpenSite(*site));
            }
        }
    }
    None
}

/// The phrases of one language.
struct Table {
    fixed: &'static [(&'static [&'static str], QuickAction)],
    /// Words before the level: «громкость на» 30.
    volume_set: &'static [&'static str],
    /// Words that may follow the level.
    percent: &'static [&'static str],
    /// Words before the time of a timer: «таймер на» 5 минут.
    timer: &'static [&'static str],
    /// Words before what to open.
    open: &'static [&'static str],
    apps: &'static [(&'static [&'static str], App)],
    sites: &'static [(&'static [&'static str], Site)],
}

/// The words after one of `prefixes` (each a few words), the longest that fits.
fn after_any<'a>(words: &'a [&'a str], prefixes: &[&str]) -> Option<&'a [&'a str]> {
    prefixes
        .iter()
        .filter_map(|p| {
            let p: Vec<&str> = p.split(' ').collect();
            (words.len() > p.len() && words[..p.len()] == p[..]).then(|| &words[p.len()..])
        })
        .min_by_key(|rest| rest.len())
}

/// A level 0–100, maybe followed by a word of percent, and nothing else.
fn percent(rest: &[&str], lang: Lang, percent: &[&str]) -> Option<u8> {
    let (n, used) = number(rest, lang)?;
    let tail = &rest[used..];
    let fits = tail.is_empty() || (tail.len() == 1 && percent.contains(&tail[0]));
    (fits && n <= 100).then_some(n as u8)
}

const RU_UNITS: &[(&str, u32)] = &[
    ("ноль", 0),
    ("один", 1),
    ("одна", 1),
    ("одну", 1),
    ("два", 2),
    ("две", 2),
    ("три", 3),
    ("четыре", 4),
    ("пять", 5),
    ("шесть", 6),
    ("семь", 7),
    ("восемь", 8),
    ("девять", 9),
    ("десять", 10),
    ("одиннадцать", 11),
    ("двенадцать", 12),
    ("тринадцать", 13),
    ("четырнадцать", 14),
    ("пятнадцать", 15),
    ("шестнадцать", 16),
    ("семнадцать", 17),
    ("восемнадцать", 18),
    ("девятнадцать", 19),
];
const RU_TENS: &[(&str, u32)] = &[
    ("двадцать", 20),
    ("тридцать", 30),
    ("сорок", 40),
    ("пятьдесят", 50),
    ("шестьдесят", 60),
    ("семьдесят", 70),
    ("восемьдесят", 80),
    ("девяносто", 90),
];
const EN_UNITS: &[(&str, u32)] = &[
    ("zero", 0),
    ("one", 1),
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("eleven", 11),
    ("twelve", 12),
    ("thirteen", 13),
    ("fourteen", 14),
    ("fifteen", 15),
    ("sixteen", 16),
    ("seventeen", 17),
    ("eighteen", 18),
    ("nineteen", 19),
];
const EN_TENS: &[(&str, u32)] = &[
    ("twenty", 20),
    ("thirty", 30),
    ("forty", 40),
    ("fifty", 50),
    ("sixty", 60),
    ("seventy", 70),
    ("eighty", 80),
    ("ninety", 90),
];

/// A number at the start of `words`: digits, or words up to 999 («сто
/// двадцать пять», «twenty five»). The value and how many words it took.
fn number(words: &[&str], lang: Lang) -> Option<(u32, usize)> {
    let first = words.first()?;
    if first.chars().all(|c| c.is_ascii_digit()) {
        return first.parse().ok().map(|n| (n, 1));
    }
    let (units, tens, hundred) = match lang {
        Lang::Ru => (RU_UNITS, RU_TENS, "сто"),
        Lang::En => (EN_UNITS, EN_TENS, "hundred"),
    };
    let find = |list: &[(&str, u32)], w: Option<&&str>| {
        w.and_then(|w| list.iter().find(|(name, _)| name == w).map(|(_, v)| *v))
    };
    let mut at = 0;
    let mut value = 0;
    // «сто», «one hundred», «a hundred».
    match lang {
        Lang::Ru if words.first() == Some(&hundred) => {
            value = 100;
            at = 1;
        }
        Lang::En
            if words.get(1) == Some(&hundred)
                && (find(units, words.first()) == Some(1) || words.first() == Some(&"a")) =>
        {
            value = 100;
            at = 2;
        }
        _ => {}
    }
    if let Some(t) = find(tens, words.get(at)) {
        value += t;
        at += 1;
        if let Some(u) = find(units, words.get(at)).filter(|u| (1..10).contains(u)) {
            value += u;
            at += 1;
        }
    } else if let Some(u) = find(units, words.get(at)) {
        value += u;
        at += 1;
    }
    (at > 0).then_some((value, at))
}

/// A duration filling all of `words`: «5 минут», «полчаса», «1 час 30
/// минут», «a minute», «two minutes and 30 seconds». Seconds, 1 to
/// `MAX_TIMER_SECONDS`.
fn duration(words: &[&str], lang: Lang) -> Option<u32> {
    let unit = |w: &str| -> Option<u32> {
        let ru = [
            (&["час", "часа", "часов", "ч"][..], 3600),
            (
                &["минута", "минуту", "минуты", "минут", "мин", "минутку"][..],
                60,
            ),
            (
                &["секунда", "секунду", "секунды", "секунд", "сек", "с"][..],
                1,
            ),
        ];
        let en = [
            (&["hour", "hours", "hr", "hrs", "h"][..], 3600),
            (&["minute", "minutes", "min", "mins", "m"][..], 60),
            (&["second", "seconds", "sec", "secs", "s"][..], 1),
        ];
        let list = match lang {
            Lang::Ru => ru,
            Lang::En => en,
        };
        list.iter()
            .find(|(names, _)| names.contains(&w))
            .map(|(_, v)| *v)
    };
    let mut total: u32 = 0;
    let mut at = 0;
    let mut groups = 0;
    while at < words.len() {
        let w = words[at];
        if (w == "и" || w == "and") && groups > 0 {
            at += 1;
            continue;
        }
        // Halves: «полчаса», «полминуты», «half an hour».
        let half = match (lang, w) {
            (Lang::Ru, "полчаса") => Some(1800),
            (Lang::Ru, "полминуты") => Some(30),
            (Lang::En, "half") => match words.get(at + 1..at + 3) {
                Some(["an", "hour"]) => Some(1800),
                Some(["a", "minute"]) => Some(30),
                _ => None,
            },
            _ => None,
        };
        if let Some(h) = half {
            total = total.checked_add(h)?;
            at += if lang == Lang::En { 3 } else { 1 };
            groups += 1;
            continue;
        }
        // «полтора часа»: one and a half.
        if lang == Lang::Ru && w == "полтора" {
            let u = unit(words.get(at + 1)?)?;
            total = total.checked_add(u + u / 2)?;
            at += 2;
            groups += 1;
            continue;
        }
        let (n, used) = match number(&words[at..], lang) {
            Some(found) => found,
            // «a minute», «an hour», «минуту», «час»: one.
            None if lang == Lang::En && (w == "a" || w == "an") => (1, 1),
            None => (1, 0),
        };
        let u = unit(words.get(at + used)?)?;
        total = total.checked_add(n.checked_mul(u)?)?;
        at += used + 1;
        groups += 1;
    }
    (groups > 0 && (1..=MAX_TIMER_SECONDS).contains(&total)).then_some(total)
}

/// English sleep phrases; the Russian ones are `pause::SLEEP_WORDS`.
const EN_SLEEP: &[&str] = &[
    "sleep",
    "go to sleep",
    "go sleep",
    "time to sleep",
    "take a nap",
    "take a rest",
];

static RU: Table = Table {
    fixed: &[
        (
            &[
                "громче",
                "погромче",
                "еще громче",
                "сделай громче",
                "сделай погромче",
                "сделай звук громче",
                "сделай звук погромче",
                "звук громче",
                "прибавь звук",
                "прибавь громкость",
                "добавь звук",
                "добавь громкость",
            ],
            QuickAction::VolumeUp,
        ),
        (
            &[
                "тише",
                "потише",
                "еще тише",
                "сделай тише",
                "сделай потише",
                "сделай звук тише",
                "сделай звук потише",
                "звук тише",
                "убавь звук",
                "убавь громкость",
            ],
            QuickAction::VolumeDown,
        ),
        (
            &["выключи звук", "отключи звук", "убери звук", "без звука"],
            QuickAction::Mute,
        ),
        (&["включи звук", "верни звук"], QuickAction::Unmute),
        (
            &["пауза", "на паузу", "поставь на паузу", "поставь паузу"],
            QuickAction::MediaPause,
        ),
        (
            // A bare «продолжи» goes to the model: after a story it means
            // «go on telling», not the play key.
            &[
                "продолжи музыку",
                "продолжи воспроизведение",
                "сними с паузы",
                "играй дальше",
            ],
            QuickAction::MediaResume,
        ),
        (
            &[
                "следующий трек",
                "следующая песня",
                "следующую песню",
                "следующий трек включи",
                "включи следующий трек",
                "включи следующую песню",
                "переключи трек",
            ],
            QuickAction::MediaNext,
        ),
        (
            &[
                "предыдущий трек",
                "предыдущая песня",
                "предыдущую песню",
                "прошлый трек",
                "прошлая песня",
                "включи предыдущий трек",
                "включи предыдущую песню",
            ],
            QuickAction::MediaPrevious,
        ),
        (
            &[
                "который час",
                "который сейчас час",
                "сколько времени",
                "сколько сейчас времени",
                "сколько время",
                "сколько щас времени",
            ],
            QuickAction::Time,
        ),
        (
            &[
                "какое число",
                "какое сегодня число",
                "какое число сегодня",
                "какая дата",
                "какая сегодня дата",
                "какой сегодня день",
                "какой день недели",
                "какой сегодня день недели",
            ],
            QuickAction::Date,
        ),
        (
            &[
                "посмотри как я играю",
                "смотри как я играю",
                "посмотри как я играю в игру",
                "комментируй игру",
                "комментируй мою игру",
                "включи напарника",
                "будь моим напарником",
            ],
            QuickAction::CompanionOn,
        ),
        (
            &[
                "хватит комментировать",
                "не комментируй",
                "не комментируй игру",
                "перестань комментировать",
                "хватит комментировать игру",
                "выключи напарника",
            ],
            QuickAction::CompanionOff,
        ),
    ],
    volume_set: &[
        "звук на",
        "звук",
        "громкость на",
        "громкость",
        "поставь громкость на",
        "поставь громкость",
        "поставь звук на",
        "сделай громкость на",
        "сделай громкость",
        "сделай звук на",
        "установи громкость на",
        "установи громкость",
    ],
    percent: &["процентов", "процента", "процент"],
    timer: &[
        "таймер на",
        "поставь таймер на",
        "установи таймер на",
        "заведи таймер на",
        "засеки",
        "засеки на",
    ],
    open: &["открой", "запусти"],
    apps: &[
        (&["браузер"], App::Browser),
        (&["проводник"], App::Explorer),
        (&["блокнот"], App::Notepad),
        (&["калькулятор"], App::Calculator),
        (&["диспетчер задач"], App::TaskManager),
    ],
    sites: &[
        (&["ютуб", "ютюб", "youtube"], Site::YouTube),
        (&["почту", "почта"], Site::Mail),
        (&["карты", "карту"], Site::Maps),
    ],
};

static EN: Table = Table {
    fixed: &[
        (
            &[
                "louder",
                "volume up",
                "turn it up",
                "turn up the volume",
                "turn the volume up",
                "sound up",
                "make it louder",
            ],
            QuickAction::VolumeUp,
        ),
        (
            &[
                "quieter",
                "softer",
                "volume down",
                "turn it down",
                "turn down the volume",
                "turn the volume down",
                "sound down",
                "make it quieter",
            ],
            QuickAction::VolumeDown,
        ),
        (
            &["mute", "mute the sound", "mute sound", "sound off"],
            QuickAction::Mute,
        ),
        (
            &["unmute", "unmute the sound", "sound on"],
            QuickAction::Unmute,
        ),
        (
            &["pause", "pause the music", "pause music"],
            QuickAction::MediaPause,
        ),
        (
            &["resume", "resume the music", "resume music", "unpause"],
            QuickAction::MediaResume,
        ),
        (
            &[
                "next track",
                "next song",
                "skip track",
                "skip song",
                "skip this song",
            ],
            QuickAction::MediaNext,
        ),
        (
            &["previous track", "previous song", "last track", "last song"],
            QuickAction::MediaPrevious,
        ),
        (
            &[
                "what time is it",
                "whats the time",
                "what is the time",
                "tell me the time",
            ],
            QuickAction::Time,
        ),
        (
            &[
                "whats the date",
                "what is the date",
                "what date is it",
                "whats the date today",
                "what is todays date",
                "whats todays date",
                "what day is it",
                "what day is it today",
                "what day is today",
            ],
            QuickAction::Date,
        ),
        (
            &[
                "watch me play",
                "watch me playing",
                "watch my game",
                "comment on my game",
                "turn on the companion",
                "be my gaming buddy",
            ],
            QuickAction::CompanionOn,
        ),
        (
            &[
                "stop commenting",
                "stop commenting on my game",
                "dont comment",
                "no comments",
                "turn off the companion",
            ],
            QuickAction::CompanionOff,
        ),
    ],
    volume_set: &[
        "volume",
        "volume to",
        "volume at",
        "set volume to",
        "set the volume to",
        "set volume",
        "sound to",
    ],
    percent: &["percent"],
    timer: &[
        "timer for",
        "a timer for",
        "set a timer for",
        "set timer for",
        "start a timer for",
    ],
    open: &["open", "launch", "start"],
    apps: &[
        (&["browser", "the browser"], App::Browser),
        (
            &["explorer", "file explorer", "the file explorer"],
            App::Explorer,
        ),
        (&["notepad"], App::Notepad),
        (&["calculator", "the calculator"], App::Calculator),
        (&["task manager", "the task manager"], App::TaskManager),
    ],
    sites: &[
        (&["youtube"], Site::YouTube),
        (&["mail", "email", "my mail"], Site::Mail),
        (&["maps", "the map"], Site::Maps),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use QuickAction::*;

    const NAMES: &[&str] = &["Мика", "Mika"];

    fn ru(text: &str) -> Option<QuickAction> {
        match_quick(text, Lang::Ru, NAMES)
    }

    fn en(text: &str) -> Option<QuickAction> {
        match_quick(text, Lang::En, NAMES)
    }

    #[test]
    fn russian_phrases_map_to_their_action() {
        let table: &[(&str, Option<QuickAction>)] = &[
            ("громче", Some(VolumeUp)),
            ("Погромче!", Some(VolumeUp)),
            ("сделай погромче", Some(VolumeUp)),
            ("Сделай звук громче.", Some(VolumeUp)),
            ("прибавь звук", Some(VolumeUp)),
            ("ещё громче", Some(VolumeUp)),
            ("тише", Some(VolumeDown)),
            ("Мика, тише", Some(VolumeDown)),
            ("эй Мика потише", Some(VolumeDown)),
            ("потише, пожалуйста", Some(VolumeDown)),
            ("сделай потише", Some(VolumeDown)),
            ("убавь громкость", Some(VolumeDown)),
            ("звук на 30", Some(VolumeSet(30))),
            ("громкость 45", Some(VolumeSet(45))),
            ("Громкость на 100 процентов", Some(VolumeSet(100))),
            ("звук на 0", Some(VolumeSet(0))),
            ("поставь громкость на двадцать пять", Some(VolumeSet(25))),
            ("громкость сорок процентов", Some(VolumeSet(40))),
            ("звук на сто", Some(VolumeSet(100))),
            ("сделай громкость 70%", Some(VolumeSet(70))),
            ("установи громкость на 15", Some(VolumeSet(15))),
            ("выключи звук", Some(Mute)),
            ("Отключи звук!", Some(Mute)),
            ("без звука", Some(Mute)),
            ("включи звук", Some(Unmute)),
            ("верни звук", Some(Unmute)),
            ("пауза", Some(MediaPause)),
            ("Пауза.", Some(MediaPause)),
            ("поставь на паузу", Some(MediaPause)),
            ("продолжи", None),
            ("продолжай", None),
            ("продолжи музыку", Some(MediaResume)),
            ("сними с паузы", Some(MediaResume)),
            ("следующий трек", Some(MediaNext)),
            ("Следующая песня", Some(MediaNext)),
            ("переключи трек", Some(MediaNext)),
            ("предыдущий трек", Some(MediaPrevious)),
            ("прошлая песня", Some(MediaPrevious)),
            ("который час", Some(Time)),
            ("Который час?", Some(Time)),
            ("Мика, сколько времени?", Some(Time)),
            ("сколько сейчас времени", Some(Time)),
            ("какое сегодня число", Some(Date)),
            ("Какое число?", Some(Date)),
            ("какой сегодня день недели", Some(Date)),
            ("какая сегодня дата", Some(Date)),
            ("таймер на 5 минут", Some(Timer { seconds: 300 })),
            ("Таймер на пять минут", Some(Timer { seconds: 300 })),
            ("засеки 30 секунд", Some(Timer { seconds: 30 })),
            ("засеки минуту", Some(Timer { seconds: 60 })),
            ("поставь таймер на полчаса", Some(Timer { seconds: 1800 })),
            ("таймер на час", Some(Timer { seconds: 3600 })),
            ("таймер на полтора часа", Some(Timer { seconds: 5400 })),
            ("таймер на 1 час 30 минут", Some(Timer { seconds: 5400 })),
            ("засеки 2 минуты и 30 секунд", Some(Timer { seconds: 150 })),
            ("засеки на двадцать минут", Some(Timer { seconds: 1200 })),
            ("засеки полминуты", Some(Timer { seconds: 30 })),
            ("открой браузер", Some(OpenApp(App::Browser))),
            ("Открой проводник", Some(OpenApp(App::Explorer))),
            ("открой блокнот", Some(OpenApp(App::Notepad))),
            ("запусти калькулятор", Some(OpenApp(App::Calculator))),
            ("открой диспетчер задач", Some(OpenApp(App::TaskManager))),
            ("открой ютуб", Some(OpenSite(Site::YouTube))),
            ("Открой YouTube", Some(OpenSite(Site::YouTube))),
            ("открой почту", Some(OpenSite(Site::Mail))),
            ("открой карты", Some(OpenSite(Site::Maps))),
            ("усни", Some(Sleep)),
            ("Спи", Some(Sleep)),
            ("засыпай", Some(Sleep)),
            ("отдохни", Some(Sleep)),
            ("Мика, иди спать", Some(Sleep)),
            ("усни, пожалуйста", Some(Sleep)),
            ("Мика, посмотри, как я играю", Some(CompanionOn)),
            ("смотри как я играю", Some(CompanionOn)),
            ("комментируй игру", Some(CompanionOn)),
            ("Хватит комментировать!", Some(CompanionOff)),
            ("не комментируй", Some(CompanionOff)),
            ("перестань комментировать", Some(CompanionOff)),
        ];
        for (text, want) in table {
            assert_eq!(ru(text), *want, "{text}");
        }
    }

    #[test]
    fn a_longer_or_other_phrase_goes_to_the_model() {
        for text in [
            "",
            "Мика",
            "пожалуйста",
            "сколько стоит пауза в игре",
            "тише едешь — дальше будешь",
            "сделай потише музыку в браузере",
            "сделай громче телеграм",
            "громче чем вчера",
            "звук на 300",
            "громкость",
            "громкость на максимум",
            "открой браузер и найди котиков",
            "открой файл отчёт",
            "открой",
            "который час в Токио",
            "таймер",
            "таймер на 5",
            "таймер на 5 яблок",
            "таймер на 30 часов",
            "засеки 0 секунд",
            "пауза между словами",
            "продолжи рассказ",
            "спи спокойно",
            "список дел",
            "хочу спать",
            "следующий",
            "включи музыку",
            "стоп",
            "тихо",
            "Михаил, тише",
            "what time is it",
            "посмотри как я играю в шахматы",
            "хватит комментировать мои ошибки",
        ] {
            assert_eq!(ru(text), None, "{text}");
        }
    }

    #[test]
    fn english_phrases_map_to_their_action() {
        let table: &[(&str, Option<QuickAction>)] = &[
            ("louder", Some(VolumeUp)),
            ("Mika, volume up", Some(VolumeUp)),
            ("turn it up please", Some(VolumeUp)),
            ("quieter", Some(VolumeDown)),
            ("hey Mika, turn it down", Some(VolumeDown)),
            ("volume 30", Some(VolumeSet(30))),
            ("set the volume to 80 percent", Some(VolumeSet(80))),
            ("volume to twenty five", Some(VolumeSet(25))),
            ("volume at one hundred", Some(VolumeSet(100))),
            ("mute", Some(Mute)),
            ("Sound off.", Some(Mute)),
            ("unmute", Some(Unmute)),
            ("pause", Some(MediaPause)),
            ("resume", Some(MediaResume)),
            ("next track", Some(MediaNext)),
            ("skip this song", Some(MediaNext)),
            ("previous song", Some(MediaPrevious)),
            ("What time is it?", Some(Time)),
            ("what's the time", Some(Time)),
            ("What’s the date today?", Some(Date)),
            ("what day is it", Some(Date)),
            ("set a timer for 5 minutes", Some(Timer { seconds: 300 })),
            ("timer for thirty seconds", Some(Timer { seconds: 30 })),
            ("set a timer for a minute", Some(Timer { seconds: 60 })),
            ("timer for half an hour", Some(Timer { seconds: 1800 })),
            (
                "set a timer for 1 hour and 15 minutes",
                Some(Timer { seconds: 4500 }),
            ),
            ("open the browser", Some(OpenApp(App::Browser))),
            ("open file explorer", Some(OpenApp(App::Explorer))),
            ("launch notepad", Some(OpenApp(App::Notepad))),
            ("open calculator", Some(OpenApp(App::Calculator))),
            ("start task manager", Some(OpenApp(App::TaskManager))),
            ("open youtube", Some(OpenSite(Site::YouTube))),
            ("open my mail", Some(OpenSite(Site::Mail))),
            ("open maps", Some(OpenSite(Site::Maps))),
            ("go to sleep", Some(Sleep)),
            ("sleep", Some(Sleep)),
            // The sleep words are heard in both languages.
            ("усни", Some(Sleep)),
            ("Mika, watch me play", Some(CompanionOn)),
            ("stop commenting", Some(CompanionOff)),
            ("don't comment", Some(CompanionOff)),
        ];
        for (text, want) in table {
            assert_eq!(en(text), *want, "{text}");
        }
        for text in [
            "how much does the pause cost in the game",
            "make the music in the browser quieter",
            "volume 200",
            "what time is it in Tokyo",
            "open the report file",
            "set a timer",
            "sleep well",
            "который час",
        ] {
            assert_eq!(en(text), None, "{text}");
        }
    }

    #[test]
    fn numbers_in_words_and_digits() {
        let n = |s: &str, lang| {
            let w: Vec<&str> = s.split(' ').collect();
            number(&w, lang)
        };
        assert_eq!(n("42", Lang::Ru), Some((42, 1)));
        assert_eq!(n("сто двадцать пять", Lang::Ru), Some((125, 3)));
        assert_eq!(n("двадцать минут", Lang::Ru), Some((20, 1)));
        assert_eq!(n("одиннадцать", Lang::Ru), Some((11, 1)));
        assert_eq!(n("минут", Lang::Ru), None);
        assert_eq!(n("a hundred", Lang::En), Some((100, 2)));
        assert_eq!(n("ninety nine", Lang::En), Some((99, 2)));
        assert_eq!(n("seventeen", Lang::En), Some((17, 1)));
    }

    #[test]
    fn each_action_calls_its_tool_with_the_arguments_of_its_schema() {
        let call = |a: QuickAction| a.call(Lang::Ru).unwrap();
        assert_eq!(call(VolumeUp), ("volume", json!({"action": "up"})));
        assert_eq!(
            call(VolumeSet(30)),
            ("volume", json!({"action": "set", "level": 30}))
        );
        assert_eq!(call(Mute), ("volume", json!({"action": "mute"})));
        assert_eq!(
            call(MediaResume),
            ("media_control", json!({"action": "play_pause"}))
        );
        assert_eq!(
            call(MediaPrevious),
            ("media_control", json!({"action": "previous"}))
        );
        assert_eq!(call(Date), ("get_time", json!({})));
        assert_eq!(
            call(Timer { seconds: 300 }),
            ("reminder_set", json!({"text": "Таймер", "in_minutes": 5}))
        );
        assert_eq!(
            call(Timer { seconds: 90 }),
            ("reminder_set", json!({"text": "Таймер", "in_seconds": 90}))
        );
        assert_eq!(
            call(OpenApp(App::Notepad)),
            ("open_app", json!({"name": "notepad.exe"}))
        );
        assert_eq!(
            call(OpenApp(App::Browser)),
            ("open_url", json!({"url": "https://ya.ru"}))
        );
        assert_eq!(
            OpenSite(Site::Mail).call(Lang::En).unwrap(),
            ("open_url", json!({"url": "https://mail.google.com"}))
        );
        assert_eq!(Sleep.call(Lang::Ru), None);
        assert_eq!(
            call(CompanionOn),
            (
                "settings_set",
                json!({"key": "companion.enabled", "value": "on"})
            )
        );
        assert_eq!(
            call(CompanionOff),
            (
                "settings_set",
                json!({"key": "companion.enabled", "value": "off"})
            )
        );
    }

    #[test]
    fn the_reply_is_short_and_from_the_locale_files() {
        let data = json!({"kind": "volume", "level": 40, "was": 30});
        assert_eq!(
            VolumeUp.reply(Lang::Ru, true, "Громкость 40% (была 30%).", Some(&data)),
            "Громкость 40 %."
        );
        assert_eq!(
            VolumeSet(40).reply(Lang::En, true, "", Some(&data)),
            "Volume 40%."
        );
        assert_eq!(Mute.reply(Lang::Ru, true, "", None), "Звук выключен.");
        assert_eq!(MediaPause.reply(Lang::Ru, true, "", None), "Пауза.");
        let now = "2026-11-22 14:05:09, воскресенье, UTC+03:00";
        assert_eq!(Time.reply(Lang::Ru, true, now, None), "Сейчас 14:05.");
        assert_eq!(
            Date.reply(Lang::Ru, true, now, None),
            "Сегодня 22 ноября, воскресенье."
        );
        assert_eq!(
            Date.reply(Lang::En, true, now, None),
            "Today is Sunday, November 22."
        );
        assert_eq!(
            Timer { seconds: 5400 }.reply(Lang::Ru, true, "", None),
            "Таймер на 1 ч 30 мин."
        );
        assert_eq!(
            Timer { seconds: 30 }.reply(Lang::En, true, "", None),
            "Timer for 30 s."
        );
        assert_eq!(
            OpenApp(App::Notepad).reply(Lang::Ru, true, "", None),
            "Открываю."
        );
        assert_eq!(
            Sleep.reply(Lang::Ru, true, "", None),
            "Сплю. Позови по имени."
        );
        assert_eq!(
            CompanionOff.reply(Lang::Ru, true, "Готово: …", None),
            "Молчу, не мешаю."
        );
        assert_eq!(
            CompanionOn.reply(Lang::En, true, "", None),
            "Watching. Play, I'm right here."
        );
        // A refusal is told in the words of the tool: «Экран» is off, or
        // it came from Telegram.
        assert_eq!(
            CompanionOn.reply(
                Lang::Ru,
                false,
                "сначала нужно включить просмотр экрана",
                None
            ),
            "сначала нужно включить просмотр экрана"
        );
        assert_eq!(
            VolumeUp.reply(Lang::Ru, false, "Действие не выполнено: отказ. ", None),
            "Действие не выполнено: отказ."
        );
    }
}
