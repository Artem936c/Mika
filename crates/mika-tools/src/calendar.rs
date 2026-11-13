//! Meetings from a calendar published as an ICS link (Google, Outlook, Яндекс).
//! The parser knows what such links really carry: single and repeating events,
//! cancelled and moved occurrences, time zones described by `VTIMEZONE`.
//! Numbers in a link are not trusted: date arithmetic is checked and stops the
//! expansion instead of overflowing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{
    DateTime, Datelike, Duration, Local, Months, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday,
};
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use parking_lot::RwLock;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::util::schema;

/// Periods (days, weeks, months, years) one repeating event may walk through
/// while expanding, a guard against broken rules.
const MAX_STEPS: i64 = 5000;
/// Occurrences one repeating event may produce while expanding.
const MAX_OCCURRENCES: usize = 20_000;

/// One `STANDARD` or `DAYLIGHT` block of a `VTIMEZONE`: from each of its
/// onsets on, the zone is `offset` seconds east of UTC.
#[derive(Debug, Clone, PartialEq)]
struct Observance {
    offset: i32,
    /// The offset before the first onset (`TZOFFSETFROM`).
    before: i32,
    /// The first onset, wall time.
    start: NaiveDateTime,
    /// Yearly onsets after the first one.
    rule: Option<Recur>,
    /// Single onsets (`RDATE`).
    dates: Vec<NaiveDateTime>,
}

impl Observance {
    /// The latest onset at or before `wall`.
    fn last_onset(&self, wall: NaiveDateTime) -> Option<NaiveDateTime> {
        let mut best = Some(self.start).filter(|t| *t <= wall);
        for d in self.dates.iter().filter(|d| **d <= wall) {
            best = best.max(Some(*d));
        }
        let Some(rule) = self.rule.as_ref().filter(|r| r.freq == Some(Freq::Yearly)) else {
            return best;
        };
        // A rule of the past stops at its UNTIL: the change it describes was
        // cancelled since.
        let until = rule.until.and_then(|(u, day, utc)| {
            if day {
                u.date().and_hms_opt(23, 59, 59)
            } else if utc {
                u.checked_add_signed(Duration::seconds(self.before.into()))
            } else {
                Some(u)
            }
        });
        let hi = until.map_or(wall, |u| u.min(wall));
        for year in [hi.year().saturating_sub(1), hi.year()] {
            for d in rule.year_days(year, self.start.date()) {
                let t = d.and_time(self.start.time());
                if t >= self.start && t <= hi {
                    best = best.max(Some(t));
                }
            }
        }
        best
    }
}

/// A zone described by `VTIMEZONE`: all its blocks, the history included.
#[derive(Debug, Clone, PartialEq)]
struct Rules {
    parts: Vec<Observance>,
}

