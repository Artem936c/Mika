//! Meetings from a calendar published as an ICS link (Google, Outlook, Яндекс).
//! The parser knows what such links really carry: single and repeating events,
//! cancelled and moved occurrences, time zones described by `VTIMEZONE`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{
    DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
    Weekday,
};
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use parking_lot::RwLock;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::util::schema;

/// Occurrences one repeating event may produce while expanding, a guard
/// against broken rules.
const MAX_STEPS: usize = 5000;

/// A day-of-year rule of a time zone change: the `nth` `weekday` of `month`
/// (negative counts from the end), at `at` wall time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Transition {
    month: u32,
    nth: i32,
    weekday: Weekday,
    at: NaiveTime,
}

impl Transition {
    fn date(&self, year: i32) -> Option<NaiveDateTime> {
        nth_weekday(year, self.month, self.weekday, self.nth).map(|d| d.and_time(self.at))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Rules {
    /// Seconds east of UTC in standard time.
    standard: i32,
    /// Daylight offset with the moments daylight and standard time begin.
    daylight: Option<(i32, Transition, Transition)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Zone {
    Utc,
    /// Floating time and unknown zones: the computer's own time.
    Local,
    Rules(Rules),
}

impl Zone {
    /// The computer's local time for a wall time in this zone.
    fn to_local(&self, wall: NaiveDateTime) -> NaiveDateTime {
        let offset = match self {
            Zone::Local => return wall,
            Zone::Utc => 0,
            Zone::Rules(r) => r.offset_at(wall),
        };
        let utc = wall - Duration::seconds(offset as i64);
        Utc.from_utc_datetime(&utc)
            .with_timezone(&Local)
            .naive_local()
    }
}

impl Rules {
    fn offset_at(&self, wall: NaiveDateTime) -> i32 {
        let Some((dst, starts, ends)) = &self.daylight else {
            return self.standard;
        };
        let year = wall.year();
        let (Some(a), Some(b)) = (starts.date(year), ends.date(year)) else {
            return self.standard;
        };
        let summer = if a < b {
            wall >= a && wall < b
        } else {
            // Southern half: daylight time spans the new year.
            wall >= a || wall < b
        };
        if summer {
            *dst
        } else {
            self.standard
        }
    }
}

/// The `nth` weekday of a month; −1 is the last one.
fn nth_weekday(year: i32, month: u32, weekday: Weekday, nth: i32) -> Option<NaiveDate> {
    if nth > 0 {
        let first = NaiveDate::from_ymd_opt(year, month, 1)?;
        let shift = (7 + weekday.num_days_from_monday() as i64
            - first.weekday().num_days_from_monday() as i64)
            % 7;
        let d = first + Duration::days(shift + 7 * (nth as i64 - 1));
        (d.month() == month).then_some(d)
    } else if nth < 0 {
        let next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        let last = next - Duration::days(1);
        let shift = (7 + last.weekday().num_days_from_monday() as i64
            - weekday.num_days_from_monday() as i64)
            % 7;
        let d = last - Duration::days(shift + 7 * (-nth as i64 - 1));
        (d.month() == month).then_some(d)
    } else {
        None
    }
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
    let split = item.len().checked_sub(2)?;
    let (n, code) = item.split_at(split);
    let nth = if n.is_empty() {
        0
    } else {
        n.trim_start_matches('+').parse().ok()?
    };
    Some((nth, weekday_of(code)?))
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

/// Zones that links name without describing them.
fn well_known(tzid: &str) -> Option<i32> {
    let hours = match tzid {
        "Europe/Moscow" | "Russian Standard Time" | "Europe/Simferopol" | "Europe/Minsk" => 3,
        "Europe/Kaliningrad" | "Kaliningrad Standard Time" => 2,
        "Europe/Samara" | "Russia Time Zone 3" => 4,
        "Asia/Yekaterinburg" | "Ekaterinburg Standard Time" => 5,
        "Asia/Omsk" | "Omsk Standard Time" => 6,
        "Asia/Novosibirsk" | "Asia/Krasnoyarsk" | "North Asia Standard Time" => 7,
        "Asia/Irkutsk" | "North Asia East Standard Time" => 8,
        "Asia/Yakutsk" | "Yakutsk Standard Time" => 9,
        "Asia/Vladivostok" | "Vladivostok Standard Time" => 10,
        "Asia/Magadan" => 11,
        "Asia/Kamchatka" => 12,
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
    rule: Option<String>,
    /// Local start times of cancelled occurrences.
    except: HashSet<NaiveDateTime>,
    /// Set on a moved occurrence: the local start it replaces.
    replaces: Option<NaiveDateTime>,
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

/// A date or date-time value: (wall time, all day, written in UTC).
fn parse_when(value: &str) -> Option<(NaiveDateTime, bool, bool)> {
    let v = value.trim();
    if v.len() == 8 {
        let d = NaiveDate::parse_from_str(v, "%Y%m%d").ok()?;
        return Some((d.and_hms_opt(0, 0, 0)?, true, false));
    }
    let utc = v.ends_with('Z');
    let t = NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok()?;
    Some((t, false, utc))
}

/// `PT1H30M`, `P1D`, `P2W` → duration.
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
                total += n * match (unit, in_time) {
                    ('W', _) => 7 * 86400,
                    ('D', _) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(Duration::seconds(sign * total))
}

/// One `STANDARD` or `DAYLIGHT` block of a `VTIMEZONE`.
#[derive(Default)]
struct ZonePart {
    offset: Option<i32>,
    start: Option<NaiveDateTime>,
    rule: Option<String>,
}

impl ZonePart {
    fn transition(&self) -> Option<Transition> {
        let start = self.start?;
        let rule = rules_of(self.rule.as_deref()?);
        let month: u32 = rule.get("BYMONTH")?.parse().ok()?;
        let (nth, weekday) = by_day(rule.get("BYDAY")?.split(',').next()?)?;
        Some(Transition {
            month,
            nth: if nth == 0 { 1 } else { nth },
            weekday,
            at: start.time(),
        })
    }
}

fn rules_of(rule: &str) -> HashMap<String, String> {
    rule.split(';')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_uppercase(), v.trim().to_string()))
        .collect()
}

fn zone_rules(standard: &ZonePart, daylight: &ZonePart) -> Option<Rules> {
    let std_offset = standard.offset.or(daylight.offset)?;
    let daylight = match (
        daylight.offset,
        daylight.transition(),
        standard.transition(),
    ) {
        (Some(dst), Some(a), Some(b)) => Some((dst, a, b)),
        _ => None,
    };
    Some(Rules {
        standard: std_offset,
        daylight,
    })
}

impl Calendar {
    /// Reads the text of an ICS file. Unknown lines are skipped.
    pub fn parse(text: &str) -> Calendar {
        let lines = unfold(text);
        let mut zones: HashMap<String, Rules> = HashMap::new();
        let mut items = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i].trim();
            if line.eq_ignore_ascii_case("BEGIN:VTIMEZONE") {
                let (end, tzid, rules) = Self::read_zone(&lines, i + 1);
                if let (Some(id), Some(r)) = (tzid, rules) {
                    zones.insert(id, r);
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

    fn read_zone(lines: &[String], from: usize) -> (usize, Option<String>, Option<Rules>) {
        let mut tzid = None;
        let (mut standard, mut daylight) = (ZonePart::default(), ZonePart::default());
        let mut part: Option<bool> = None;
        let mut i = from;
        while i < lines.len() {
            let line = lines[i].trim();
            let upper = line.to_uppercase();
            match upper.as_str() {
                "END:VTIMEZONE" => break,
                "BEGIN:STANDARD" => part = Some(false),
                "BEGIN:DAYLIGHT" => part = Some(true),
                "END:STANDARD" | "END:DAYLIGHT" => part = None,
                _ => {
                    if let Some((name, _, value)) = split_line(line) {
                        let target = match part {
                            Some(true) => Some(&mut daylight),
                            Some(false) => Some(&mut standard),
                            None => None,
                        };
                        match (name.as_str(), target) {
                            ("TZID", None) => tzid = Some(value.trim().to_string()),
                            ("TZOFFSETTO", Some(p)) => p.offset = utc_offset(&value),
                            ("DTSTART", Some(p)) => p.start = parse_when(&value).map(|w| w.0),
                            ("RRULE", Some(p)) => p.rule = Some(value),
                            _ => {}
                        }
                    }
                }
            }
            i += 1;
        }
        (i, tzid, zone_rules(&standard, &daylight))
    }

    fn zone_for(
        params: &HashMap<String, String>,
        utc: bool,
        zones: &HashMap<String, Rules>,
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
            Some(offset) => Zone::Rules(Rules {
                standard: offset,
                daylight: None,
            }),
            None => Zone::Local,
        }
    }

    fn read_event(lines: &[String], zones: &HashMap<String, Rules>) -> Option<Item> {
        let mut uid = String::new();
        let mut title = String::new();
        let mut location = String::new();
        let mut start: Option<(NaiveDateTime, Zone, bool)> = None;
        let mut end: Option<NaiveDateTime> = None;
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
                "STATUS" if value.trim().eq_ignore_ascii_case("CANCELLED") => return None,
                "DTSTART" => {
                    let (wall, all_day, utc) = parse_when(&value)?;
                    start = Some((wall, Self::zone_for(&params, utc, zones), all_day));
                }
                "DTEND" => end = parse_when(&value).map(|w| w.0),
                "DURATION" => length = parse_duration(&value),
                "RRULE" => rule = Some(value.trim().to_string()),
                "EXDATE" => {
                    for v in value.split(',') {
                        except_raw.push((v.to_string(), params.clone()));
                    }
                }
                "RECURRENCE-ID" => replaces_raw = Some((value, params)),
                _ => {}
            }
        }
        let (start, zone, all_day) = start?;
        let length = length
            .or_else(|| end.map(|e| e - start))
            .filter(|d| *d >= Duration::zero())
            .unwrap_or(if all_day {
                Duration::days(1)
            } else {
                Duration::zero()
            });
        let local_of = |raw: &str, params: &HashMap<String, String>| {
            parse_when(raw).map(|(wall, day, utc)| {
                if day {
                    wall
                } else {
                    Self::zone_for(params, utc, zones).to_local(wall)
                }
            })
        };
        let except = except_raw
            .iter()
            .filter_map(|(raw, p)| local_of(raw, p))
            .collect();
        let replaces = replaces_raw.and_then(|(raw, p)| local_of(&raw, &p));
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
        })
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Meetings that overlap `from..to` (local time), the earliest first.
    pub fn between(&self, from: NaiveDateTime, to: NaiveDateTime) -> Vec<Meeting> {
        // Occurrences moved elsewhere are dropped from their series.
        let moved: HashSet<(String, NaiveDateTime)> = self
            .items
            .iter()
            .filter_map(|i| i.replaces.map(|r| (i.uid.clone(), r)))
            .collect();
        let mut out = Vec::new();
        for item in &self.items {
            for wall in starts(item, to) {
                let start = if item.all_day {
                    wall
                } else {
                    item.zone.to_local(wall)
                };
                let end = start + item.length;
                let lasts = if end > start {
                    end
                } else {
                    start + Duration::seconds(1)
                };
                if lasts <= from || start >= to {
                    continue;
                }
                if item.replaces.is_none()
                    && (item.except.contains(&start) || moved.contains(&(item.uid.clone(), start)))
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
    let limit = to + Duration::days(1);
    let Some(rule) = item.rule.as_deref().filter(|_| item.replaces.is_none()) else {
        return vec![item.start];
    };
    let r = rules_of(rule);
    let interval: i64 = r
        .get("INTERVAL")
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(1);
    let count: Option<usize> = r.get("COUNT").and_then(|v| v.parse().ok());
    let until: Option<NaiveDateTime> = r.get("UNTIL").and_then(|v| {
        parse_when(v).map(|(wall, day, utc)| {
            if day {
                wall + Duration::days(1) - Duration::seconds(1)
            } else if utc {
                Zone::Utc.to_local(wall)
            } else {
                wall
            }
        })
    });
    let days: Vec<(i32, Weekday)> = r
        .get("BYDAY")
        .map(|v| v.split(',').filter_map(by_day).collect())
        .unwrap_or_default();
    let month_day: Option<u32> = r
        .get("BYMONTHDAY")
        .and_then(|v| v.split(',').next()?.parse().ok());
    let s = item.start;
    let mut out = Vec::new();
    let push = |t: NaiveDateTime, out: &mut Vec<NaiveDateTime>| -> bool {
        if t < s {
            return true;
        }
        if until.is_some_and(|u| t > u) || t > limit {
            return false;
        }
        if count.is_some_and(|c| out.len() >= c) {
            return false;
        }
        out.push(t);
        true
    };
    let freq = r.get("FREQ").map(String::as_str).unwrap_or("");
    for step in 0..MAX_STEPS as i64 {
        let more = match freq {
            "DAILY" => push(s + Duration::days(step * interval), &mut out),
            "WEEKLY" => {
                let monday = s.date() - Duration::days(s.weekday().num_days_from_monday() as i64)
                    + Duration::weeks(step * interval);
                let mut week: Vec<NaiveDateTime> = if days.is_empty() {
                    vec![
                        (monday + Duration::days(s.weekday().num_days_from_monday() as i64))
                            .and_time(s.time()),
                    ]
                } else {
                    days.iter()
                        .map(|(_, w)| {
                            (monday + Duration::days(w.num_days_from_monday() as i64))
                                .and_time(s.time())
                        })
                        .collect()
                };
                week.sort();
                let mut go = true;
                for t in week {
                    if !push(t, &mut out) {
                        go = false;
                        break;
                    }
                }
                go
            }
            "MONTHLY" => {
                let months = s.month0() as i64 + step * interval;
                let (year, month) = (s.year() + (months / 12) as i32, (months % 12) as u32 + 1);
                let date = match (days.first(), month_day) {
                    (Some((nth, w)), _) if *nth != 0 => nth_weekday(year, month, *w, *nth),
                    (_, Some(d)) => NaiveDate::from_ymd_opt(year, month, d),
                    _ => NaiveDate::from_ymd_opt(year, month, s.day()),
                };
                match date {
                    Some(d) => push(d.and_time(s.time()), &mut out),
                    // The 31st in a short month: no meeting that month.
                    None => NaiveDate::from_ymd_opt(year, month, 1)
                        .is_some_and(|d| d.and_time(s.time()) <= limit),
                }
            }
            "YEARLY" => {
                let year = s.year() + (step * interval) as i32;
                match NaiveDate::from_ymd_opt(year, s.month(), s.day()) {
                    Some(d) => push(d.and_time(s.time()), &mut out),
                    None => year <= limit.year(),
                }
            }
            _ => {
                out.push(s);
                false
            }
        };
        if !more {
            break;
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
    Some(match day.trim() {
        "" | "today" => (today, today, "сегодня".into()),
        "tomorrow" => {
            let t = today + Duration::days(1);
            (t, t, "завтра".into())
        }
        "week" => (today, today + Duration::days(6), "на неделю".into()),
        other => {
            let d = NaiveDate::parse_from_str(other, "%Y-%m-%d").ok()?;
            (d, d, format!("на {}", d.format("%d.%m.%Y")))
        }
    })
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
        let end = (to + Duration::days(1))
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
        let local = |utc: &str| Zone::Utc.to_local(at(utc));
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
        assert_eq!(
            nth_weekday(2026, 3, Weekday::Sun, -1),
            NaiveDate::from_ymd_opt(2026, 3, 29)
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
    }
}