impl Rules {
    /// Seconds east of UTC at a wall time: the block with the latest onset
    /// before it wins.
    fn offset_at(&self, wall: NaiveDateTime) -> i32 {
        self.parts
            .iter()
            .filter_map(|p| Some((p.last_onset(wall)?, p.offset)))
            .max_by_key(|(t, _)| *t)
            .map(|(_, offset)| offset)
            // Before the first onset: what the earliest block changes from.
            .unwrap_or_else(|| {
                self.parts
                    .iter()
                    .min_by_key(|p| p.start)
                    .map_or(0, |p| p.before)
            })
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Zone {
    Utc,
    /// Floating time and unknown zones: the computer's own time.
    Local,
    /// A zone named without description, seconds east of UTC.
    Fixed(i32),
    Rules(Arc<Rules>),
}

impl Zone {
    /// Seconds east of UTC at a wall time; `None` for the computer's zone.
    fn offset_at(&self, wall: NaiveDateTime) -> Option<i32> {
        match self {
            Zone::Local => None,
            Zone::Utc => Some(0),
            Zone::Fixed(offset) => Some(*offset),
            Zone::Rules(r) => Some(r.offset_at(wall)),
        }
    }

    /// The UTC moment of a wall time in this zone.
    fn to_utc(&self, wall: NaiveDateTime) -> Option<NaiveDateTime> {
        match self.offset_at(wall) {
            Some(offset) => wall.checked_sub_signed(Duration::seconds(offset.into())),
            None => Local
                .from_local_datetime(&wall)
                .earliest()
                .map(|t| t.naive_utc()),
        }
    }

    /// The computer's local time for a wall time in this zone.
    fn to_local(&self, wall: NaiveDateTime) -> NaiveDateTime {
        if *self == Zone::Local {
            return wall;
        }
        self.to_utc(wall).map_or(wall, |utc| {
            Utc.from_utc_datetime(&utc)
                .with_timezone(&Local)
                .naive_local()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// A repeat rule (`RRULE`) as far as calendar links use it.
#[derive(Debug, Clone, PartialEq)]
struct Recur {
    /// `None` for frequencies the parser does not expand.
    freq: Option<Freq>,
    interval: i64,
    count: Option<usize>,
    /// The last start: (wall time, written as a date, written in UTC).
    until: Option<(NaiveDateTime, bool, bool)>,
    by_day: Vec<(i32, Weekday)>,
    by_month: Vec<u32>,
    /// Negative days count from the end of the month.
    by_month_day: Vec<i32>,
    by_set_pos: Vec<i32>,
    week_start: Weekday,
}

/// The comma-separated values of one rule part.
fn values<'a>(rule: &'a HashMap<String, String>, key: &str) -> impl Iterator<Item = &'a str> {
    rule.get(key)
        .into_iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
}

impl Recur {
    fn parse(text: &str) -> Recur {
        let r = rules_of(text);
        let freq = match r.get("FREQ").map(|f| f.to_ascii_uppercase()).as_deref() {
            Some("DAILY") => Some(Freq::Daily),
            Some("WEEKLY") => Some(Freq::Weekly),
            Some("MONTHLY") => Some(Freq::Monthly),
            Some("YEARLY") => Some(Freq::Yearly),
            _ => None,
        };
        Recur {
            freq,
            interval: r
                .get("INTERVAL")
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(1),
            count: r.get("COUNT").and_then(|v| v.parse().ok()),
            until: r.get("UNTIL").and_then(|v| parse_when(v)),
            by_day: values(&r, "BYDAY").filter_map(by_day).collect(),
            by_month: values(&r, "BYMONTH")
                .filter_map(|v| v.parse().ok())
                .filter(|m| (1..=12).contains(m))
                .collect(),
            by_month_day: values(&r, "BYMONTHDAY")
                .filter_map(|v| v.parse::<i32>().ok())
                .filter(|d| *d != 0 && d.unsigned_abs() <= 31)
                .collect(),
            by_set_pos: values(&r, "BYSETPOS")
                .filter_map(|v| v.parse::<i32>().ok())
                .filter(|p| *p != 0 && p.unsigned_abs() <= 366)
                .collect(),
            week_start: r
                .get("WKST")
                .and_then(|v| weekday_of(&v.trim().to_ascii_uppercase()))
                .unwrap_or(Weekday::Mon),
        }
    }

    fn weekday_ok(&self, d: NaiveDate) -> bool {
        self.by_day.is_empty() || self.by_day.iter().any(|(_, w)| *w == d.weekday())
    }

    fn month_ok(&self, d: NaiveDate) -> bool {
        self.by_month.is_empty() || self.by_month.contains(&d.month())
    }

    fn month_day_ok(&self, d: NaiveDate) -> bool {
        let first = d.with_day(1);
        self.by_month_day.is_empty()
            || self
                .by_month_day
                .iter()
                .any(|md| first.and_then(|f| day_of_month(f, *md)) == Some(d))
    }

    /// Days of a month the rule picks: by month day, by weekday, or the day
    /// of the first start.
    fn month_days(&self, year: i32, month: u32, start: NaiveDate) -> Vec<NaiveDate> {
        let Some(first) = NaiveDate::from_ymd_opt(year, month, 1) else {
            return Vec::new();
        };
        if !self.by_month_day.is_empty() {
            // Both given: the weekdays only narrow the month days.
            return self
                .by_month_day
                .iter()
                .filter_map(|md| day_of_month(first, *md))
                .filter(|d| self.weekday_ok(*d))
                .collect();
        }
        if !self.by_day.is_empty() {
            let Some(last) = month_end(first) else {
                return Vec::new();
            };
            return self
                .by_day
                .iter()
                .flat_map(|(nth, w)| weekdays_in(first, last, *w, *nth))
                .collect();
        }
        // The 31st in a short month: no meeting that month.
        first.with_day(start.day()).into_iter().collect()
    }

    /// Days of a year the rule picks.
    fn year_days(&self, year: i32, start: NaiveDate) -> Vec<NaiveDate> {
        if !self.by_month.is_empty() {
            return self
                .by_month
                .iter()
                .flat_map(|m| self.month_days(year, *m, start))
                .collect();
        }
        if !self.by_month_day.is_empty() {
            return (1..=12)
                .flat_map(|m| self.month_days(year, m, start))
                .collect();
        }
        if !self.by_day.is_empty() {
            if let (Some(first), Some(last)) = (
                NaiveDate::from_ymd_opt(year, 1, 1),
                NaiveDate::from_ymd_opt(year, 12, 31),
            ) {
                return self
                    .by_day
                    .iter()
                    .flat_map(|(nth, w)| weekdays_in(first, last, *w, *nth))
                    .collect();
            }
        }
        // The 29th of February: only in leap years.
        NaiveDate::from_ymd_opt(year, start.month(), start.day())
            .into_iter()
            .collect()
    }

    /// The first day of the period `n` periods after the first start, and the
    /// days the rule picks in it, sorted. `None` when dates run out of range.
    fn period(&self, freq: Freq, start: NaiveDate, n: i64) -> Option<(NaiveDate, Vec<NaiveDate>)> {
        let (first, mut days) = match freq {
            Freq::Daily => {
                let d = start.checked_add_signed(Duration::try_days(n)?)?;
                let keep = self.month_ok(d) && self.month_day_ok(d) && self.weekday_ok(d);
                (d, if keep { vec![d] } else { Vec::new() })
            }
            Freq::Weekly => {
                let wkst = self.week_start.num_days_from_monday();
                let back = (7 + start.weekday().num_days_from_monday() - wkst) % 7;
                let first = start
                    .checked_sub_signed(Duration::days(back.into()))?
                    .checked_add_signed(Duration::try_weeks(n)?)?;
                let wanted: Vec<Weekday> = if self.by_day.is_empty() {
                    vec![start.weekday()]
                } else {
                    self.by_day.iter().map(|(_, w)| *w).collect()
                };
                let days = wanted
                    .into_iter()
                    .filter_map(|w| {
                        let ahead = (7 + w.num_days_from_monday() - wkst) % 7;
                        first.checked_add_signed(Duration::days(ahead.into()))
                    })
                    .filter(|d| self.month_ok(*d))
                    .collect();
                (first, days)
            }
            Freq::Monthly => {
                let months = i64::from(start.month0()).checked_add(n)?;
                let year = start.year().checked_add(i32::try_from(months / 12).ok()?)?;
                let month = u32::try_from(months % 12).ok()? + 1;
                let first = NaiveDate::from_ymd_opt(year, month, 1)?;
                let days = if self.by_month.is_empty() || self.by_month.contains(&month) {
                    self.month_days(year, month, start)
                } else {
                    Vec::new()
                };
                (first, days)
            }
            Freq::Yearly => {
                let year = start.year().checked_add(i32::try_from(n).ok()?)?;
                (
                    NaiveDate::from_ymd_opt(year, 1, 1)?,
                    self.year_days(year, start),
                )
            }
        };
        days.sort();
        days.dedup();
        Some((first, set_pos(days, &self.by_set_pos)))
    }

    /// Whether a wall start `t` in `zone` comes after the rule's end.
    fn past_until(&self, t: NaiveDateTime, zone: &Zone) -> bool {
        match self.until {
            None => false,
            Some((u, true, _)) => t.date() > u.date(),
            // A moment in UTC: compared with the start in UTC too.
            Some((u, false, true)) => zone.to_utc(t).is_some_and(|t| t > u),
            Some((u, false, false)) => t > u,
        }
    }
}

/// The last day of the month that starts at `first`.
fn month_end(first: NaiveDate) -> Option<NaiveDate> {
    first.checked_add_months(Months::new(1))?.pred_opt()
}

/// Day `md` of the month that starts at `first`; negative counts from the end.
fn day_of_month(first: NaiveDate, md: i32) -> Option<NaiveDate> {
    if md > 0 {
        return first.with_day(md.unsigned_abs());
    }
    let day = i64::from(month_end(first)?.day()) + 1 + i64::from(md);
    first.with_day(u32::try_from(day).ok()?)
}

/// Weekdays `w` from `first` to `last`: every one for `nth` 0, else the
/// `nth` one (negative counts from the end).
fn weekdays_in(first: NaiveDate, last: NaiveDate, w: Weekday, nth: i32) -> Vec<NaiveDate> {
    let shift = (7 + w.num_days_from_monday() - first.weekday().num_days_from_monday()) % 7;
    let all: Vec<NaiveDate> = first
        .iter_days()
        .skip(shift as usize)
        .step_by(7)
        .take_while(|d| *d <= last)
        .collect();
    let pick = match nth {
        0 => return all,
        n if n > 0 => all.get(n.unsigned_abs() as usize - 1),
        n => all
            .len()
            .checked_sub(n.unsigned_abs() as usize)
            .and_then(|i| all.get(i)),
    };
    pick.copied().into_iter().collect()
}

/// `BYSETPOS`: the chosen places among a period's sorted days.
fn set_pos(days: Vec<NaiveDate>, pos: &[i32]) -> Vec<NaiveDate> {
    if pos.is_empty() {
        return days;
    }
    let mut out: Vec<NaiveDate> = pos
        .iter()
        .filter_map(|p| {
            let i = if *p > 0 {
                p.unsigned_abs() as usize - 1
            } else {
                days.len().checked_sub(p.unsigned_abs() as usize)?
            };
            days.get(i).copied()
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn weekday_of(code: &str) -> Option<Weekday> {
    Some(match code {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// `2TU` → (2, Tue), `-1SU` → (−1, Sun), `MO` → (0, Mon).
fn by_day(item: &str) -> Option<(i32, Weekday)> {
    let item = item.trim();
    // The split below counts bytes: only ASCII text splits on a character.
    if !item.is_ascii() {
        return None;
    }
    let split = item.len().checked_sub(2)?;
    let (n, code) = item.split_at(split);
    let nth: i32 = if n.is_empty() {
        0
    } else {
        n.trim_start_matches('+').parse().ok()?
    };
    // A year has at most 53 of each weekday.
    if nth.unsigned_abs() > 53 {
        return None;
    }
    Some((nth, weekday_of(&code.to_ascii_uppercase())?))
}

/// `+0300`, `-0530`, `+023000` → seconds.
fn utc_offset(text: &str) -> Option<i32> {
    let t = text.trim();
    let sign = match t.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let digits = &t[1..];
    if digits.len() < 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let h: i32 = digits[0..2].parse().ok()?;
    let m: i32 = digits[2..4].parse().ok()?;
    let s: i32 = digits.get(4..6).and_then(|x| x.parse().ok()).unwrap_or(0);
    Some(sign * (h * 3600 + m * 60 + s))
}

/// Zones that links name without describing them: those of Russia and its
/// neighbours, by IANA and by Windows names, with their offsets today.
fn well_known(tzid: &str) -> Option<i32> {
    let hours = match tzid {
        "Europe/Kaliningrad" | "Kaliningrad Standard Time" => 2,
        "Europe/Moscow"
        | "W-SU"
        | "Russian Standard Time"
        | "Europe/Simferopol"
        | "Europe/Kirov"
        | "Europe/Volgograd"
        | "Volgograd Standard Time"
        | "Europe/Minsk"
        | "Belarus Standard Time" => 3,
        "Europe/Samara"
        | "Russia Time Zone 3"
        | "Europe/Astrakhan"
        | "Astrakhan Standard Time"
        | "Europe/Saratov"
        | "Saratov Standard Time"
        | "Europe/Ulyanovsk" => 4,
        "Asia/Yekaterinburg" | "Ekaterinburg Standard Time" => 5,
        "Asia/Omsk" | "Omsk Standard Time" => 6,
        "Asia/Novosibirsk"
        | "N. Central Asia Standard Time"
        | "Asia/Barnaul"
        | "Altai Standard Time"
        | "Asia/Tomsk"
        | "Tomsk Standard Time"
        | "Asia/Novokuznetsk"
        | "Asia/Krasnoyarsk"
        | "North Asia Standard Time" => 7,
        "Asia/Irkutsk" | "North Asia East Standard Time" => 8,
        "Asia/Yakutsk"
        | "Yakutsk Standard Time"
        | "Asia/Chita"
        | "Transbaikal Standard Time"
        | "Asia/Khandyga" => 9,
        "Asia/Vladivostok" | "Vladivostok Standard Time" | "Asia/Ust-Nera" => 10,
        "Asia/Magadan"
        | "Magadan Standard Time"
        | "Asia/Sakhalin"
        | "Sakhalin Standard Time"
        | "Asia/Srednekolymsk"
        | "Russia Time Zone 10" => 11,
        "Asia/Kamchatka" | "Asia/Anadyr" | "Russia Time Zone 11" => 12,
        "UTC" | "Etc/UTC" | "GMT" => 0,
        _ => return None,
    };
    Some(hours * 3600)
}

/// One event as the link describes it, before repeats are expanded.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    uid: String,
    title: String,
    location: String,
    start: NaiveDateTime,
    zone: Zone,
    all_day: bool,
    length: Duration,
    rule: Option<Recur>,
    /// Local start times of cancelled occurrences.
    except: HashSet<NaiveDateTime>,
    /// Set on a moved occurrence: the local start it replaces.
    replaces: Option<NaiveDateTime>,
    /// A cancelled occurrence of a series: it only hides the one it replaces.
    cancelled: bool,
}

/// One meeting at its place in the computer's local time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Meeting {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub all_day: bool,
    pub title: String,
    pub location: String,
    pub uid: String,
}

/// A parsed calendar: events with their repeat rules.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Calendar {
    items: Vec<Item>,
}

/// Lines with folding undone: a line that starts with a space or a tab
/// continues the previous one.
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        out.push(line.to_string());
    }
    out
}

/// `DTSTART;TZID=Europe/Moscow:20261113T100000` → (`DTSTART`, params, value).
fn split_line(line: &str) -> Option<(String, HashMap<String, String>, String)> {
    let colon = {
        // The value starts after the first colon outside quotes.
        let mut quoted = false;
        line.char_indices()
            .find(|(_, c)| {
                if *c == '"' {
                    quoted = !quoted;
                }
                *c == ':' && !quoted
            })?
            .0
    };
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| {
            (
                k.trim().to_uppercase(),
                v.trim().trim_matches('"').to_string(),
            )
        })
        .collect();
    Some((name, params, value.to_string()))
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push(' '),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out.trim().to_string()
}

/// A date or date-time value: (wall time, all day, written in UTC). Years
/// outside 1–9999 are not dates of a calendar.
fn parse_when(value: &str) -> Option<(NaiveDateTime, bool, bool)> {
    let v = value.trim();
    let (t, all_day, utc) = if v.len() == 8 {
        let d = NaiveDate::parse_from_str(v, "%Y%m%d").ok()?;
        (d.and_hms_opt(0, 0, 0)?, true, false)
    } else {
        let utc = v.ends_with('Z');
        let t = NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?;
        (t, false, utc)
    };
    (1..=9999).contains(&t.year()).then_some((t, all_day, utc))
}

/// `PT1H30M`, `P1D`, `P2W` → duration; `None` when it does not fit.
fn parse_duration(text: &str) -> Option<Duration> {
    let t = text.trim();
    let (sign, t) = match t.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, t.trim_start_matches('+')),
    };
    let t = t.strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut in_time = false;
    for c in t.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                let seconds = match (unit, in_time) {
                    ('W', _) => 7 * 86400,
                    ('D', _) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                total = total.checked_add(n.checked_mul(seconds)?)?;
            }
        }
    }
    Duration::try_seconds(sign * total)
}

/// One `STANDARD` or `DAYLIGHT` block while it is read.
#[derive(Default)]
struct ZonePart {
    offset: Option<i32>,
    before: Option<i32>,
    start: Option<NaiveDateTime>,
    rule: Option<Recur>,
    dates: Vec<NaiveDateTime>,
}

impl ZonePart {
    fn finish(self) -> Option<Observance> {
        let offset = self.offset?;
        Some(Observance {
            offset,
            before: self.before.unwrap_or(offset),
            start: self.start.unwrap_or(NaiveDateTime::MIN),
            rule: self.rule,
            dates: self.dates,
        })
    }
}

fn rules_of(rule: &str) -> HashMap<String, String> {
    rule.split(';')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_uppercase(), v.trim().to_string()))
        .collect()
}

impl Calendar {
    /// Reads the text of an ICS file. Unknown lines are skipped.
    pub fn parse(text: &str) -> Calendar {
        let lines = unfold(text);
        let mut zones: HashMap<String, Arc<Rules>> = HashMap::new();
        let mut items = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim();
            if line.eq_ignore_ascii_case("BEGIN:VTIMEZONE") {
                let (end, tzid, rules) = Self::read_zone(&lines, i + 1);
                if let (Some(id), Some(r)) = (tzid, rules) {
                    zones.insert(id, Arc::new(r));
                }
                i = end;
            } else if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
                let end = lines[i + 1..]
                    .iter()
                    .position(|l| l.trim().eq_ignore_ascii_case("END:VEVENT"))
                    .map_or(lines.len(), |p| i + 1 + p);
                if let Some(item) = Self::read_event(&lines[i + 1..end], &zones) {
                    items.push(item);
                }
                i = end;
            }
            i += 1;
        }
        Calendar { items }
    }

    /// Every block of the zone on its own: links carry the history of a
    /// zone, and old rules must not mix with today's.
    fn read_zone(lines: &[String], from: usize) -> (usize, Option<String>, Option<Rules>) {
        let mut tzid = None;
        let mut parts = Vec::new();
        let mut part: Option<ZonePart> = None;
        let mut i = from;
        while i < lines.len() {
            let line = lines[i].trim();
            match line.to_uppercase().as_str() {
                "END:VTIMEZONE" => break,
                "BEGIN:STANDARD" | "BEGIN:DAYLIGHT" => part = Some(ZonePart::default()),
                "END:STANDARD" | "END:DAYLIGHT" => {
                    parts.extend(part.take().and_then(ZonePart::finish))
                }
                _ => {
                    if let Some((name, _, value)) = split_line(line) {
                        match (name.as_str(), part.as_mut()) {
                            ("TZID", None) => tzid = Some(value.trim().to_string()),
                            ("TZOFFSETTO", Some(p)) => p.offset = utc_offset(&value),
                            ("TZOFFSETFROM", Some(p)) => p.before = utc_offset(&value),
                            ("DTSTART", Some(p)) => p.start = parse_when(&value).map(|w| w.0),
                            ("RRULE", Some(p)) => p.rule = Some(Recur::parse(&value)),
                            ("RDATE", Some(p)) => p.dates.extend(
                                value.split(',').filter_map(|v| parse_when(v).map(|w| w.0)),
                            ),
                            _ => {}
                        }
                    }
                }
            }
            i += 1;
        }
        let rules = (!parts.is_empty()).then_some(Rules { parts });
        (i, tzid, rules)
    }

    fn zone_for(
        params: &HashMap<String, String>,
        utc: bool,
        zones: &HashMap<String, Arc<Rules>>,
    ) -> Zone {
        if utc {
            return Zone::Utc;
        }
        let Some(id) = params.get("TZID") else {
            return Zone::Local;
        };
        if let Some(r) = zones.get(id) {
            return Zone::Rules(r.clone());
        }
        match well_known(id) {
            Some(0) => Zone::Utc,
            Some(offset) => Zone::Fixed(offset),
            None => Zone::Local,
        }
    }

    fn read_event(lines: &[String], zones: &HashMap<String, Arc<Rules>>) -> Option<Item> {
        let mut uid = String::new();
        let mut title = String::new();
        let mut location = String::new();
        let mut cancelled = false;
        let mut start: Option<(NaiveDateTime, Zone, bool)> = None;
        let mut end: Option<(NaiveDateTime, Zone, bool)> = None;
        let mut length: Option<Duration> = None;
        let mut rule = None;
        let mut except_raw: Vec<(String, HashMap<String, String>)> = Vec::new();
        let mut replaces_raw: Option<(String, HashMap<String, String>)> = None;
        let mut nested = 0;
        for line in lines {
            let line = line.trim();
            let upper = line.to_uppercase();
            // Alarms inside an event have their own properties.
            if upper.starts_with("BEGIN:") {
                nested += 1;
                continue;
            }
            if upper.starts_with("END:") {
                nested -= 1;
                continue;
            }
            if nested > 0 {
                continue;
            }
            let Some((name, params, value)) = split_line(line) else {
                continue;
            };
            match name.as_str() {
                "UID" => uid = value.trim().to_string(),
                "SUMMARY" => title = unescape(&value),
                "LOCATION" => location = unescape(&value),
                "STATUS" => cancelled = value.trim().eq_ignore_ascii_case("CANCELLED"),
                "DTSTART" => {
                    let (wall, all_day, utc) = parse_when(&value)?;
                    start = Some((wall, Self::zone_for(&params, utc, zones), all_day));
                }
                "DTEND" => {
                    end = parse_when(&value)
                        .map(|(wall, day, utc)| (wall, Self::zone_for(&params, utc, zones), day))
                }
                "DURATION" => length = parse_duration(&value),
                "RRULE" => rule = Some(Recur::parse(value.trim())),
                "EXDATE" => {
                    for v in value.split(',') {
                        except_raw.push((v.to_string(), params.clone()));
                    }
                }
                "RECURRENCE-ID" => replaces_raw = Some((value, params)),
                _ => {}
            }
        }
        let local_of = |raw: &str, params: &HashMap<String, String>| {
            parse_when(raw).map(|(wall, day, utc)| {
                if day {
                    wall
                } else {
                    Self::zone_for(params, utc, zones).to_local(wall)
                }
            })
        };
        let replaces = replaces_raw.and_then(|(raw, p)| local_of(&raw, &p));
        if cancelled {
            // A cancelled occurrence hides its place in the series; a
            // cancelled event on its own is just gone.
            let replaced = replaces?;
            return Some(Item {
                uid,
                title,
                location,
                start: replaced,
                zone: Zone::Local,
                all_day: false,
                length: Duration::zero(),
                rule: None,
                except: HashSet::new(),
                replaces: Some(replaced),
                cancelled: true,
            });
        }
        let (start, zone, all_day) = start?;
        let length = length
            .or_else(|| {
                end.map(|(e, end_zone, end_day)| {
                    if all_day || end_day {
                        e - start
                    } else {
                        // The end may be written in another zone.
                        end_zone.to_local(e) - zone.to_local(start)
                    }
                })
            })
            .filter(|d| *d >= Duration::zero())
            .unwrap_or(if all_day {
                Duration::days(1)
            } else {
                Duration::zero()
            });
        let except = except_raw
            .iter()
            .filter_map(|(raw, p)| local_of(raw, p))
            .collect();
        Some(Item {
            uid,
            title: if title.is_empty() {
                "Без названия".into()
            } else {
                title
            },
            location,
            start,
            zone,
            all_day,
            length,
            rule,
            except,
            replaces,
            cancelled: false,
        })
    }

    /// Events in the calendar; cancelled occurrences are not counted.
    pub fn len(&self) -> usize {
        self.items.iter().filter(|i| !i.cancelled).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Meetings that overlap `from..to` (local time), the earliest first.
    pub fn between(&self, from: NaiveDateTime, to: NaiveDateTime) -> Vec<Meeting> {
        // Occurrences moved elsewhere or cancelled are dropped from their series.
        let moved: HashSet<(&str, NaiveDateTime)> = self
            .items
            .iter()
            .filter_map(|i| i.replaces.map(|r| (i.uid.as_str(), r)))
            .collect();
        // Zones differ from the computer's by little more than a day.
        let early = from.checked_sub_signed(Duration::days(2)).unwrap_or(from);
        let mut out = Vec::new();
        for item in self.items.iter().filter(|i| !i.cancelled) {
            for wall in starts(item, to) {
                if wall
                    .checked_add_signed(item.length)
                    .is_some_and(|end| end < early)
                {
                    continue;
                }
                let start = if item.all_day {
                    wall
                } else {
                    item.zone.to_local(wall)
                };
                let Some(end) = start.checked_add_signed(item.length) else {
                    continue;
                };
                let lasts = if end > start {
                    end
                } else {
                    start
                        .checked_add_signed(Duration::seconds(1))
                        .unwrap_or(end)
                };
                if lasts <= from || start >= to {
                    continue;
                }
                if item.replaces.is_none()
                    && (item.except.contains(&start) || moved.contains(&(item.uid.as_str(), start)))
                {
                    continue;
                }
                out.push(Meeting {
                    start,
                    end,
                    all_day: item.all_day,
                    title: item.title.clone(),
                    location: item.location.clone(),
                    uid: item.uid.clone(),
                });
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// Wall start times of an item up to `to`, a little past it for zone shifts.
fn starts(item: &Item, to: NaiveDateTime) -> Vec<NaiveDateTime> {
    let s = item.start;
    let rule = item.rule.as_ref().filter(|_| item.replaces.is_none());
    let Some((rule, freq)) = rule.and_then(|r| Some((r, r.freq?))) else {
        return vec![s];
    };
    let limit = to.checked_add_signed(Duration::days(1)).unwrap_or(to);
    let mut out = Vec::new();
    for step in 0..MAX_STEPS {
        let period = step
            .checked_mul(rule.interval)
            .and_then(|n| rule.period(freq, s.date(), n));
        let Some((first, days)) = period else {
            break;
        };
        if first.and_time(s.time()) > limit {
            break;
        }
        for t in days.into_iter().map(|d| d.and_time(s.time())) {
            if t < s {
                continue;
            }
            if t > limit
                || rule.past_until(t, &item.zone)
                || rule.count.is_some_and(|c| out.len() >= c)
                || out.len() >= MAX_OCCURRENCES
            {
                return out;
            }
            out.push(t);
        }
    }
    out
}

/// What the calendar knows now, shared by the reader in the app and the tool.
#[derive(Clone, Default)]
pub struct CalendarState(Arc<RwLock<Loaded>>);

#[derive(Default)]
struct Loaded {
    calendar: Option<Calendar>,
    /// Unix seconds of the last good read.
    fetched_at: Option<i64>,
    error: Option<String>,
    /// The link is set and calendars are on.
    connected: bool,
}

impl CalendarState {
    pub fn set(&self, calendar: Calendar, at: i64) {
        let mut l = self.0.write();
        l.calendar = Some(calendar);
        l.fetched_at = Some(at);
        l.error = None;
        l.connected = true;
    }

    pub fn fail(&self, why: &str) {
        let mut l = self.0.write();
        l.error = Some(why.to_string());
        l.connected = true;
    }

    pub fn disconnect(&self) {
        *self.0.write() = Loaded::default();
    }

    /// (events, unix seconds of the last read, last error).
    pub fn status(&self) -> (usize, Option<i64>, Option<String>) {
        let l = self.0.read();
        (
            l.calendar.as_ref().map_or(0, Calendar::len),
            l.fetched_at,
            l.error.clone(),
        )
    }

    pub fn between(&self, from: NaiveDateTime, to: NaiveDateTime) -> Option<Vec<Meeting>> {
        self.0.read().calendar.as_ref().map(|c| c.between(from, to))
    }

    /// The link was read at least once, well or not, since it was set.
    pub fn connected(&self) -> bool {
        self.0.read().connected
    }
}

/// Meetings that start within `minutes` after `now` and are not all-day.
pub fn starting_soon(meetings: &[Meeting], now: NaiveDateTime, minutes: u32) -> Vec<Meeting> {
    let edge = now + Duration::minutes(minutes as i64);
    meetings
        .iter()
        .filter(|m| !m.all_day && m.start > now && m.start <= edge)
        .cloned()
        .collect()
}

/// `10:00–11:00 Созвон (Переговорная 2)`, or `весь день: Отпуск`.
pub fn meeting_line(m: &Meeting, with_date: bool) -> String {
    let place = if m.location.is_empty() {
        String::new()
    } else {
        format!(" ({})", m.location)
    };
    let date = if with_date {
        format!("{} ", m.start.format("%d.%m"))
    } else {
        String::new()
    };
    if m.all_day {
        format!("{date}весь день: {}{place}", m.title)
    } else if m.end > m.start {
        format!(
            "{date}{}–{} {}{place}",
            m.start.format("%H:%M"),
            m.end.format("%H:%M"),
            m.title
        )
    } else {
        format!("{date}{} {}{place}", m.start.format("%H:%M"), m.title)
    }
}

/// The days of a request: `today`, `tomorrow`, `week` or `YYYY-MM-DD`.
pub fn day_range(day: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate, String)> {
    let range = match day.trim() {
        "" | "today" => (today, today, "сегодня".into()),
        "tomorrow" => {
            let t = today.succ_opt()?;
            (t, t, "завтра".into())
        }
        "week" => (
            today,
            today.checked_add_signed(Duration::days(6))?,
            "на неделю".into(),
        ),
        other => {
            let d = NaiveDate::parse_from_str(other, "%Y-%m-%d").ok()?;
            (d, d, format!("на {}", d.format("%d.%m.%Y")))
        }
    };
    // The search ends at the start of the next day: it has to exist.
    range.1.succ_opt().map(|_| range)
}

pub struct CalendarTool(pub CalendarState);

#[async_trait]
impl Tool for CalendarTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "calendar_events",
            "Встречи и события из календаря пользователя (Google, Outlook, Яндекс по ссылке ICS из настроек). Это не список дел: дела — todo_list.",
            schema(
                &[(
                    "day",
                    "string",
                    "today, tomorrow, week или дата ГГГГ-ММ-ДД",
                )],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let day = arg_str_opt(args, "day").unwrap_or("");
        if day_range(day, Local::now().date_naive()).is_none() {
            return Err(ToolError::BadArguments(format!(
                "не поняла день «{day}»: today, tomorrow, week или ГГГГ-ММ-ДД"
            )));
        }
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть календарь",
            "календарь",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let today = Local::now().date_naive();
        let day = arg_str_opt(&args, "day").unwrap_or("");
        let (from, to, label) = day_range(day, today).unwrap_or((today, today, "сегодня".into()));
        let start = from.and_hms_opt(0, 0, 0).unwrap_or_default();
        let end = to
            .succ_opt()
            .unwrap_or(to)
            .and_hms_opt(0, 0, 0)
            .unwrap_or_default();
        let Some(meetings) = self.0.between(start, end) else {
            let (_, _, error) = self.0.status();
            return Ok(ToolOutput::new(match error {
                Some(e) if self.0.connected() => format!("Календарь сейчас не читается: {e}"),
                _ => "Календарь не подключён: ссылку ICS можно добавить в настройках, раздел «Инструменты». \
Дела из своего списка — todo_list."
                    .to_string(),
            }));
        };
        if meetings.is_empty() {
            return Ok(ToolOutput::new(format!("В календаре {label} ничего нет.")));
        }
        let several_days = from != to;
        let lines: Vec<String> = meetings
            .iter()
            .map(|m| meeting_line(m, several_days))
            .collect();
        Ok(ToolOutput::new(format!(
            "Календарь {label}:\n{}",
            lines.join("\n")
        )))
    }
}

/// The unix time of a local wall time, for comparing with "now".
pub fn local_timestamp(t: NaiveDateTime) -> i64 {
    Local
        .from_local_datetime(&t)
        .earliest()
        .map_or(0, |d: DateTime<Local>| d.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use serde_json::json;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn day(s: &str) -> (NaiveDateTime, NaiveDateTime) {
        let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        (
            d.and_hms_opt(0, 0, 0).unwrap(),
            (d + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap(),
        )
    }

    /// The computer's time of a UTC moment: expectations hold in any zone.
    fn local(utc: &str) -> NaiveDateTime {
        Zone::Utc.to_local(at(utc))
    }

    /// The computer's time of a Moscow wall time.
    fn msk(wall: &str) -> NaiveDateTime {
        Zone::Fixed(3 * 3600).to_local(at(wall))
    }

    /// Starts of the meetings called `title` within `from..to`.
    fn starts_of(
        c: &Calendar,
        title: &str,
        from: NaiveDateTime,
        to: NaiveDateTime,
    ) -> Vec<NaiveDateTime> {
        c.between(from, to)
            .into_iter()
            .filter(|m| m.title == title)
            .map(|m| m.start)
            .collect()
    }

    /// A Google-like file: a floating weekly meeting with an exception and a
    /// moved occurrence, an all-day event, a cancelled one.
    const FLOATING: &str = "BEGIN:VCALENDAR\r
VERSION:2.0\r
BEGIN:VEVENT\r
UID:weekly-1\r
DTSTART:20261102T100000\r
DTEND:20261102T103000\r
RRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261130T235959\r
EXDATE:20261104T100000\r
SUMMARY:Планёрка\\, команда\r
LOCATION:Переговорная 2\r
BEGIN:VALARM\r
TRIGGER:-PT10M\r
DESCRIPTION:Напоминание\r
END:VALARM\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:weekly-1\r
RECURRENCE-ID:20261111T100000\r
DTSTART:20261111T150000\r
DTEND:20261111T153000\r
SUMMARY:Планёрка (перенесена)\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:vacation\r
DTSTART;VALUE=DATE:20261113\r
DTEND;VALUE=DATE:20261114\r
SUMMARY:Отгул\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:gone\r
DTSTART:20261113T120000\r
STATUS:CANCELLED\r
SUMMARY:Отменено\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:long-title\r
DTSTART:20261113T170000\r
DURATION:PT1H30M\r
SUMMARY:Очень длинное название, которое\r
  перенесено на следующую строку\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn repeats_exceptions_moves_and_all_day_events() {
        let c = Calendar::parse(FLOATING);
        assert_eq!(c.len(), 4);
        let (a, b) = day("2026-11-02");
        let m = c.between(a, b);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].title, "Планёрка, команда");
        assert_eq!(
            meeting_line(&m[0], false),
            "10:00–10:30 Планёрка, команда (Переговорная 2)"
        );
        // Wednesday the 4th is cancelled by EXDATE.
        let (a, b) = day("2026-11-04");
        assert!(c.between(a, b).is_empty());
        // Wednesday the 11th moved to 15:00.
        let (a, b) = day("2026-11-11");
        let m = c.between(a, b);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].start, at("2026-11-11 15:00"));
        // Friday the 13th: all day off, a long folded title, nothing cancelled.
        let (a, b) = day("2026-11-13");
        let m = c.between(a, b);
        let lines: Vec<String> = m.iter().map(|x| meeting_line(x, false)).collect();
        assert_eq!(
            lines,
            [
                "весь день: Отгул",
                "17:00–18:30 Очень длинное название, которое перенесено на следующую строку"
            ]
        );
        // UNTIL stops the series.
        let (a, _) = day("2026-11-30");
        let (_, b) = day("2026-12-09");
        let m = c.between(a, b);
        assert_eq!(
            m.iter().map(|x| x.start).collect::<Vec<_>>(),
            [at("2026-11-30 10:00")]
        );
    }

    /// Google: a weekly standup, one occurrence of it cancelled for everyone.
    const GOOGLE_CANCELLED: &str = "BEGIN:VCALENDAR\r
PRODID:-//Google Inc//Google Calendar 70.9054//EN\r
VERSION:2.0\r
CALSCALE:GREGORIAN\r
METHOD:PUBLISH\r
X-WR-CALNAME:Работа\r
X-WR-TIMEZONE:Europe/Moscow\r
BEGIN:VTIMEZONE\r
TZID:Europe/Moscow\r
X-LIC-LOCATION:Europe/Moscow\r
BEGIN:STANDARD\r
TZOFFSETFROM:+0300\r
TZOFFSETTO:+0300\r
TZNAME:MSK\r
DTSTART:19700101T000000\r
END:STANDARD\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Moscow:20261102T100000\r
DTEND;TZID=Europe/Moscow:20261102T101500\r
RRULE:FREQ=WEEKLY;BYDAY=MO\r
DTSTAMP:20261112T090000Z\r
UID:5t2c0r1n8a3l4b6d@google.com\r
CREATED:20261020T080000Z\r
LAST-MODIFIED:20261108T120000Z\r
SEQUENCE:0\r
STATUS:CONFIRMED\r
SUMMARY:Standup\r
TRANSP:OPAQUE\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Moscow:20261109T100000\r
DTEND;TZID=Europe/Moscow:20261109T101500\r
DTSTAMP:20261112T090000Z\r
UID:5t2c0r1n8a3l4b6d@google.com\r
RECURRENCE-ID;TZID=Europe/Moscow:20261109T100000\r
CREATED:20261020T080000Z\r
LAST-MODIFIED:20261108T120000Z\r
SEQUENCE:1\r
STATUS:CANCELLED\r
SUMMARY:Standup\r
TRANSP:OPAQUE\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn a_cancelled_occurrence_leaves_its_series() {
        let c = Calendar::parse(GOOGLE_CANCELLED);
        assert_eq!(c.len(), 1);
        assert_eq!(
            starts_of(
                &c,
                "Standup",
                msk("2026-11-01 00:00"),
                msk("2026-11-20 00:00")
            ),
            [msk("2026-11-02 10:00"), msk("2026-11-16 10:00")]
        );
    }

    /// Outlook (Exchange) and Nextcloud monthly rules.
    const OUTLOOK_MONTHLY: &str = "BEGIN:VCALENDAR\r
METHOD:PUBLISH\r
PRODID:Microsoft Exchange Server 2010\r
VERSION:2.0\r
X-WR-CALNAME:Календарь\r
BEGIN:VTIMEZONE\r
TZID:Russian Standard Time\r
BEGIN:STANDARD\r
DTSTART:16010101T000000\r
TZOFFSETFROM:+0300\r
TZOFFSETTO:+0300\r
END:STANDARD\r
BEGIN:DAYLIGHT\r
DTSTART:16010101T000000\r
TZOFFSETFROM:+0300\r
TZOFFSETTO:+0300\r
END:DAYLIGHT\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
RRULE:FREQ=MONTHLY;COUNT=3;BYDAY=TH;BYSETPOS=3;WKST=SU\r
UID:040000008200E00074C5B7101A82E0080000000010DA3B6C3A52DD01000000000000000010000000F1\r
SUMMARY;LANGUAGE=ru-RU:Третий четверг\r
DTSTART;TZID=Russian Standard Time:20261119T110000\r
DTEND;TZID=Russian Standard Time:20261119T120000\r
CLASS:PUBLIC\r
PRIORITY:5\r
DTSTAMP:20261112T090000Z\r
TRANSP:OPAQUE\r
STATUS:CONFIRMED\r
SEQUENCE:0\r
X-MICROSOFT-CDO-BUSYSTATUS:BUSY\r
END:VEVENT\r
BEGIN:VEVENT\r
RRULE:FREQ=MONTHLY;COUNT=3;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;WKST=SU\r
UID:040000008200E00074C5B7101A82E0080000000010DA3B6C3A52DD01000000000000000010000000F2\r
SUMMARY;LANGUAGE=ru-RU:Последний будний\r
DTSTART;TZID=Russian Standard Time:20261130T110000\r
DTEND;TZID=Russian Standard Time:20261130T120000\r
STATUS:CONFIRMED\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:b7e1c5a2-3f04-4d6e-9a8b-1c2d3e4f5a6b\r
DTSTART;TZID=Russian Standard Time:20261110T110000\r
DTEND;TZID=Russian Standard Time:20261110T113000\r
RRULE:FREQ=MONTHLY;COUNT=4;BYDAY=2TU,4TU\r
SUMMARY:Вторники\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:c8f2d6b3-4a15-4e7f-8b9c-2d3e4f5a6b7c\r
DTSTART;TZID=Russian Standard Time:20261130T110000\r
DTEND;TZID=Russian Standard Time:20261130T113000\r
RRULE:FREQ=MONTHLY;COUNT=4;BYMONTHDAY=-1\r
SUMMARY:Последний день\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn monthly_rules_by_set_position_several_weekdays_and_from_the_end() {
        let c = Calendar::parse(OUTLOOK_MONTHLY);
        let of =
            |title: &str| starts_of(&c, title, msk("2026-11-01 00:00"), msk("2027-06-01 00:00"));
        let all = |days: &[&str]| -> Vec<NaiveDateTime> {
            days.iter().map(|d| msk(&format!("{d} 11:00"))).collect()
        };
        assert_eq!(
            of("Третий четверг"),
            all(&["2026-11-19", "2026-12-17", "2027-01-21"])
        );
        assert_eq!(
            of("Последний будний"),
            all(&["2026-11-30", "2026-12-31", "2027-01-29"])
        );
        assert_eq!(
            of("Вторники"),
            all(&["2026-11-10", "2026-11-24", "2026-12-08", "2026-12-22"])
        );
        assert_eq!(
            of("Последний день"),
            all(&["2026-11-30", "2026-12-31", "2027-01-31", "2027-02-28"])
        );
    }

    /// Thunderbird: «every weekday» is a daily rule limited by weekdays.
    const THUNDERBIRD_WEEKDAYS: &str = "BEGIN:VCALENDAR\r
PRODID:-//Mozilla.org/NONSGML Mozilla Calendar V1.1//EN\r
VERSION:2.0\r
BEGIN:VTIMEZONE\r
TZID:Europe/Moscow\r
X-TZINFO:Europe/Moscow[2024a]\r
BEGIN:STANDARD\r
TZOFFSETTO:+030000\r
TZOFFSETFROM:+040000\r
TZNAME:MSK\r
DTSTART:20141026T020000\r
RDATE:20141026T020000\r
END:STANDARD\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
CREATED:20261101T100000Z\r
LAST-MODIFIED:20261101T100000Z\r
DTSTAMP:20261101T100000Z\r
UID:3f1c2d4e-5a6b-7c8d-9e0f-a1b2c3d4e5f6\r
SUMMARY:Будни\r
RRULE:FREQ=DAILY;BYDAY=MO,TU,WE,TH,FR\r
DTSTART;TZID=Europe/Moscow:20261113T093000\r
DTEND;TZID=Europe/Moscow:20261113T094500\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn a_daily_rule_keeps_to_its_weekdays() {
        let c = Calendar::parse(THUNDERBIRD_WEEKDAYS);
        assert_eq!(
            starts_of(
                &c,
                "Будни",
                msk("2026-11-13 00:00"),
                msk("2026-11-17 00:00")
            ),
            [msk("2026-11-13 09:30"), msk("2026-11-16 09:30")]
        );
    }

    /// Outlook yearly rules: the last Sunday of November, all day, and the
    /// second Monday of March.
    const OUTLOOK_YEARLY: &str = "BEGIN:VCALENDAR\r
METHOD:PUBLISH\r
PRODID:Microsoft Exchange Server 2010\r
VERSION:2.0\r
BEGIN:VEVENT\r
RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=-1SU\r
UID:040000008200E00074C5B7101A82E0080000000010DA3B6C3A52DD01000000000000000010000000F3\r
SUMMARY;LANGUAGE=ru-RU:День матери\r
DTSTART;VALUE=DATE:20261129\r
DTEND;VALUE=DATE:20261130\r
X-MICROSOFT-CDO-ALLDAYEVENT:TRUE\r
END:VEVENT\r
BEGIN:VEVENT\r
RRULE:FREQ=YEARLY;BYDAY=2MO;BYMONTH=3\r
UID:040000008200E00074C5B7101A82E0080000000010DA3B6C3A52DD01000000000000000010000000F4\r
SUMMARY;LANGUAGE=ru-RU:Годовой план\r
DTSTART:20270308T100000\r
DTEND:20270308T110000\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn yearly_rules_by_month_and_weekday() {
        let c = Calendar::parse(OUTLOOK_YEARLY);
        assert_eq!(
            starts_of(
                &c,
                "День матери",
                at("2026-01-01 00:00"),
                at("2029-01-01 00:00")
            ),
            [
                at("2026-11-29 00:00"),
                at("2027-11-28 00:00"),
                at("2028-11-26 00:00")
            ]
        );
        assert_eq!(
            starts_of(
                &c,
                "Годовой план",
                at("2027-01-01 00:00"),
                at("2030-01-01 00:00")
            ),
            [
                at("2027-03-08 10:00"),
                at("2028-03-13 10:00"),
                at("2029-03-12 10:00")
            ]
        );
    }

    #[test]
    fn zones_from_vtimezone_and_well_known_names() {
        let text = "BEGIN:VCALENDAR
BEGIN:VTIMEZONE
TZID:Custom Zone
BEGIN:STANDARD
DTSTART:16010101T030000
TZOFFSETFROM:+0200
TZOFFSETTO:+0100
RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10
END:STANDARD
BEGIN:DAYLIGHT
DTSTART:16010101T020000
TZOFFSETFROM:+0100
TZOFFSETTO:+0200
RRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3
END:DAYLIGHT
END:VTIMEZONE
BEGIN:VEVENT
UID:a
DTSTART;TZID=Custom Zone:20260715T120000
SUMMARY:Лето
END:VEVENT
BEGIN:VEVENT
UID:b
DTSTART;TZID=Custom Zone:20261215T120000
SUMMARY:Зима
END:VEVENT
BEGIN:VEVENT
UID:c
DTSTART;TZID=Europe/Moscow:20261215T120000
SUMMARY:Москва
END:VEVENT
BEGIN:VEVENT
UID:d
DTSTART:20261215T090000Z
SUMMARY:UTC
END:VEVENT
END:VCALENDAR";
        let c = Calendar::parse(text);
        let start = |title: &str| {
            c.between(at("2026-01-01 00:00"), at("2027-01-01 00:00"))
                .into_iter()
                .find(|m| m.title == title)
                .unwrap()
                .start
        };
        assert_eq!(start("Лето"), local("2026-07-15 10:00"));
        assert_eq!(start("Зима"), local("2026-12-15 11:00"));
        assert_eq!(start("Москва"), local("2026-12-15 09:00"));
        assert_eq!(start("UTC"), local("2026-12-15 09:00"));
    }

    /// Europe/Moscow with its history, as libical and tzurl.org write it:
    /// summer time ended in 2010, a year-round +4 from 2011, +3 since 2014.
    const MOSCOW_HISTORY: &str = "BEGIN:VCALENDAR\r
PRODID:-//Nextcloud calendar v1.6.4\r
VERSION:2.0\r
BEGIN:VTIMEZONE\r
TZID:Europe/Moscow\r
LAST-MODIFIED:20240422T053451Z\r
TZURL:https://www.tzurl.org/zoneinfo/Europe/Moscow\r
X-LIC-LOCATION:Europe/Moscow\r
BEGIN:STANDARD\r
TZNAME:MMT\r
TZOFFSETFROM:+023017\r
TZOFFSETTO:+023017\r
DTSTART:18800101T000000\r
END:STANDARD\r
BEGIN:DAYLIGHT\r
TZNAME:MSD\r
TZOFFSETFROM:+0300\r
TZOFFSETTO:+0400\r
DTSTART:19930328T020000\r
RRULE:FREQ=YEARLY;UNTIL=20100327T230000Z;BYMONTH=3;BYDAY=-1SU\r
END:DAYLIGHT\r
BEGIN:STANDARD\r
TZNAME:MSK\r
TZOFFSETFROM:+0400\r
TZOFFSETTO:+0300\r
DTSTART:19961027T030000\r
RRULE:FREQ=YEARLY;UNTIL=20101030T230000Z;BYMONTH=10;BYDAY=-1SU\r
END:STANDARD\r
BEGIN:STANDARD\r
TZNAME:MSK\r
TZOFFSETFROM:+0300\r
TZOFFSETTO:+0400\r
DTSTART:20110327T020000\r
RDATE:20110327T020000\r
END:STANDARD\r
BEGIN:STANDARD\r
TZNAME:MSK\r
TZOFFSETFROM:+0400\r
TZOFFSETTO:+0300\r
DTSTART:20141026T020000\r
RDATE:20141026T020000\r
END:STANDARD\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
UID:summer-2026\r
DTSTART;TZID=Europe/Moscow:20260715T120000\r
SUMMARY:Лето 2026\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:winter-2026\r
DTSTART;TZID=Europe/Moscow:20260115T120000\r
SUMMARY:Зима 2026\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:summer-2009\r
DTSTART;TZID=Europe/Moscow:20090715T120000\r
SUMMARY:Лето 2009\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:winter-2012\r
DTSTART;TZID=Europe/Moscow:20120115T120000\r
SUMMARY:Зима 2012\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn a_zone_history_keeps_old_rules_in_their_years() {
        let c = Calendar::parse(MOSCOW_HISTORY);
        let all = c.between(at("2009-01-01 00:00"), at("2027-01-01 00:00"));
        let start = |title: &str| all.iter().find(|m| m.title == title).unwrap().start;
        assert_eq!(start("Лето 2026"), local("2026-07-15 09:00"));
        assert_eq!(start("Зима 2026"), local("2026-01-15 09:00"));
        assert_eq!(start("Лето 2009"), local("2009-07-15 08:00"));
        assert_eq!(start("Зима 2012"), local("2012-01-15 08:00"));
    }

    /// Outlook: a daily series in Kamchatka time, its UNTIL the start of the
    /// last occurrence in UTC.
    const OUTLOOK_UNTIL: &str = "BEGIN:VCALENDAR\r
METHOD:PUBLISH\r
PRODID:Microsoft Exchange Server 2010\r
VERSION:2.0\r
BEGIN:VTIMEZONE\r
TZID:Russia Time Zone 11\r
BEGIN:STANDARD\r
DTSTART:16010101T000000\r
TZOFFSETFROM:+1200\r
TZOFFSETTO:+1200\r
END:STANDARD\r
BEGIN:DAYLIGHT\r
DTSTART:16010101T000000\r
TZOFFSETFROM:+1200\r
TZOFFSETTO:+1200\r
END:DAYLIGHT\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
RRULE:FREQ=DAILY;UNTIL=20261104T220000Z;INTERVAL=1\r
UID:040000008200E00074C5B7101A82E0080000000010DA3B6C3A52DD01000000000000000010000000F5\r
SUMMARY;LANGUAGE=ru-RU:Камчатка\r
DTSTART;TZID=Russia Time Zone 11:20261102T100000\r
DTEND;TZID=Russia Time Zone 11:20261102T103000\r
STATUS:CONFIRMED\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn until_in_utc_keeps_the_last_occurrence() {
        let c = Calendar::parse(OUTLOOK_UNTIL);
        assert_eq!(
            starts_of(
                &c,
                "Камчатка",
                local("2026-10-30 00:00"),
                local("2026-11-10 00:00")
            ),
            [
                local("2026-11-01 22:00"),
                local("2026-11-02 22:00"),
                local("2026-11-03 22:00"),
                local("2026-11-04 22:00")
            ]
        );
    }

    /// Google: a flight that lands in another zone, and one with its end in UTC.
    const GOOGLE_FLIGHT: &str = "BEGIN:VCALENDAR\r
PRODID:-//Google Inc//Google Calendar 70.9054//EN\r
VERSION:2.0\r
BEGIN:VTIMEZONE\r
TZID:Europe/Moscow\r
X-LIC-LOCATION:Europe/Moscow\r
BEGIN:STANDARD\r
TZOFFSETFROM:+0300\r
TZOFFSETTO:+0300\r
TZNAME:MSK\r
DTSTART:19700101T000000\r
END:STANDARD\r
END:VTIMEZONE\r
BEGIN:VTIMEZONE\r
TZID:Asia/Novosibirsk\r
X-LIC-LOCATION:Asia/Novosibirsk\r
BEGIN:STANDARD\r
TZOFFSETFROM:+0700\r
TZOFFSETTO:+0700\r
TZNAME:GMT+7\r
DTSTART:19700101T000000\r
END:STANDARD\r
END:VTIMEZONE\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Moscow:20261113T100000\r
DTEND;TZID=Asia/Novosibirsk:20261113T170000\r
UID:flight-out@google.com\r
SUMMARY:Перелёт\r
END:VEVENT\r
BEGIN:VEVENT\r
DTSTART;TZID=Europe/Moscow:20261114T100000\r
DTEND:20261114T080000Z\r
UID:flight-back@google.com\r
SUMMARY:Обратно\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn the_end_is_read_in_its_own_zone() {
        let c = Calendar::parse(GOOGLE_FLIGHT);
        let all = c.between(local("2026-11-12 00:00"), local("2026-11-15 00:00"));
        let times = |title: &str| {
            let m = all.iter().find(|m| m.title == title).unwrap();
            (m.start, m.end)
        };
        assert_eq!(
            times("Перелёт"),
            (local("2026-11-13 07:00"), local("2026-11-13 10:00"))
        );
        assert_eq!(
            times("Обратно"),
            (local("2026-11-14 07:00"), local("2026-11-14 08:00"))
        );
    }

    #[test]
    fn russian_zones_are_known_without_description() {
        for (name, hours) in [
            ("N. Central Asia Standard Time", 7),
            ("Altai Standard Time", 7),
            ("Tomsk Standard Time", 7),
            ("Transbaikal Standard Time", 9),
            ("Magadan Standard Time", 11),
            ("Sakhalin Standard Time", 11),
            ("Russia Time Zone 10", 11),
            ("Russia Time Zone 11", 12),
            ("Volgograd Standard Time", 3),
            ("Astrakhan Standard Time", 4),
            ("Saratov Standard Time", 4),
            ("Europe/Volgograd", 3),
            ("Europe/Kirov", 3),
            ("Europe/Astrakhan", 4),
            ("Europe/Saratov", 4),
            ("Europe/Ulyanovsk", 4),
            ("Asia/Barnaul", 7),
            ("Asia/Tomsk", 7),
            ("Asia/Novokuznetsk", 7),
            ("Asia/Chita", 9),
            ("Asia/Sakhalin", 11),
            ("Asia/Anadyr", 12),
        ] {
            assert_eq!(well_known(name), Some(hours * 3600), "{name}");
        }
        let c = Calendar::parse(
            "BEGIN:VEVENT\r
UID:nsk\r
DTSTART;TZID=N. Central Asia Standard Time:20261116T140000\r
SUMMARY:Новосибирск\r
END:VEVENT\r
",
        );
        assert_eq!(
            starts_of(
                &c,
                "Новосибирск",
                local("2026-11-15 00:00"),
                local("2026-11-18 00:00")
            ),
            [local("2026-11-16 07:00")]
        );
    }

    #[test]
    fn non_ascii_weekdays_are_skipped() {
        assert_eq!(by_day("ÜS"), None);
        assert_eq!(by_day("1Ü"), None);
        assert_eq!(by_day("-1SU"), Some((-1, Weekday::Sun)));
        assert_eq!(by_day("+2tu"), Some((2, Weekday::Tue)));
        assert_eq!(by_day("2147483647MO"), None);
        let c = Calendar::parse(
            "BEGIN:VCALENDAR
BEGIN:VTIMEZONE
TZID:Broken
BEGIN:STANDARD
DTSTART:16010101T030000
TZOFFSETFROM:+0400
TZOFFSETTO:+0300
RRULE:FREQ=YEARLY;BYDAY=ÜS;BYMONTH=10
END:STANDARD
END:VTIMEZONE
BEGIN:VEVENT
UID:a
DTSTART;TZID=Broken:20261113T100000
RRULE:FREQ=WEEKLY;BYDAY=FR,MÖ
SUMMARY:Пятница
END:VEVENT
END:VCALENDAR",
        );
        assert_eq!(c.len(), 1);
        assert_eq!(
            starts_of(
                &c,
                "Пятница",
                msk("2026-11-13 00:00"),
                msk("2026-11-21 00:00")
            ),
            [msk("2026-11-13 10:00"), msk("2026-11-20 10:00")]
        );
    }

    #[test]
    fn broken_numbers_do_not_panic() {
        let lines = [
            "DURATION:P99999999D",
            "DURATION:PT9223372036854775807S",
            "DURATION:P9999999999999999W",
            "RRULE:FREQ=DAILY;INTERVAL=100000000",
            "RRULE:FREQ=DAILY;INTERVAL=9223372036854775807",
            "RRULE:FREQ=WEEKLY;INTERVAL=9223372036854775807;BYDAY=MO,SU",
            "RRULE:FREQ=MONTHLY;INTERVAL=9223372036854775807",
            "RRULE:FREQ=YEARLY;INTERVAL=9223372036854775807",
            "RRULE:FREQ=MONTHLY;BYDAY=2147483647MO",
            "RRULE:FREQ=MONTHLY;BYDAY=-2147483648MO",
            "RRULE:FREQ=MONTHLY;BYMONTHDAY=-2147483648,-31;BYSETPOS=-2147483648,-366,366",
            "RRULE:FREQ=YEARLY;BYDAY=MO,TU,WE,TH,FR,SA,SU;COUNT=18446744073709551615",
            "RRULE:FREQ=DAILY;UNTIL=99991231T235959Z",
        ];
        for extra in lines {
            for start in [
                "20261113T100000",
                "20261113T100000Z",
                "99991231T235959",
                "00010101T000000",
            ] {
                let c = Calendar::parse(&format!(
                    "BEGIN:VEVENT\nUID:x\nDTSTART:{start}\n{extra}\nSUMMARY:x\nEND:VEVENT"
                ));
                c.between(at("2026-11-01 00:00"), at("2026-12-01 00:00"));
                c.between(NaiveDateTime::MIN, NaiveDateTime::MAX);
            }
        }
        assert_eq!(parse_duration("PT9223372036854775807S"), None);
        let last = NaiveDate::MAX.format("%Y-%m-%d").to_string();
        assert!(day_range(&last, NaiveDate::MAX).is_none());
        assert!(day_range("tomorrow", NaiveDate::MAX).is_none());
    }

    #[test]
    fn monthly_by_weekday_daily_count_and_helpers() {
        let text = "BEGIN:VEVENT
UID:m
DTSTART:20261110T090000
RRULE:FREQ=MONTHLY;BYDAY=2TU;COUNT=3
SUMMARY:Отчёт
END:VEVENT
BEGIN:VEVENT
UID:d
DTSTART:20261113T080000
RRULE:FREQ=DAILY;INTERVAL=2;COUNT=2
SUMMARY:Зарядка
END:VEVENT
BEGIN:VEVENT
UID:y
DTSTART;VALUE=DATE:20200229
RRULE:FREQ=YEARLY
SUMMARY:Високосный
END:VEVENT";
        let c = Calendar::parse(text);
        let all = c.between(at("2026-11-01 00:00"), at("2027-12-31 00:00"));
        let report: Vec<String> = all
            .iter()
            .filter(|m| m.title == "Отчёт")
            .map(|m| m.start.format("%Y-%m-%d").to_string())
            .collect();
        assert_eq!(report, ["2026-11-10", "2026-12-08", "2027-01-12"]);
        let gym: Vec<NaiveDateTime> = all
            .iter()
            .filter(|m| m.title == "Зарядка")
            .map(|m| m.start)
            .collect();
        assert_eq!(gym, [at("2026-11-13 08:00"), at("2026-11-15 08:00")]);
        assert!(all.iter().all(|m| m.title != "Високосный"));
        let soon = starting_soon(&all, at("2026-11-13 07:52"), 10);
        assert_eq!(soon.len(), 1);
        assert!(starting_soon(&all, at("2026-11-13 07:40"), 10).is_empty());
        let march = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
        assert_eq!(
            weekdays_in(march, month_end(march).unwrap(), Weekday::Sun, -1),
            [NaiveDate::from_ymd_opt(2026, 3, 29).unwrap()]
        );
        assert_eq!(parse_duration("P1DT2H"), Some(Duration::hours(26)));
        assert_eq!(utc_offset("-0530"), Some(-(5 * 3600 + 30 * 60)));
        let today = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        assert_eq!(
            day_range("week", today).unwrap().1,
            NaiveDate::from_ymd_opt(2026, 11, 19).unwrap()
        );
        assert!(day_range("послезавтра", today).is_none());
    }

    #[tokio::test]
    async fn the_tool_says_when_no_calendar_is_connected() {
        let state = CalendarState::default();
        let tool = CalendarTool(state.clone());
        let out = run(&tool, json!({})).await.unwrap().text;
        assert!(out.contains("не подключён"), "{out}");
        let today = Local::now().format("%Y%m%d").to_string();
        state.set(
            Calendar::parse(&format!(
                "BEGIN:VEVENT\nUID:x\nDTSTART:{today}T235800\nSUMMARY:Поздний звонок\nEND:VEVENT"
            )),
            0,
        );
        let out = run(&tool, json!({"day": "today"})).await.unwrap().text;
        assert!(out.contains("23:58 Поздний звонок"), "{out}");
        state.fail("сеть");
        assert!(run(&tool, json!({"day": "today"}))
            .await
            .unwrap()
            .text
            .contains("Поздний"));
        assert!(tool.assess(&json!({"day": "вчера"})).is_err());
        let last = NaiveDate::MAX.format("%Y-%m-%d").to_string();
        assert!(tool.assess(&json!({ "day": last })).is_err());
    }
}
