//! A small iCalendar (RFC 5545) reader for the agenda (crate::agenda): the
//! events of a calendar feed that fall in a time window, recurring ones
//! expanded.
//!
//! It reads what Google, Outlook.com and iCloud feeds use: VEVENT with
//! SUMMARY, LOCATION and URL; DTSTART / DTEND / DURATION as UTC, zoned
//! (TZID), floating or all-day (DATE) values; RRULE for daily, weekly,
//! monthly and yearly repeats (INTERVAL, COUNT, UNTIL, BYDAY, BYMONTHDAY,
//! BYMONTH), EXDATE, RDATE, RECURRENCE-ID overrides and cancelled events.
//! Zones come from `super::tz::Zones` (ICU in Windows); a zone it does not
//! know uses the feed's own VTIMEZONE rules, else the PC's zone.
//!
//! A feed is untrusted text: anything malformed drops that property or
//! event, and nothing panics. A feed cut off before its END:VCALENDAR is
//! an error (the caller keeps what it had). Repeats it cannot expand
//! (hourly ones, BYSETPOS, BYWEEKNO...) keep their first occurrence only.
//!
//! The work is bounded first by hard caps on the input, checked before and
//! while the lines are unfolded, so every later stage is small by
//! construction. A feed past any of them is an error (the caller keeps
//! what it had): more than MAX_INPUT bytes; a logical line we keep longer
//! than MAX_LINE; more than MAX_PARAMS parameters on a line; a value longer
//! than MAX_VALUE; a TZID longer than MAX_TZID; more than MAX_COMPS
//! components, MAX_VEVENTS events or MAX_ZONES different TZIDs; a VTIMEZONE
//! with more than MAX_OBS blocks or MAX_OBS_DATES dates in one. Properties
//! this reader never uses (DESCRIPTION, X-ALT-DESC and the like) are
//! skipped as they stream past, never stored, whatever their length.
//!
//! Then one budget for the whole feed (MAX_WORK steps, counted from its
//! first byte) is the backstop: unfolding, every line, every list value,
//! every time conversion, every zone's rule generation and every step of
//! expanding a repeat takes from it, before the work is done. Running out
//! while the feed is still being read (its components, zones and events)
//! is an error: what is known would be wrong. Running out while repeats are
//! being expanded stops there and returns what was found.
//!
//! Beyond that: components nest at most MAX_DEPTH deep (deeper ones are
//! skipped, counted, never built), a rule walks at most MAX_STEPS periods,
//! a feed gives at most MAX_EVENTS events. A series whose EXDATE or RDATE
//! list is longer than MAX_DATES is left out whole (Parsed::dropped), never
//! shown with exceptions missing. Of several events with one UID and no
//! RECURRENCE-ID (masters), only the first is used. Time zone names are
//! kept once each (an index in every time value, never a copy).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use super::tz::{civil_from_days, days_from_civil, days_in_month, weekday, Zones};

const DAY: i64 = 86_400;
/// Periods (days, weeks, months, years) one rule may walk through.
const MAX_STEPS: i64 = 100_000;
/// Events one feed may give.
const MAX_EVENTS: usize = 20_000;
/// Longest title, location or URL kept (characters).
const MAX_TEXT: usize = 2_000;
/// Deepest nesting read (VCALENDAR > VEVENT > VALARM is 3).
const MAX_DEPTH: usize = 32;
/// Hard caps on the input (see the module docs): bytes, a logical line we
/// keep, parameters on a line, a value, a TZID, components, events and
/// different TZIDs.
const MAX_INPUT: usize = 8 * 1024 * 1024;
const MAX_LINE: usize = 8 * 1024;
const MAX_PARAMS: usize = 16;
const MAX_VALUE: usize = 4 * 1024;
const MAX_TZID: usize = 64;
const MAX_COMPS: usize = 20_000;
const MAX_VEVENTS: usize = 10_000;
const MAX_ZONES: usize = 64;
/// The properties this reader uses; every other one is skipped unread.
const KEPT: [&str; 18] = [
    "BEGIN", "END", "SUMMARY", "LOCATION", "URL", "DTSTART", "DTEND", "DURATION", "RRULE", "EXDATE", "RDATE", "RECURRENCE-ID", "STATUS", "UID", "X-WR-CALNAME", "TZID", "TZOFFSETFROM", "TZOFFSETTO",
];
/// EXDATE and RDATE values one event may have (each); past that the series
/// is left out.
const MAX_DATES: usize = 5_000;
/// Steps (lines, list values, conversions, periods walked, occurrences
/// tried) one feed may take.
const MAX_WORK: u64 = 10_000_000;
/// BYDAY entries a rule may have (with ordinals, after duplicates go).
const MAX_BYDAY: usize = 53;
/// STANDARD / DAYLIGHT blocks one VTIMEZONE may have, and RDATE values
/// one of them may have.
const MAX_OBS: usize = 64;
const MAX_OBS_DATES: usize = 1_000;
/// Years of a feed zone's rule transitions kept worked out; past that a
/// year is worked out again each time it is asked (and paid for again).
const MAX_ZONE_YEARS: usize = 256;

/// The feed's one work budget (MAX_WORK steps), counted from its first
/// line.
struct Budget(Cell<u64>);

impl Budget {
    fn new() -> Self {
        Budget(Cell::new(0))
    }
    /// Takes `n` steps; false once the budget is spent.
    fn spend(&self, n: u64) -> bool {
        let w = self.0.get().saturating_add(n);
        self.0.set(w);
        w <= MAX_WORK
    }
    fn spent(&self) -> bool {
        self.0.get() > MAX_WORK
    }
}

/// The error for a feed that would take more than the budget to read.
const TOO_MUCH: &str = "the feed takes too much work to read";

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Event {
    /// Timed: UTC seconds. All-day: the first day as days_from_civil * 86400
    /// (a date, no zone).
    pub start: i64,
    /// Exclusive. Timed: UTC seconds. All-day: the day after the last, * 86400.
    pub end: i64,
    pub all_day: bool,
    pub title: String,
    pub location: String,
    /// The URL property if it is http(s) (anything else is dropped: feeds
    /// are untrusted).
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    /// The calendar's own name (X-WR-CALNAME), if it gives one.
    pub name: Option<String>,
    pub events: Vec<Event>,
    /// Series left out because their EXDATE or RDATE list was longer than
    /// MAX_DATES (shown partly, they could bring back cancelled events).
    pub dropped: usize,
}

/// Every occurrence that overlaps [from, to) (UTC seconds), sorted by start
/// then title. All-day ones are kept if their days overlap
/// [from - 86400, to + 86400) (the caller picks local days). Err for text
/// that is not an iCalendar at all, or one cut off before its end.
/// (Text in tests; the fetch reads bytes: parse_bytes.)
#[cfg(test)]
pub fn parse(text: &str, from: i64, to: i64, zones: &dyn Zones) -> Result<Parsed, String> {
    parse_bytes(text.as_bytes(), from, to, zones)
}

/// parse, for the bytes as fetched: lines are unfolded before the text is
/// read as UTF-8, so a character split across a fold stays whole.
pub fn parse_bytes(bytes: &[u8], from: i64, to: i64, zones: &dyn Zones) -> Result<Parsed, String> {
    if bytes.len() > MAX_INPUT {
        return Err(format!("a feed larger than {} MB", MAX_INPUT >> 20));
    }
    let budget = Budget::new();
    let roots = tree(&unfold(bytes, &budget)?, &budget)?;
    let cals: Vec<&Comp> = roots.iter().filter(|c| c.name == "VCALENDAR").collect();
    if cals.is_empty() {
        return Err("not an iCalendar feed (no BEGIN:VCALENDAR)".into());
    }
    let zt = ZoneTable::default();
    let mut name = None;
    let mut vtz: HashMap<usize, VTz> = HashMap::new();
    let mut events: Vec<&Comp> = Vec::new();
    for cal in &cals {
        if name.is_none() {
            name = cal.prop("X-WR-CALNAME").map(|p| text_value(&p.value)).filter(|n| !n.is_empty());
        }
        for k in &cal.kids {
            match k.name.as_str() {
                "VTIMEZONE" => {
                    if let Some((id, z)) = VTz::read(k, &zt, &budget)? {
                        vtz.insert(id, z);
                    }
                }
                "VEVENT" => {
                    if events.len() >= MAX_VEVENTS {
                        return Err(format!("a feed with more than {MAX_VEVENTS} events"));
                    }
                    events.push(k);
                }
                _ => {}
            }
        }
    }
    let mut evs: Vec<Ev> = Vec::with_capacity(events.len());
    for c in &events {
        if let Some(e) = Ev::read(c, &zt, &budget) {
            evs.push(e);
        }
        if budget.spent() {
            return Err(TOO_MUCH.into());
        }
    }
    if zt.over.get() {
        return Err(format!("a feed with more than {MAX_ZONES} time zones"));
    }
    let names = zt.names.into_inner();
    let conv = Conv { zones, vtz: &vtz, names: &names, budget: &budget };
    // A series with an exception list cut short is left out whole: shown
    // partly, it could bring back events that were cancelled.
    let cut: HashSet<String> = evs.iter().filter(|e| e.cut && !e.uid.is_empty()).map(|e| e.uid.clone()).collect();
    let dropped = cut.len() + evs.iter().filter(|e| e.cut && e.uid.is_empty()).count();
    evs.retain(|e| !e.cut && !cut.contains(&e.uid));
    // Overrides of a repeat (RECURRENCE-ID), by UID: the occurrences they
    // replace or cancel, and those that also move every later one
    // (RANGE=THISANDFUTURE), in order.
    let mut moved: HashMap<&str, (HashSet<i64>, HashSet<i64>)> = HashMap::new();
    let mut lists: HashMap<&str, Vec<Future>> = HashMap::new();
    for e in evs.iter().filter(|e| e.recur_id.is_some() && !e.uid.is_empty()) {
        if !budget.spend(1) {
            return Err(TOO_MUCH.into());
        }
        let rid = e.recur_id.as_ref().unwrap_or(&e.start);
        let slot = moved.entry(e.uid.as_str()).or_default();
        let at = if rid.frame == Frame::Date {
            slot.1.insert(rid.wall.div_euclid(DAY));
            rid.wall
        } else {
            let at = conv.utc(rid);
            slot.0.insert(at);
            at
        };
        if e.range_future {
            let start = e.start_value(&conv);
            lists.entry(e.uid.as_str()).or_default().push(Future { at, delta: start - at, len: e.length(&conv, start), ev: e });
        }
        if budget.spent() {
            return Err(TOO_MUCH.into());
        }
    }
    // Each series' THISANDFUTURE overrides: sorted once, their widest shift
    // and longest length worked out once.
    let mut futures: HashMap<&str, Futures> = HashMap::with_capacity(lists.len());
    for (uid, mut list) in lists {
        if !budget.spend(1 + list.len() as u64) {
            return Err(TOO_MUCH.into());
        }
        list.sort_by_key(|f| f.at);
        let shift = list.iter().map(|f| f.delta.abs()).max().unwrap_or(0).min(3660 * DAY);
        let longest = list.iter().map(|f| f.len).max().unwrap_or(0).min(3660 * DAY);
        futures.insert(uid, Futures { list, shift, longest });
    }
    // Of several masters with one UID, the first is the series.
    let mut masters: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for e in &evs {
        let master = e.recur_id.is_none();
        if master && !e.uid.is_empty() && !masters.insert(e.uid.as_str()) {
            continue;
        }
        if e.cancelled {
            continue;
        }
        let skip = if master { moved.get(e.uid.as_str()) } else { None };
        let later = if master { futures.get(e.uid.as_str()) } else { None };
        e.occurrences(&conv, from, to, skip, later, &mut out);
        if out.len() >= MAX_EVENTS {
            out.truncate(MAX_EVENTS);
            break;
        }
        if !conv.spend(0) {
            // Out of budget: what was found so far.
            break;
        }
    }
    out.sort_by(|a, b| (a.start, &a.title).cmp(&(b.start, &b.title)));
    Ok(Parsed { name, events: out, dropped })
}

// ---- content lines -----------------------------------------------------------------

/// A line's property is one this reader uses (KEPT): its name, up to the
/// first ';' or ':', in any case.
fn kept(line: &[u8]) -> bool {
    let end = line.iter().position(|&b| b == b';' || b == b':').unwrap_or(line.len());
    let name = line[..end].trim_ascii();
    KEPT.iter().any(|k| k.as_bytes().eq_ignore_ascii_case(name))
}

/// The logical lines this reader uses: a line starting with a space or a
/// tab continues the one before (CRLF or LF files, a BOM ignored). Unfolded
/// as bytes, then read as UTF-8 (anything invalid becomes U+FFFD): a fold
/// may fall in the middle of a character (RFC 5545 3.1). A property not in
/// KEPT is skipped with its continuations as they stream past, never
/// stored; whether it is kept is decided only once its whole name is in
/// (the first ';' or ':', which may come after a fold: "EXDA" + " TE:…"),
/// the line held until then like a kept one. Err when a held logical line
/// grows past MAX_LINE (checked as it grows) or the budget runs out (each
/// physical line takes a step, and one more per KiB).
fn unfold(bytes: &[u8], budget: &Budget) -> Result<Vec<String>, String> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    let mut lines: Vec<String> = Vec::new();
    // The logical line being put together, whether its name is complete
    // (kept() decided), and whether the one being read now is skipped.
    let mut cur: Option<Vec<u8>> = None;
    let mut named = false;
    let mut skipping = false;
    let too_long = || format!("a line longer than {} KB", MAX_LINE >> 10);
    let has_end = |b: &[u8]| b.iter().any(|&c| c == b';' || c == b':');
    // A held line: keep or drop it once its name is complete.
    let finish = |cur: Option<Vec<u8>>, lines: &mut Vec<String>| {
        if let Some(l) = cur.filter(|l| kept(l)) {
            lines.push(String::from_utf8_lossy(&l).into_owned());
        }
    };
    for raw in bytes.split(|&b| b == b'\n') {
        if !budget.spend(1 + (raw.len() >> 10) as u64) {
            return Err(TOO_MUCH.into());
        }
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if let Some((&(b' ' | b'\t'), rest)) = line.split_first() {
            if let (Some(c), false) = (cur.as_mut(), skipping) {
                if c.len() + rest.len() > MAX_LINE {
                    return Err(too_long());
                }
                c.extend_from_slice(rest);
                if !named && has_end(rest) {
                    // The name is complete now: a property not read is
                    // dropped here, its other continuations skipped.
                    named = true;
                    if !cur.as_deref().is_some_and(kept) {
                        cur = None;
                        skipping = true;
                    }
                }
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }
        finish(cur.take(), &mut lines);
        named = has_end(line);
        skipping = named && !kept(line);
        if skipping {
            continue;
        }
        if line.len() > MAX_LINE {
            return Err(too_long());
        }
        cur = Some(line.to_vec());
    }
    finish(cur, &mut lines);
    Ok(lines)
}

#[derive(Clone, Debug, PartialEq)]
struct Prop {
    /// Upper case.
    name: String,
    /// (NAME upper case, value without its quotes).
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// Splits `s` at `sep` outside double quotes.
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut quoted, mut begin) = (false, 0);
    for (i, c) in s.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == sep && !quoted {
            out.push(&s[begin..i]);
            begin = i + c.len_utf8();
        }
    }
    out.push(&s[begin..]);
    out
}

/// "NAME;PARAM=value;P2="a:b":value" -> a property (None: not one). Err
/// past the caps: more than MAX_PARAMS parameters, a value longer than
/// MAX_VALUE, a TZID (parameter or property) longer than MAX_TZID.
fn content_line(line: &str) -> Result<Option<Prop>, String> {
    let mut quoted = false;
    let Some(colon) = line.char_indices().find(|&(_, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        c == ':' && !quoted
    }) else {
        return Ok(None);
    };
    let (head, value) = (&line[..colon.0], &line[colon.0 + 1..]);
    if value.len() > MAX_VALUE {
        return Err(format!("a value longer than {} KB", MAX_VALUE >> 10));
    }
    let parts = split_unquoted(head, ';');
    if parts.len() > MAX_PARAMS + 1 {
        return Err(format!("a line with more than {MAX_PARAMS} parameters"));
    }
    let mut parts = parts.into_iter();
    let Some(name) = parts.next().map(|n| n.trim().to_ascii_uppercase()).filter(|n| !n.is_empty()) else { return Ok(None) };
    let params: Vec<(String, String)> = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
            Some((k.trim().to_ascii_uppercase(), v.to_string()))
        })
        .collect();
    let tzid_param = params.iter().any(|(k, v)| k == "TZID" && v.len() > MAX_TZID);
    if tzid_param || (name == "TZID" && value.trim().len() > MAX_TZID) {
        return Err(format!("a time zone name longer than {MAX_TZID} bytes"));
    }
    Ok(Some(Prop { name, params, value: value.to_string() }))
}

/// A component (VCALENDAR, VEVENT, VTIMEZONE, STANDARD...).
#[derive(Debug, Default)]
struct Comp {
    name: String,
    props: Vec<Prop>,
    kids: Vec<Comp>,
}

impl Comp {
    fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }
    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Prop> + 'a {
        self.props.iter().filter(move |p| p.name == name)
    }
}

/// The components, nested by BEGIN / END. An END without its BEGIN is
/// ignored, and an END closes whatever is still open inside it. Past
/// MAX_DEPTH a component is skipped and only counted: inside skipped ones
/// every END closes one level, whatever it names (so each line costs the
/// same, and no tree is ever deep or torn down deeply). Each line takes a
/// step of the budget (and one more per KiB). Err when the text ends with a
/// component still open (a feed cut off), past MAX_COMPS components or the
/// line caps (content_line), or when the budget runs out.
fn tree(lines: &[String], budget: &Budget) -> Result<Vec<Comp>, String> {
    fn close(stack: &mut Vec<Comp>, roots: &mut Vec<Comp>) {
        if let Some(c) = stack.pop() {
            match stack.last_mut() {
                Some(parent) => parent.kids.push(c),
                None => roots.push(c),
            }
        }
    }
    let (mut stack, mut roots): (Vec<Comp>, Vec<Comp>) = (Vec::new(), Vec::new());
    // Skipped components open now (only counted).
    let mut skipped = 0usize;
    let mut begun = 0usize;
    for l in lines {
        if !budget.spend(1 + (l.len() >> 10) as u64) {
            return Err(TOO_MUCH.into());
        }
        let Some(p) = content_line(l)? else { continue };
        match p.name.as_str() {
            "BEGIN" => {
                begun += 1;
                if begun > MAX_COMPS {
                    return Err(format!("a feed with more than {MAX_COMPS} components"));
                }
                if skipped > 0 || stack.len() >= MAX_DEPTH {
                    skipped += 1;
                } else {
                    stack.push(Comp { name: p.value.trim().to_ascii_uppercase(), ..Default::default() });
                }
            }
            "END" => {
                if skipped > 0 {
                    skipped -= 1;
                } else {
                    // At most MAX_DEPTH to look through.
                    let n = p.value.trim().to_ascii_uppercase();
                    if let Some(at) = stack.iter().rposition(|c| c.name == n) {
                        while stack.len() > at {
                            close(&mut stack, &mut roots);
                        }
                    }
                }
            }
            _ => {
                if let (Some(top), 0) = (stack.last_mut(), skipped) {
                    top.props.push(p);
                }
            }
        }
    }
    if !stack.is_empty() || skipped > 0 {
        return Err("the feed ends before its calendar does (cut off)".into());
    }
    Ok(roots)
}

/// A TEXT value unescaped (\n \N \, \; \\).
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// A TEXT value for one line of the agenda: unescaped, line breaks and
/// runs of spaces made one space, cut to MAX_TEXT.
fn text_value(v: &str) -> String {
    unescape(v).split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TEXT).collect()
}

/// An http(s) link, else "" (no file:, javascript: or app protocols).
fn web_url(v: &str) -> String {
    let v = v.trim();
    let lower = v.get(..8).unwrap_or(v).to_ascii_lowercase();
    let web = lower.starts_with("https://") || lower.starts_with("http://");
    if web && v.len() <= MAX_TEXT && !v.chars().any(|c| c.is_whitespace() || c.is_control()) {
        v.to_string()
    } else {
        String::new()
    }
}

// ---- times -----------------------------------------------------------------------

/// What a time value is relative to.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Frame {
    /// A date (all-day).
    Date,
    Utc,
    /// No zone: the PC's.
    Floating,
    /// A TZID: its index in the feed's ZoneTable (the name is kept once).
    Zone(usize),
}

/// The feed's TZIDs, each kept once (time values hold an index). More than
/// MAX_ZONES different ones set `over` (the feed is then an error).
#[derive(Default)]
struct ZoneTable {
    names: RefCell<Vec<String>>,
    over: Cell<bool>,
}

impl ZoneTable {
    /// The index of zone `z` (added if new); None past MAX_ZONES.
    fn intern(&self, z: &str) -> Option<usize> {
        let mut n = self.names.borrow_mut();
        if let Some(i) = n.iter().position(|x| x == z) {
            return Some(i);
        }
        if n.len() >= MAX_ZONES {
            self.over.set(true);
            return None;
        }
        n.push(z.to_string());
        Some(n.len() - 1)
    }
}

/// A time value: seconds since 1970 as read in its frame (a date at
/// 00:00; UTC values are the instant).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stamp {
    frame: Frame,
    wall: i64,
}

fn digits(s: &str) -> Option<u32> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// "20261105" (a date), "20261105T093000" or "20261105T093000Z" ->
/// (is a date, is UTC, wall seconds).
fn stamp_value(v: &str) -> Option<(bool, bool, i64)> {
    let v = v.trim();
    if !v.is_ascii() || v.len() < 8 {
        return None;
    }
    let (y, m, d) = (digits(&v[..4])? as i32, digits(&v[4..6])?, digits(&v[6..8])?);
    if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    let day = days_from_civil(y, m, d) * DAY;
    let rest = &v[8..];
    if rest.is_empty() {
        return Some((true, false, day));
    }
    let rest = rest.strip_prefix(['T', 't'])?;
    let (hms, utc) = match rest.strip_suffix(['Z', 'z']) {
        Some(h) => (h, true),
        None => (rest, false),
    };
    if hms.len() != 6 {
        return None;
    }
    let (h, mi, s) = (digits(&hms[..2])?, digits(&hms[2..4])?, digits(&hms[4..6])?);
    if h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some((false, utc, day + h as i64 * 3600 + mi as i64 * 60 + s.min(59) as i64))
}

/// A TZID parameter as a key: quotes and spaces off.
fn zone_name(z: &str) -> String {
    z.trim().trim_matches('"').trim().to_string()
}

/// The time values of a property (EXDATE and RDATE may list several; a
/// PERIOD keeps its start), read as they are taken. Its TZID is looked up
/// once (zt); each value, valid or not, takes a step of the budget first.
fn stamps<'a>(p: &'a Prop, zt: &ZoneTable, budget: &'a Budget) -> impl Iterator<Item = Stamp> + 'a {
    let zone = p.param("TZID").map(zone_name).filter(|z| !z.is_empty()).and_then(|z| zt.intern(&z));
    p.value.split(',').filter_map(move |v| {
        if !budget.spend(1) {
            return None;
        }
        let (date, utc, wall) = stamp_value(v.split('/').next().unwrap_or(v))?;
        let frame = if date {
            Frame::Date
        } else if utc {
            Frame::Utc
        } else if let Some(z) = zone {
            Frame::Zone(z)
        } else {
            Frame::Floating
        };
        Some(Stamp { frame, wall })
    })
}

/// "P1D", "PT1H30M", "-P1W" -> seconds.
fn duration(v: &str) -> Option<i64> {
    let v = v.trim();
    let (neg, v) = match v.as_bytes().first()? {
        b'-' => (true, &v[1..]),
        b'+' => (false, &v[1..]),
        _ => (false, v),
    };
    let v = v.strip_prefix(['P', 'p'])?;
    let (mut total, mut num, mut time, mut any) = (0i64, String::new(), false, false);
    for c in v.chars() {
        match c.to_ascii_uppercase() {
            '0'..='9' if num.len() < 9 => num.push(c),
            'T' if num.is_empty() => time = true,
            u @ ('W' | 'D' | 'H' | 'M' | 'S') => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                any = true;
                let unit = match (u, time) {
                    ('W', false) => 7 * DAY,
                    ('D', false) => DAY,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                total = total.saturating_add(n.saturating_mul(unit));
            }
            _ => return None,
        }
    }
    (any && num.is_empty()).then_some(if neg { -total } else { total })
}

/// "+0900", "-0430", "+053000" -> seconds east of UTC.
fn utc_offset(v: &str) -> Option<i64> {
    let v = v.trim();
    let (sign, v) = match v.as_bytes().first()? {
        b'-' => (-1, &v[1..]),
        b'+' => (1, &v[1..]),
        _ => return None,
    };
    if !v.is_ascii() || (v.len() != 4 && v.len() != 6) {
        return None;
    }
    let (h, m) = (digits(&v[..2])? as i64, digits(&v[2..4])? as i64);
    let s = if v.len() == 6 { digits(&v[4..6])? as i64 } else { 0 };
    (h < 24 && m < 60 && s < 60).then_some(sign * (h * 3600 + m * 60 + s))
}

// ---- repeats -----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Clone, Debug, PartialEq)]
struct Rule {
    freq: Freq,
    interval: i64,
    count: Option<u32>,
    until: Option<Stamp>,
    /// (ordinal, weekday 0 = Sunday); ordinal 0 = every one.
    byday: Vec<(i32, u32)>,
    bymonthday: Vec<i32>,
    bymonth: Vec<u32>,
    /// The week's first day (0 = Sunday); Monday unless said.
    wkst: u32,
}

fn weekday_code(s: &str) -> Option<u32> {
    ["SU", "MO", "TU", "WE", "TH", "FR", "SA"].iter().position(|d| d.eq_ignore_ascii_case(s)).map(|i| i as u32)
}

/// An RRULE value. None: invalid, or using parts this reader does not
/// expand (then only the first occurrence is shown). Its lists lose their
/// duplicates as they are read (each value takes a step of the budget):
/// BYDAY keeps at most MAX_BYDAY entries (more: None), BYMONTHDAY and
/// BYMONTH can then hold at most 62 and 12.
fn rule(v: &str, budget: &Budget) -> Option<Rule> {
    let mut seen_day: HashSet<(i32, u32)> = HashSet::new();
    let mut seen_md = 0u64;
    let mut r = Rule { freq: Freq::Daily, interval: 1, count: None, until: None, byday: Vec::new(), bymonthday: Vec::new(), bymonth: Vec::new(), wkst: 1 };
    let mut freq = None;
    for part in v.trim().split(';') {
        if !budget.spend(1) {
            return None;
        }
        if part.trim().is_empty() {
            continue;
        }
        let (k, val) = part.split_once('=')?;
        let val = val.trim();
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = Some(match val.to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    "MONTHLY" => Freq::Monthly,
                    "YEARLY" => Freq::Yearly,
                    _ => return None,
                })
            }
            "INTERVAL" => r.interval = digits(val)?.clamp(1, 1000) as i64,
            "COUNT" => r.count = Some(digits(val)?),
            "UNTIL" => {
                let (date, utc, wall) = stamp_value(val)?;
                r.until = Some(Stamp { frame: if date { Frame::Date } else if utc { Frame::Utc } else { Frame::Floating }, wall });
            }
            "BYDAY" => {
                for d in val.split(',') {
                    if !budget.spend(1) {
                        return None;
                    }
                    let d = d.trim();
                    if d.len() < 2 || !d.is_ascii() {
                        return None;
                    }
                    let (ord, code) = d.split_at(d.len() - 2);
                    let ord: i32 = if ord.is_empty() || ord == "+" { 0 } else { ord.trim_start_matches('+').parse().ok()? };
                    if ord.abs() > 53 {
                        return None;
                    }
                    let e = (ord, weekday_code(code)?);
                    if seen_day.insert(e) {
                        if r.byday.len() >= MAX_BYDAY {
                            return None;
                        }
                        r.byday.push(e);
                    }
                }
            }
            "BYMONTHDAY" => {
                for d in val.split(',') {
                    if !budget.spend(1) {
                        return None;
                    }
                    let d: i32 = d.trim().parse().ok()?;
                    if d == 0 || d.abs() > 31 {
                        return None;
                    }
                    // At most 62 values can be here.
                    let bit = 1u64 << (d + 31);
                    if seen_md & bit == 0 {
                        seen_md |= bit;
                        r.bymonthday.push(d);
                    }
                }
            }
            "BYMONTH" => {
                for m in val.split(',') {
                    if !budget.spend(1) {
                        return None;
                    }
                    let m = digits(m.trim())?;
                    if !(1..=12).contains(&m) {
                        return None;
                    }
                    if !r.bymonth.contains(&m) {
                        r.bymonth.push(m);
                    }
                }
            }
            "WKST" => r.wkst = weekday_code(val)?,
            // BYSETPOS, BYHOUR, BYMINUTE, BYSECOND, BYWEEKNO, BYYEARDAY, X-...
            _ => return None,
        }
    }
    r.freq = freq?;
    let ordinals = r.byday.iter().any(|&(o, _)| o != 0);
    match r.freq {
        // Ordinals in weeks or days mean nothing; BYMONTHDAY in weeks is
        // not allowed.
        Freq::Weekly if ordinals || !r.bymonthday.is_empty() => None,
        Freq::Daily if ordinals => None,
        // "The 20th Monday of the year": not expanded ("every Monday" is).
        Freq::Yearly if ordinals && r.bymonth.is_empty() => None,
        _ => Some(r),
    }
}

/// The days of month `m` of year `y` a rule picks (sorted): its
/// BYMONTHDAY (negative from the end) and BYDAY (with ordinals: "2TU", the
/// second Tuesday; "-1FR", the last Friday), both when it has both; else
/// day `default` (none if the month is too short for it).
fn month_days(y: i32, m: u32, byday: &[(i32, u32)], bymonthday: &[i32], default: u32) -> Vec<u32> {
    let n = days_in_month(y, m);
    // Days as bits (1 << day): sets with no duplicates, any input.
    let mut md = 0u64;
    for &d in bymonthday {
        let d = if d > 0 { d } else { n as i32 + 1 + d };
        if d >= 1 && d as u32 <= n {
            md |= 1 << d;
        }
    }
    let picked = if byday.is_empty() {
        if bymonthday.is_empty() {
            if default <= n { 1u64 << default } else { 0 }
        } else {
            md
        }
    } else {
        let first = weekday(days_from_civil(y, m, 1));
        let mut picked = 0u64;
        for &(ord, wd) in byday {
            let first_day = 1 + (wd + 7 - first) % 7;
            let count = (n - first_day) / 7 + 1;
            let pick = |k: u32| 1u64 << (first_day + 7 * k);
            match ord {
                0 => (0..count).for_each(|k| picked |= pick(k)),
                o if o > 0 && (o as u32) <= count => picked |= pick(o as u32 - 1),
                o if o < 0 && o.unsigned_abs() <= count => picked |= pick(count - o.unsigned_abs()),
                _ => {}
            }
        }
        if bymonthday.is_empty() { picked } else { picked & md }
    };
    (1..=n).filter(|d| picked & (1 << d) != 0).collect()
}

/// What walls() asks of the event's zone.
struct Walk<'a> {
    /// The wall time is after UNTIL.
    past_until: &'a dyn Fn(i64) -> bool,
    /// The wall time happens (not in a gap the clocks jump over): one that
    /// does not is skipped and does not count (RFC 5545 3.3.10).
    real: &'a dyn Fn(i64) -> bool,
    /// Takes `n` steps of the feed's budget; false once it is spent.
    spend: &'a dyn Fn(u64) -> bool,
}

/// Pure: the period (step) a rule without COUNT can start walking from to
/// reach wall day `keep_day`: one before the period it falls in (nothing
/// before it can show, and nothing needs counting).
fn first_step(r: &Rule, sday: i64, keep_day: i64) -> i64 {
    if r.count.is_some() || keep_day <= sday {
        return 0;
    }
    let (sy, sm, _) = civil_from_days(sday);
    let (ky, km, _) = civil_from_days(keep_day);
    let periods = match r.freq {
        Freq::Daily => keep_day - sday,
        Freq::Weekly => (keep_day - sday) / 7,
        Freq::Monthly => (ky as i64 * 12 + km as i64) - (sy as i64 * 12 + sm as i64),
        Freq::Yearly => (ky - sy) as i64,
    };
    (periods / r.interval - 1).max(0)
}

/// The wall times of a rule's occurrences from `start` (which always
/// counts as the first) up to `stop`, honouring COUNT and UNTIL. Only
/// those at or after `keep` are returned (the earlier ones still count).
fn walls(r: &Rule, start: i64, keep: i64, stop: i64, walk: &Walk) -> Vec<i64> {
    let sday = start.div_euclid(DAY);
    let tod = start.rem_euclid(DAY);
    let (sy, sm, sd) = civil_from_days(sday);
    let mut out = Vec::new();
    let mut n = 0u32;
    let mut take = |w: i64, out: &mut Vec<i64>| -> bool {
        if (walk.past_until)(w) {
            return false;
        }
        n += 1;
        if w >= keep {
            out.push(w);
        }
        r.count.is_none_or(|c| n < c)
    };
    if !take(start, &mut out) || start > stop {
        return out;
    }
    let in_months = |m: u32| r.bymonth.is_empty() || r.bymonth.contains(&m);
    // A yearly rule picking days (BYMONTHDAY, BYDAY) without BYMONTH picks
    // them in every month; a plain one keeps DTSTART's month.
    let year_months: Vec<u32> = if !r.bymonth.is_empty() {
        r.bymonth.clone()
    } else if r.bymonthday.is_empty() && r.byday.is_empty() {
        vec![sm]
    } else {
        (1..=12).collect()
    };
    // The weekdays BYDAY names, as bits.
    let wd_mask: u8 = r.byday.iter().fold(0, |m, &(_, wd)| m | 1 << wd);
    // A period costs its lists' work.
    let cost = 1 + (r.byday.len() + r.bymonthday.len()) as u64 * if r.freq == Freq::Yearly { 12 } else { 1 };
    let s0 = first_step(r, sday, keep.div_euclid(DAY));
    for step in s0..s0 + MAX_STEPS {
        if !(walk.spend)(cost) {
            break;
        }
        let k = step * r.interval;
        let (first, days): (i64, Vec<i64>) = match r.freq {
            Freq::Daily => {
                let d = sday + k;
                let (_, m, dd) = civil_from_days(d);
                let ok = in_months(m)
                    && (r.bymonthday.is_empty() || month_days(civil_from_days(d).0, m, &[], &r.bymonthday, 0).contains(&dd))
                    && (r.byday.is_empty() || wd_mask & (1 << weekday(d)) != 0);
                (d, if ok { vec![d] } else { Vec::new() })
            }
            Freq::Weekly => {
                let week = sday - ((weekday(sday) + 7 - r.wkst) % 7) as i64 + k * 7;
                let days = (week..week + 7)
                    .filter(|&d| if r.byday.is_empty() { weekday(d) == weekday(sday) } else { wd_mask & (1 << weekday(d)) != 0 })
                    .filter(|&d| in_months(civil_from_days(d).1))
                    .collect();
                (week, days)
            }
            Freq::Monthly => {
                let mi = (sy as i64 * 12 + sm as i64 - 1) + k;
                let (y, m) = (mi.div_euclid(12).clamp(-200_000, 200_000) as i32, mi.rem_euclid(12) as u32 + 1);
                let days = if in_months(m) { month_days(y, m, &r.byday, &r.bymonthday, sd).into_iter().map(|d| days_from_civil(y, m, d)).collect() } else { Vec::new() };
                (days_from_civil(y, m, 1), days)
            }
            Freq::Yearly => {
                let y = (sy as i64 + k).clamp(-200_000, 200_000) as i32;
                let mut days: Vec<i64> = year_months.iter().flat_map(|&m| month_days(y, m, &r.byday, &r.bymonthday, sd).into_iter().map(move |d| days_from_civil(y, m, d))).collect();
                days.sort_unstable();
                (days_from_civil(y, 1, 1), days)
            }
        };
        if first * DAY > stop {
            break;
        }
        for d in days {
            let w = d * DAY + tod;
            if w <= start {
                continue;
            }
            if w > stop {
                return out;
            }
            if !(walk.real)(w) {
                continue;
            }
            if !take(w, &mut out) {
                return out;
            }
        }
    }
    out
}

// ---- time zones from the feed ----------------------------------------------------

/// One STANDARD or DAYLIGHT block of a VTIMEZONE.
#[derive(Clone, Debug)]
struct Observance {
    from: i64,
    to: i64,
    /// Its first onset, wall time in the `from` offset.
    start: i64,
    rule: Option<Rule>,
    rdates: Vec<i64>,
}

/// An offset change: (UTC instant, offset after, offset before).
type Change = (i64, i64, i64);

/// A feed's own zone (for a TZID the system does not know). Its onsets
/// and RDATEs are sorted once; its rules' changes are worked out only for
/// the years a time is asked in (each year once, kept: MAX_ZONE_YEARS of
/// them, never cleared), the whole work paid before it is done; a lookup
/// is a binary search.
#[derive(Clone, Debug)]
struct VTz {
    obs: Vec<Observance>,
    /// Every onset and RDATE, sorted.
    fixed: Vec<Change>,
    /// Every offset it uses, sorted, once each.
    offs: Vec<i64>,
    /// What working out one year's rule changes costs: every block's rule,
    /// over 3 years, every month it names, every BYDAY / BYMONTHDAY entry.
    gen: u64,
    /// The rules' changes for years y - 1 ..= y + 1, by y.
    years: RefCell<HashMap<i32, std::rc::Rc<Vec<Change>>>>,
}

impl VTz {
    /// Ok(None): not a zone this reader can use. Err: past MAX_OBS blocks
    /// or MAX_OBS_DATES RDATEs in one, or out of budget (its times would be
    /// wrong). Each block and date takes a step of the budget.
    fn read(c: &Comp, zt: &ZoneTable, budget: &Budget) -> Result<Option<(usize, VTz)>, String> {
        let Some(id) = c.prop("TZID").map(|p| zone_name(&p.value)) else { return Ok(None) };
        let mut obs: Vec<Observance> = Vec::new();
        for k in c.kids.iter().filter(|k| k.name == "STANDARD" || k.name == "DAYLIGHT") {
            if obs.len() >= MAX_OBS {
                return Err(format!("a time zone with more than {MAX_OBS} parts"));
            }
            if !budget.spend(1) {
                return Err(TOO_MUCH.into());
            }
            let Some(to) = k.prop("TZOFFSETTO").and_then(|p| utc_offset(&p.value)) else { continue };
            let from = k.prop("TZOFFSETFROM").and_then(|p| utc_offset(&p.value)).unwrap_or(to);
            let start = k.prop("DTSTART").and_then(|p| stamps(p, zt, budget).next().map(|s| s.wall)).unwrap_or(0);
            let rule = k.prop("RRULE").and_then(|p| rule(&p.value, budget)).filter(|r| r.freq == Freq::Yearly);
            let mut rdates = Vec::new();
            for s in k.all("RDATE").flat_map(|p| stamps(p, zt, budget)) {
                if rdates.len() >= MAX_OBS_DATES {
                    return Err(format!("a time zone with more than {MAX_OBS_DATES} dates"));
                }
                if !budget.spend(1) {
                    return Err(TOO_MUCH.into());
                }
                rdates.push(s.wall);
            }
            obs.push(Observance { from, to, start, rule, rdates });
        }
        if budget.spent() {
            return Err(TOO_MUCH.into());
        }
        if id.is_empty() || obs.is_empty() {
            return Ok(None);
        }
        let Some(id) = zt.intern(&id) else { return Ok(None) };
        let gen = obs
            .iter()
            .filter_map(|o| o.rule.as_ref())
            .map(|r| 3 * r.bymonth.len().max(1) as u64 * (1 + (r.byday.len() + r.bymonthday.len()) as u64))
            .sum::<u64>()
            + 1;
        let mut fixed: Vec<Change> = Vec::new();
        for o in &obs {
            fixed.push((o.start - o.from, o.to, o.from));
            fixed.extend(o.rdates.iter().map(|&w| (w - o.from, o.to, o.from)));
        }
        fixed.sort_unstable();
        let mut offs: Vec<i64> = obs.iter().flat_map(|o| [o.from, o.to]).collect();
        offs.sort_unstable();
        offs.dedup();
        Ok(Some((id, VTz { obs, fixed, offs, gen, years: Default::default() })))
    }

    /// The rules' changes around year `y`: worked out once and kept (past
    /// MAX_ZONE_YEARS years, worked out again each time), its whole cost
    /// (`gen`) taken from `budget` before. None once the budget is spent.
    fn year(&self, y: i32, budget: &Budget) -> Option<std::rc::Rc<Vec<Change>>> {
        if let Some(t) = self.years.borrow().get(&y) {
            return Some(t.clone());
        }
        if !budget.spend(self.gen) {
            return None;
        }
        let t = std::rc::Rc::new(self.rule_changes(y));
        let mut g = self.years.borrow_mut();
        if g.len() < MAX_ZONE_YEARS {
            g.insert(y, t.clone());
        }
        Some(t)
    }

    /// The offset in force at UTC instant `utc`, given the rules' changes
    /// `t` for its year: the last change at or before it, else the offset
    /// before the first change.
    fn offset_at(&self, t: &[Change], utc: i64) -> i64 {
        let last = |v: &[Change]| {
            let i = v.partition_point(|x| x.0 <= utc);
            (i > 0).then(|| v[i - 1])
        };
        match (last(&self.fixed), last(t)) {
            (Some(a), Some(b)) => {
                if a.0 >= b.0 {
                    a.1
                } else {
                    b.1
                }
            }
            (Some(a), None) | (None, Some(a)) => a.1,
            (None, None) => {
                let first = match (self.fixed.first(), t.first()) {
                    (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
                    (a, b) => a.or(b),
                };
                first.map(|x| x.2).unwrap_or_else(|| self.obs.first().map_or(0, |o| o.to))
            }
        }
    }

    /// The rules' (not the onsets' or RDATEs') changes around year `y`.
    fn rule_changes(&self, y: i32) -> Vec<Change> {
        let mut t = Vec::new();
        for o in &self.obs {
            let Some(r) = &o.rule else { continue };
            let (sy, sm, sd) = civil_from_days(o.start.div_euclid(DAY));
            let tod = o.start.rem_euclid(DAY);
            for year in (y - 1).max(sy)..=y + 1 {
                let months = if r.bymonth.is_empty() { vec![sm] } else { r.bymonth.clone() };
                for m in months {
                    let Some(&d) = month_days(year, m, &r.byday, &r.bymonthday, sd).first() else { continue };
                    let w = days_from_civil(year, m, d) * DAY + tod;
                    let utc = w - o.from;
                    let ended = r.until.as_ref().is_some_and(|u| if u.frame == Frame::Utc { utc > u.wall } else { w > u.wall + if u.frame == Frame::Date { DAY } else { 0 } });
                    if w > o.start && !ended {
                        t.push((utc, o.to, o.from));
                    }
                }
            }
        }
        t.sort_unstable();
        t
    }

    /// The UTC instant of wall time `wall` in this zone (None once the
    /// budget is spent).
    fn to_utc(&self, wall: i64, budget: &Budget) -> Option<i64> {
        if !budget.spend(self.offs.len() as u64) {
            return None;
        }
        let t = self.year(civil_from_days(wall.div_euclid(DAY)).0, budget)?;
        let at = |utc: i64| self.offset_at(&t, utc);
        let fits = self.offs.iter().filter(|&&o| at(wall - o) == o).map(|o| wall - o).min();
        // Twice: the first. Never (a gap): the offset from before it.
        match fits {
            Some(u) => Some(u),
            None => Some(wall - at(self.offs.iter().map(|o| wall - o).min()?)),
        }
    }

    /// Wall time `wall` happens in this zone (it is not in a gap). True
    /// once the budget is spent (nothing more is worked out).
    fn happens(&self, wall: i64, budget: &Budget) -> bool {
        if !budget.spend(self.offs.len() as u64) {
            return true;
        }
        let Some(t) = self.year(civil_from_days(wall.div_euclid(DAY)).0, budget) else { return true };
        self.offs.iter().any(|&o| self.offset_at(&t, wall - o) == o)
    }
}

/// Turns time values into UTC instants, each taking a step of the feed's
/// budget (a feed zone's lookups take more). Once the budget is spent each
/// call does nothing more (its answer no longer matters: the caller stops).
struct Conv<'a> {
    zones: &'a dyn Zones,
    vtz: &'a HashMap<usize, VTz>,
    /// The ZoneTable's names, by index.
    names: &'a [String],
    budget: &'a Budget,
}

impl Conv<'_> {
    fn utc(&self, s: &Stamp) -> i64 {
        if !self.budget.spend(1) {
            return s.wall;
        }
        match s.frame {
            Frame::Date | Frame::Utc => s.wall,
            Frame::Floating => self.zones.local_to_utc(s.wall),
            Frame::Zone(i) => {
                let z = self.names.get(i).map_or("", |n| n.as_str());
                self.zones.to_utc(z, s.wall).or_else(|| self.vtz.get(&i).and_then(|v| v.to_utc(s.wall, self.budget))).unwrap_or_else(|| self.zones.local_to_utc(s.wall))
            }
        }
    }

    /// The wall time of `s` happens: not in the hour the clocks skip.
    /// When the zone cannot tell, it does.
    fn real(&self, s: &Stamp) -> bool {
        if !self.budget.spend(1) {
            return true;
        }
        let local = |w: i64| self.zones.utc_to_local(self.zones.local_to_utc(w)).is_none_or(|b| b == w);
        match s.frame {
            Frame::Date | Frame::Utc => true,
            Frame::Floating => local(s.wall),
            Frame::Zone(i) => match self.zones.to_utc(self.names.get(i).map_or("", |n| n.as_str()), s.wall) {
                Some(u) => self.zones.utc_to_zone(self.names.get(i).map_or("", |n| n.as_str()), u).is_none_or(|b| b == s.wall),
                None => match self.vtz.get(&i) {
                    Some(v) => v.happens(s.wall, self.budget),
                    None => local(s.wall),
                },
            },
        }
    }

    /// Takes `n` steps of the budget; false once it is spent.
    fn spend(&self, n: u64) -> bool {
        self.budget.spend(n)
    }
}

/// A RECURRENCE-ID;RANGE=THISANDFUTURE override: every occurrence of its
/// series after `at` moves by `delta`, takes its length and its texts (or
/// is dropped, if it is cancelled).
struct Future<'a> {
    at: i64,
    delta: i64,
    len: i64,
    ev: &'a Ev,
}

/// A series' THISANDFUTURE overrides, sorted by `at` (the one that applies
/// to an occurrence is found by binary search), with their widest shift
/// and longest length (each at most ten years) worked out once.
struct Futures<'a> {
    list: Vec<Future<'a>>,
    shift: i64,
    longest: i64,
}

// ---- events -------------------------------------------------------------------------

/// A VEVENT as read.
#[derive(Debug)]
struct Ev {
    uid: String,
    start: Stamp,
    end: Option<Stamp>,
    duration: Option<i64>,
    rule: Option<Rule>,
    /// It has an RRULE this reader does not expand.
    odd_rule: bool,
    exdates: Vec<Stamp>,
    rdates: Vec<Stamp>,
    /// Its EXDATE or RDATE list was longer than MAX_DATES: the series is
    /// left out (Parsed::dropped).
    cut: bool,
    recur_id: Option<Stamp>,
    /// Its RECURRENCE-ID says RANGE=THISANDFUTURE.
    range_future: bool,
    cancelled: bool,
    title: String,
    location: String,
    url: String,
}

impl Ev {
    /// Each EXDATE / RDATE value takes a step of the budget; past
    /// MAX_DATES of either the rest is not read and `cut` is set.
    fn read(c: &Comp, zt: &ZoneTable, budget: &Budget) -> Option<Ev> {
        if !budget.spend(1 + c.props.len() as u64) {
            return None;
        }
        let start = c.prop("DTSTART").and_then(|p| stamps(p, zt, budget).next())?;
        let rrule = c.prop("RRULE").map(|p| rule(&p.value, budget));
        let rid = c.prop("RECURRENCE-ID");
        let dates = |name: &str| -> (Vec<Stamp>, bool) {
            let mut v = Vec::new();
            for s in c.all(name).flat_map(|p| stamps(p, zt, budget)) {
                if v.len() >= MAX_DATES {
                    return (v, true);
                }
                v.push(s);
            }
            (v, false)
        };
        let (exdates, ex_cut) = dates("EXDATE");
        let (rdates, r_cut) = dates("RDATE");
        Some(Ev {
            uid: c.prop("UID").map(|p| p.value.trim().to_string()).unwrap_or_default(),
            end: c.prop("DTEND").and_then(|p| stamps(p, zt, budget).next()),
            duration: c.prop("DURATION").and_then(|p| duration(&p.value)),
            odd_rule: matches!(rrule, Some(None)),
            rule: rrule.flatten(),
            exdates,
            rdates,
            cut: ex_cut || r_cut,
            recur_id: rid.and_then(|p| stamps(p, zt, budget).next()),
            range_future: rid.and_then(|p| p.param("RANGE")).is_some_and(|r| r.trim().eq_ignore_ascii_case("THISANDFUTURE")),
            cancelled: c.prop("STATUS").is_some_and(|p| p.value.trim().eq_ignore_ascii_case("CANCELLED")),
            title: c.prop("SUMMARY").map(|p| text_value(&p.value)).unwrap_or_default(),
            location: c.prop("LOCATION").map(|p| text_value(&p.value)).unwrap_or_default(),
            url: c.prop("URL").map(|p| web_url(&p.value)).unwrap_or_default(),
            start,
        })
    }

    fn all_day(&self) -> bool {
        self.start.frame == Frame::Date
    }

    /// Its start as the events give it: a date as its day, else UTC.
    fn start_value(&self, conv: &Conv) -> i64 {
        if self.all_day() { self.start.wall } else { conv.utc(&self.start) }
    }

    /// Its length: from DTEND, else DURATION, else a day (dates) or
    /// nothing. `start` is start_value.
    fn length(&self, conv: &Conv, start: i64) -> i64 {
        if self.all_day() {
            let days = match (&self.end, self.duration) {
                (Some(e), _) => (e.wall.div_euclid(DAY) - self.start.wall.div_euclid(DAY)).max(1),
                (None, Some(d)) => ((d + DAY - 1) / DAY).max(1),
                _ => 1,
            };
            days.min(3660) * DAY
        } else {
            match (&self.end, self.duration) {
                (Some(e), _) => (conv.utc(e) - start).clamp(0, 3660 * DAY),
                (None, Some(d)) => d.clamp(0, 3660 * DAY),
                _ => 0,
            }
        }
    }

    /// Pushes onto `out` the occurrences that overlap [from, to), less the
    /// ones in EXDATE and in `moved` (overridden: by instant, or by day
    /// for a date RECURRENCE-ID); those after a THISANDFUTURE override in
    /// `later` take its shift, length and texts.
    fn occurrences(&self, conv: &Conv, from: i64, to: i64, moved: Option<&(HashSet<i64>, HashSet<i64>)>, later: Option<&Futures>, out: &mut Vec<Event>) {
        let all_day = self.all_day();
        let start_utc = conv.utc(&self.start);
        let len = self.length(conv, start_utc);
        // Dates and wall times are within a day of UTC instants; a later
        // override may move an occurrence in from outside, or make it last
        // long enough to reach into the window from far before it.
        let list: &[Future] = later.map_or(&[], |f| f.list.as_slice());
        let shift = later.map_or(0, |f| f.shift);
        let longest = later.map_or(0, |f| f.longest).max(len).min(3660 * DAY);
        let keep = from - 2 * DAY - longest - shift;
        let stop = to + 2 * DAY + shift;
        let mut ex_at: HashSet<i64> = HashSet::new();
        let mut ex_day: HashSet<i64> = HashSet::new();
        if !conv.spend(self.exdates.len() as u64 + self.rdates.len() as u64 + 1) {
            return;
        }
        for x in &self.exdates {
            if x.frame == Frame::Date || all_day {
                ex_day.insert(x.wall.div_euclid(DAY));
            } else {
                ex_at.insert(conv.utc(x));
            }
        }
        // Candidate starts: (value, the wall day).
        let mut starts: Vec<(i64, i64)> = Vec::new();
        let frame = self.start.frame;
        let value = |w: i64| if all_day { w } else { conv.utc(&Stamp { frame, wall: w }) };
        match &self.rule {
            Some(r) if !self.odd_rule && self.recur_id.is_none() => {
                let off = self.start.wall - start_utc;
                let past_until = |w: i64| -> bool {
                    let Some(u) = &r.until else { return false };
                    match u.frame {
                        Frame::Date => w >= u.wall + DAY,
                        Frame::Utc if all_day => w > u.wall,
                        Frame::Utc => {
                            let rough = w - off;
                            if rough < u.wall - 2 * DAY {
                                false
                            } else if rough > u.wall + 2 * DAY {
                                true
                            } else {
                                value(w) > u.wall
                            }
                        }
                        _ => w > u.wall,
                    }
                };
                let real = |w: i64| all_day || conv.real(&Stamp { frame, wall: w });
                let spend = |n: u64| conv.spend(n);
                let walk = Walk { past_until: &past_until, real: &real, spend: &spend };
                starts.extend(walls(r, self.start.wall, keep, stop, &walk).into_iter().map(|w| (value(w), w.div_euclid(DAY))));
            }
            _ => starts.push((if all_day { self.start.wall } else { start_utc }, self.start.wall.div_euclid(DAY))),
        }
        let mut seen: HashSet<i64> = starts.iter().map(|s| s.0).collect();
        for x in &self.rdates {
            let v = if all_day { x.wall } else { conv.utc(x) };
            if seen.insert(v) {
                starts.push((v, x.wall.div_euclid(DAY)));
            }
        }
        if !conv.spend(starts.len() as u64) {
            return;
        }
        for (s, day) in starts {
            let gone = if all_day {
                ex_day.contains(&day) || moved.is_some_and(|m| m.1.contains(&day))
            } else {
                ex_at.contains(&s) || ex_day.contains(&day) || moved.is_some_and(|m| m.0.contains(&s) || m.1.contains(&day))
            };
            if gone {
                continue;
            }
            // The last THISANDFUTURE override before it, if any.
            let (s, len, src) = match list.partition_point(|f| f.at < s).checked_sub(1).map(|i| &list[i]) {
                Some(f) if f.ev.cancelled => continue,
                Some(f) => (s + f.delta, f.len, f.ev),
                None => (s, len, self),
            };
            let e = s + len;
            let overlaps = if all_day { s < to + DAY && e > from - DAY } else { s < to && (e > from || (e == s && s >= from)) };
            if overlaps {
                out.push(Event { start: s, end: e, all_day, title: src.title.clone(), location: src.location.clone(), url: src.url.clone() });
                if out.len() >= MAX_EVENTS {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tz::SystemZones;

    /// Fixed offsets (seconds east of UTC) by zone name; the PC is `local`.
    struct Fixed {
        zones: Vec<(&'static str, i64)>,
        local: i64,
    }

    impl Zones for Fixed {
        fn to_utc(&self, tzid: &str, local: i64) -> Option<i64> {
            self.zones.iter().find(|(z, _)| *z == tzid).map(|(_, o)| local - o)
        }
        fn local_to_utc(&self, local: i64) -> i64 {
            local - self.local
        }
    }

    fn fixed() -> Fixed {
        Fixed { zones: vec![("Asia/Seoul", 9 * 3600), ("Europe/London", 0)], local: 9 * 3600 }
    }

    fn at(y: i32, m: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, m, d) * DAY + h * 3600 + mi * 60
    }

    fn day(y: i32, m: u32, d: u32) -> i64 {
        days_from_civil(y, m, d) * DAY
    }

    fn cal(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{}\r\nEND:VCALENDAR\r\n", body.trim().replace('\n', "\r\n"))
    }

    fn ev(props: &str) -> String {
        cal(&format!("BEGIN:VEVENT\n{}\nEND:VEVENT", props.trim()))
    }

    /// Starts (UTC) of the events in [from, to).
    fn starts(text: &str, from: i64, to: i64, z: &dyn Zones) -> Vec<i64> {
        parse(text, from, to, z).unwrap().events.iter().map(|e| e.start).collect()
    }

    #[test]
    fn property_names_folded_in_the_middle_still_count() {
        // A daily 09:00 Z event, its start, rule and exception each with
        // its name cut by a fold.
        let text = cal("BEGIN:VEVENT\nUID:f1\nDTSTA\n RT:20261105T090000Z\nDTEND:20261105T100000Z\nRRU\n LE:FREQ=DAILY;COUNT=5\nEXDA\n TE:20261107T090000Z\nSUMMARY:Daily\nEND:VEVENT");
        let got = starts(&text, day(2026, 11, 1), day(2026, 11, 30), &fixed());
        assert_eq!(got, [at(2026, 11, 5, 9, 0), at(2026, 11, 6, 9, 0), at(2026, 11, 8, 9, 0), at(2026, 11, 9, 9, 0)], "the 7th excluded, five in all");
        // Folded before its ':' too, and in its parameters.
        let text = cal("BEGIN:VEVENT\nUID:f2\nDTSTART;TZ\n ID=Asia/Seoul:20261105T090000\nSUMMARY:Seoul\nEND:VEVENT");
        assert_eq!(starts(&text, day(2026, 11, 1), day(2026, 11, 30), &fixed()), [at(2026, 11, 5, 0, 0)]);
        // A property not read, folded in its name, is still skipped whole.
        let text = cal("BEGIN:VEVENT\nUID:f3\nDTSTART:20261105T090000Z\nDESCRI\n PTION:long\n  text\nSUMMARY:Kept\nEND:VEVENT");
        let p = parse(&text, day(2026, 11, 1), day(2026, 11, 30), &fixed()).unwrap();
        assert_eq!(p.events[0].title, "Kept");
    }

    #[test]
    fn lines_unfold_and_unescape() {
        let text = "\u{feff}BEGIN:VCALENDAR\nX-WR-CALNAME:Work\\, home\nBEGIN:VEVENT\nUID:1\nDTSTART:20261105T010000Z\nsummary;LANGUAGE=en:Stand\n  up\\nmeeting\nLOCATION;ALTREP=\"http://x/a;b:c\":Room 4\\; east \\\\ wing\nURL;VALUE=URI:https://example.com/e?id=1\nEND:VEVENT\nEND:VCALENDAR\n";
        let p = parse(text, at(2026, 11, 5, 0, 0), at(2026, 11, 6, 0, 0), &fixed()).unwrap();
        assert_eq!(p.name.as_deref(), Some("Work, home"));
        let e = &p.events[0];
        assert_eq!(e.title, "Stand up meeting", "folded, the escaped line break made a space");
        assert_eq!(e.location, "Room 4; east \\ wing");
        assert_eq!(e.url, "https://example.com/e?id=1");
        assert_eq!((e.start, e.end, e.all_day), (at(2026, 11, 5, 1, 0), at(2026, 11, 5, 1, 0), false));
        assert_eq!(unescape("a\\nb\\Nc\\,d\\;e\\\\f"), "a\nb\nc,d;e\\f");
        let l = content_line("DTSTART;TZID=\"Korea Standard Time\";X=\"a;b:c\":20261105T090000").unwrap().unwrap();
        assert_eq!(l.param("TZID"), Some("Korea Standard Time"));
        assert_eq!(l.param("X"), Some("a;b:c"));
        assert_eq!(l.value, "20261105T090000");
    }

    #[test]
    fn only_web_links_are_kept() {
        assert_eq!(web_url(" HTTPS://a.b/c "), "HTTPS://a.b/c");
        assert_eq!(web_url("http://a.b"), "http://a.b");
        for bad in ["file:///c:/x.exe", "javascript:alert(1)", "ms-settings:", "https://a b", "ftp://x"] {
            assert_eq!(web_url(bad), "", "{bad}");
        }
    }

    #[test]
    fn not_a_calendar_is_an_error() {
        assert!(parse("<html>404</html>", 0, 1, &fixed()).is_err());
        assert!(parse("", 0, 1, &fixed()).is_err());
        assert_eq!(parse("BEGIN:VCALENDAR\nEND:VCALENDAR", 0, 1, &fixed()).unwrap().events, vec![]);
        // Cut off: the calendar, or an event in it, never ends.
        assert!(parse("BEGIN:VCALENDAR\nVERSION:2.0\n", 0, 1, &fixed()).is_err());
        assert!(parse("BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20261102T000000Z\nEND:VEVENT\n", 0, 1, &fixed()).is_err());
        assert!(parse("BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20261102T000000Z\nEND:VCALENDAR\n", 0, 1, &fixed()).is_ok(), "an END closes what is open inside it");
    }

    #[test]
    fn a_character_split_by_a_fold_stays_whole() {
        // The first character is ED 95 9C: folded between its second and third byte.
        let mut b = b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART:20261105T010000Z\r\nSUMMARY:\xED\x95\r\n \x9C\xEA\xB8\x80\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n".to_vec();
        let p = parse_bytes(&b, at(2026, 11, 5, 0, 0), at(2026, 11, 6, 0, 0), &fixed()).unwrap();
        assert_eq!(p.events[0].title, "\u{d55c}\u{ae00}");
        // Invalid bytes are replaced, never refused.
        b.splice(0..0, *b"\xFF\r\n");
        assert!(parse_bytes(&b, at(2026, 11, 5, 0, 0), at(2026, 11, 6, 0, 0), &fixed()).is_ok());
    }

    #[test]
    fn deep_and_huge_feeds_stay_bounded() {
        let z = fixed();
        let ev = "BEGIN:VEVENT\nDTSTART:20261102T000000Z\nSUMMARY:kept\nEND:VEVENT\n";
        // Nested components up to the cap, closed again: skipped past
        // MAX_DEPTH, the events around them read, nothing deep built.
        let n = MAX_COMPS - 3;
        let deep = format!("BEGIN:VCALENDAR\n{ev}{}{}BEGIN:VEVENT\nDTSTART:20261102T010000Z\nEND:VEVENT\nEND:VCALENDAR\n", "BEGIN:X\nSUMMARY:x\n".repeat(n), "END:X\n".repeat(n));
        let p = parse(&deep, day(2026, 11, 1), day(2026, 11, 3), &z).unwrap();
        assert_eq!(p.events.iter().map(|e| e.start).collect::<Vec<_>>(), [at(2026, 11, 2, 0, 0), at(2026, 11, 2, 1, 0)]);
        // 200k of them: past MAX_COMPS, refused at once.
        let t0 = std::time::Instant::now();
        let big = 200_000;
        let deep = format!("BEGIN:VCALENDAR\n{}{}END:VCALENDAR\n", "BEGIN:X\n".repeat(big), "END:X\n".repeat(big));
        assert!(parse(&deep, 0, 1, &z).is_err());
        // Never closed: cut off.
        let open = format!("BEGIN:VCALENDAR\n{}", "BEGIN:X\n".repeat(n));
        assert!(parse(&open, 0, 1, &z).is_err());
        // MAX_DATES + 1 RDATEs, one a line: the series is left out (counted).
        let lines: String = (0..=MAX_DATES).map(|i| format!("RDATE:{}T000000Z\n", 19000101 + i % 28)).collect();
        let many = format!("BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20261102T000000Z\n{lines}END:VEVENT\nEND:VCALENDAR\n");
        let p = parse(&many, day(2026, 11, 1), day(2026, 11, 3), &z).unwrap();
        assert_eq!((p.events.len(), p.dropped), (0, 1));
        assert!(t0.elapsed().as_secs() < 10, "{:?}", t0.elapsed());
        // Thousands of old daily repeats: walked from near the window, so
        // every one still shows.
        let reps: String = (0..2_000).map(|i| format!("BEGIN:VEVENT\nUID:{i}\nDTSTART:19900101T{:02}0000Z\nRRULE:FREQ=DAILY\nEND:VEVENT\n", i % 24)).collect();
        let p = parse(&format!("BEGIN:VCALENDAR\n{reps}END:VCALENDAR\n"), day(2026, 11, 1), day(2026, 11, 2), &z).unwrap();
        assert_eq!(p.events.len(), 2_000);
        // The budget: a feed that would take more work stops early.
        let budget = Budget::new();
        let conv = Conv { zones: &z, vtz: &HashMap::new(), names: &[], budget: &budget };
        assert!(conv.spend(MAX_WORK) && !conv.spend(1));
    }

    #[test]
    fn unmatched_ends_inside_skipped_components_cost_one_step_each() {
        let z = fixed();
        let ev = "BEGIN:VEVENT\nDTSTART:20261102T000000Z\nEND:VEVENT\n";
        let t0 = std::time::Instant::now();
        let text = |n: usize| format!("BEGIN:VCALENDAR\n{ev}{}{}{}{ev}END:VCALENDAR\n", "BEGIN:X\n".repeat(n), "END:Y\n".repeat(n), "END:X\n".repeat(n));
        let p = parse(&text(MAX_COMPS - 3), day(2026, 11, 1), day(2026, 11, 3), &z).unwrap();
        assert_eq!(p.events.len(), 2, "the events around them still read");
        // 200k BEGIN:X, 200k unmatched END:Y, 200k END:X: refused at the cap.
        assert!(parse(&text(200_000), day(2026, 11, 1), day(2026, 11, 3), &z).is_err());
        assert!(t0.elapsed().as_secs() < 10, "{:?}", t0.elapsed());
        // A feed with more lines than the budget is refused, not half read.
        let budget = Budget::new();
        let lines: Vec<String> = vec!["X:1".to_string(); 1000];
        assert!(budget.spend(MAX_WORK - 10));
        assert!(tree(&lines, &budget).is_err());
    }

    #[test]
    fn repeated_rule_lists_lose_their_duplicates() {
        let z = fixed();
        let t0 = std::time::Instant::now();
        let byday = vec!["MO"; 100_000].join(",");
        let bymd = vec!["1"; 100_000].join(",");
        // In a feed the value cap holds a rule to 4 KB: as many repeats as fit.
        let (fd, fm) = (vec!["MO"; 700].join(","), vec!["1"; 900].join(","));
        // Mondays that are the 1st: 2027-02-01 after the start (which counts).
        let text = ev(&format!("DTSTART:20261102T080000Z\nRRULE:FREQ=MONTHLY;BYDAY={fd};BYMONTHDAY={fm}"));
        assert_eq!(starts(&text, day(2026, 11, 1), day(2027, 3, 1), &z), [at(2026, 11, 2, 8, 0), at(2027, 2, 1, 8, 0)]);
        // Past it: refused.
        assert!(parse(&ev(&format!("DTSTART:20261102T080000Z\nRRULE:FREQ=MONTHLY;BYDAY={byday}")), 0, 1, &z).is_err());
        assert!(t0.elapsed().as_secs() < 10, "{:?}", t0.elapsed());
        let r = rule(&format!("FREQ=MONTHLY;BYDAY={byday};BYMONTHDAY={bymd};BYMONTH=1,1,1"), &Budget::new()).unwrap();
        assert_eq!((r.byday.len(), r.bymonthday.len(), r.bymonth.len()), (1, 1, 1));
        // More than MAX_BYDAY different entries: not expanded.
        let many: Vec<String> = (1..=54).map(|o| format!("{}MO", o.min(53) * if o > 53 { -1 } else { 1 })).collect();
        assert!(rule(&format!("FREQ=MONTHLY;BYDAY={}", many.join(",")), &Budget::new()).is_none());
        assert_eq!(month_days(2026, 11, &[(0, 1), (2, 1), (-1, 1)], &[], 1), [2, 9, 16, 23, 30]);
    }

    #[test]
    fn a_feed_zone_bomb_is_refused_and_a_big_zone_is_read_once() {
        let z = Fixed { zones: vec![], local: 0 };
        // RDATEs 100 a line (the value cap).
        let zone = |dates: usize| {
            let rd: Vec<String> = (0..dates).map(|i| format!("{}T020000", 19000101 + (i % 28))).collect();
            let lines: String = rd.chunks(100).map(|c| format!("RDATE:{}\n", c.join(","))).collect();
            format!("BEGIN:VTIMEZONE\nTZID:Bomb\nBEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0100\n{lines}END:STANDARD\nEND:VTIMEZONE")
        };
        let events: String = (0..10_000).map(|i| format!("BEGIN:VEVENT\nUID:{i}\nDTSTART;TZID=Bomb:20261102T{:02}0000\nEND:VEVENT\n", i % 24)).collect();
        let t0 = std::time::Instant::now();
        // 100k dates in one block: past MAX_OBS_DATES, an error (its times would be wrong).
        assert!(parse(&cal(&format!("{}\n{events}", zone(100_000))), day(2026, 11, 1), day(2026, 11, 3), &z).is_err());
        // At the cap, with 10k events using it: read once, fast, right.
        let p = parse(&cal(&format!("{}\n{events}", zone(MAX_OBS_DATES))), day(2026, 11, 1), day(2026, 11, 3), &z).unwrap();
        assert_eq!(p.events.len(), 10_000);
        assert!(p.events.iter().all(|e| e.start.rem_euclid(3600) == 0 && civil_from_days((e.start + 3600).div_euclid(DAY)) == (2026, 11, 2)), "+01:00");
        assert!(t0.elapsed().as_secs() < 20, "{:?}", t0.elapsed());
        // Too many blocks: an error too.
        let blocks = "BEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0100\nEND:STANDARD\n".repeat(MAX_OBS + 1);
        assert!(parse(&cal(&format!("BEGIN:VTIMEZONE\nTZID:Many\n{blocks}END:VTIMEZONE")), 0, 1, &z).is_err());
    }

    #[test]
    fn a_series_with_too_many_exceptions_is_left_out() {
        let z = fixed();
        // One EXDATE a line (the value cap).
        let ex = |n: usize| {
            let first = days_from_civil(1900, 1, 1);
            (0..n as i64).map(|i| civil_from_days(first + i)).map(|(y, m, d)| format!("EXDATE:{y:04}{m:02}{d:02}T080000Z")).collect::<Vec<_>>().join("\n")
        };
        let series = |n: usize| cal(&format!("BEGIN:VEVENT\nUID:s\nDTSTART:19000101T080000Z\nRRULE:FREQ=DAILY\n{}\nEND:VEVENT\nBEGIN:VEVENT\nUID:s\nRECURRENCE-ID:20261102T080000Z\nDTSTART:20261102T090000Z\nEND:VEVENT\nBEGIN:VEVENT\nUID:other\nDTSTART:20261102T100000Z\nEND:VEVENT", ex(n)));
        let (from, to) = (day(2026, 11, 2), day(2026, 11, 3));
        // At the cap: shown (the day's 08:00 moved to 09:00, and the other one).
        let p = parse(&series(MAX_DATES), from, to, &z).unwrap();
        assert_eq!(p.events.iter().map(|e| e.start).collect::<Vec<_>>(), [at(2026, 11, 2, 9, 0), at(2026, 11, 2, 10, 0)]);
        assert_eq!(p.dropped, 0);
        // One more: the whole series (and its override) left out, counted.
        let p = parse(&series(MAX_DATES + 1), from, to, &z).unwrap();
        assert_eq!(p.events.iter().map(|e| e.start).collect::<Vec<_>>(), [at(2026, 11, 2, 10, 0)]);
        assert_eq!(p.dropped, 1);
    }

    #[test]
    fn every_hard_cap_rejects_quickly() {
        let z = fixed();
        let t0 = std::time::Instant::now();
        let fails = |text: &str| parse(text, day(2026, 11, 1), day(2026, 11, 3), &z).is_err();
        // Input bytes.
        let mut huge = b"BEGIN:VCALENDAR\r\n".to_vec();
        huge.resize(MAX_INPUT + 1, b'x');
        assert!(parse_bytes(&huge, 0, 1, &z).is_err());
        // A kept logical line past MAX_LINE, grown by folds.
        let folded = format!("SUMMARY:a\n{}", " bcdefghij\n".repeat(MAX_LINE / 9 + 1));
        assert!(fails(&ev(&format!("DTSTART:20261102T000000Z\n{}", folded.trim_end()))));
        // A long DESCRIPTION (never shown) is skipped, whatever its length.
        let desc = format!("DESCRIPTION:a\n{}", " bcdefghij\n".repeat(5_000));
        let p = parse(&ev(&format!("DTSTART:20261102T000000Z\n{}SUMMARY:ok", desc)), day(2026, 11, 1), day(2026, 11, 3), &z).unwrap();
        assert_eq!(p.events[0].title, "ok");
        // Parameters on a line.
        let params = (0..=MAX_PARAMS).map(|i| format!(";X-{i}=a")).collect::<String>();
        assert!(fails(&ev(&format!("DTSTART{params}:20261102T000000Z"))));
        let ok = (0..MAX_PARAMS).map(|i| format!(";X-{i}=a")).collect::<String>();
        assert!(!fails(&ev(&format!("DTSTART{ok}:20261102T000000Z"))));
        // A value.
        assert!(fails(&ev(&format!("DTSTART:20261102T000000Z\nSUMMARY:{}", "x".repeat(MAX_VALUE + 1)))));
        // A TZID, as a parameter and as a zone's name.
        let long = "Z".repeat(MAX_TZID + 1);
        assert!(fails(&ev(&format!("DTSTART;TZID={long}:20261102T000000"))));
        assert!(fails(&cal(&format!("BEGIN:VTIMEZONE\nTZID:{long}\nEND:VTIMEZONE"))));
        // Components and events.
        assert!(fails(&cal(&"BEGIN:X\nEND:X\n".repeat(MAX_COMPS + 1))));
        let evs = |n: usize| (0..n).map(|i| format!("BEGIN:VEVENT\nUID:{i}\nDTSTART:20261102T000000Z\nEND:VEVENT\n")).collect::<String>();
        assert!(fails(&cal(&evs(MAX_VEVENTS + 1))));
        assert!(!fails(&cal(&evs(MAX_VEVENTS))));
        // Blocks of one zone.
        let blocks = "BEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0100\nEND:STANDARD\n".repeat(MAX_OBS + 1);
        assert!(fails(&cal(&format!("BEGIN:VTIMEZONE\nTZID:Many\n{blocks}END:VTIMEZONE"))));
        // Different TZIDs.
        let zones = |n: usize| (0..n).map(|i| format!("BEGIN:VEVENT\nUID:{i}\nDTSTART;TZID=Zone/{i}:20261102T000000\nEND:VEVENT\n")).collect::<String>();
        assert!(fails(&cal(&zones(MAX_ZONES + 1))));
        assert!(!fails(&cal(&zones(MAX_ZONES))));
        assert!(t0.elapsed().as_secs() < 20, "{:?}", t0.elapsed());
    }

    #[test]
    fn a_zone_name_is_kept_once_whatever_the_dates() {
        // 5,000 EXDATEs naming one zone: one name, every value an index.
        let z = fixed();
        let lines: String = (0..MAX_DATES).map(|i| format!("EXDATE;TZID=Asia/Seoul:{}T090000\n", 19000101 + i % 28)).collect();
        let text = ev(&format!("UID:a\nDTSTART;TZID=Asia/Seoul:20261102T090000\nRRULE:FREQ=DAILY;COUNT=3\n{lines}"));
        let budget = Budget::new();
        let zt = ZoneTable::default();
        let roots = tree(&unfold(text.as_bytes(), &budget).unwrap(), &budget).unwrap();
        let e = Ev::read(&roots[0].kids[0], &zt, &budget).unwrap();
        assert_eq!(zt.names.borrow().len(), 1);
        assert!(e.exdates.iter().all(|s| s.frame == Frame::Zone(0)));
        assert_eq!(starts(&text, day(2026, 11, 1), day(2026, 11, 6), &z).len(), 3);
    }

    #[test]
    fn many_overrides_and_masters_stay_cheap() {
        // 4,000 masters of one UID (the first wins) and 4,000 THISANDFUTURE
        // overrides of it far in the future: each occurrence finds its
        // override by binary search.
        let z = fixed();
        let t0 = std::time::Instant::now();
        let masters: String = (0..4_000).map(|i| format!("BEGIN:VEVENT\nUID:m\nDTSTART:2026110{}T{:02}0000Z\nRRULE:FREQ=DAILY\nEND:VEVENT\n", 1 + i % 2, i % 24)).collect();
        let overrides: String = (0..4_000).map(|i| format!("BEGIN:VEVENT\nUID:m\nRECURRENCE-ID;RANGE=THISANDFUTURE:2099{:02}{:02}T000000Z\nDTSTART:2099{:02}{:02}T010000Z\nSTATUS:CANCELLED\nEND:VEVENT\n", 1 + i % 12, 1 + i % 28, 1 + i % 12, 1 + i % 28)).collect();
        let p = parse(&cal(&format!("{masters}{overrides}")), day(2026, 11, 1), day(2026, 11, 4), &z).unwrap();
        // Only the first master: daily at 00:00 from Nov 1.
        assert_eq!(p.events.iter().map(|e| e.start).collect::<Vec<_>>(), [at(2026, 11, 1, 0, 0), at(2026, 11, 2, 0, 0), at(2026, 11, 3, 0, 0)]);
        assert!(t0.elapsed().as_secs() < 10, "{:?}", t0.elapsed());
    }

    #[test]
    fn a_costly_feed_zone_is_paid_for_before_it_is_worked_out() {
        // 64 blocks whose rules name every month and 53 weekdays that never
        // fall in one, used by overrides across many years: each year's
        // whole cost is taken first, so the feed stops within the budget.
        let z = Fixed { zones: vec![], local: 0 };
        let days: Vec<String> = (1..=53).map(|o| format!("{o}SU")).collect();
        let block = format!("BEGIN:STANDARD\nDTSTART:19000101T020000\nTZOFFSETFROM:+0100\nTZOFFSETTO:+0100\nRRULE:FREQ=YEARLY;BYMONTH=1,2,3,4,5,6,7,8,9,10,11,12;BYDAY={}\nEND:STANDARD\n", days.join(","));
        let zone = format!("BEGIN:VTIMEZONE\nTZID:Costly\n{}END:VTIMEZONE\n", block.repeat(MAX_OBS));
        let evs: String = (0..MAX_VEVENTS - 1).map(|i| format!("BEGIN:VEVENT\nUID:x\nRECURRENCE-ID;TZID=Costly:{}0101T000000\nDTSTART;TZID=Costly:{}0101T000000\nSTATUS:CANCELLED\nEND:VEVENT\n", 2000 + i % 7000, 2000 + i % 7000)).collect();
        let t0 = std::time::Instant::now();
        let r = parse(&cal(&format!("{zone}{evs}")), day(2026, 11, 1), day(2026, 11, 3), &z);
        assert!(r.is_err(), "the zone's years cost more than the budget");
        assert!(t0.elapsed().as_secs() < 20, "{:?}", t0.elapsed());
        // The same zone with one event: worked out once, fine.
        let one = "BEGIN:VEVENT\nDTSTART;TZID=Costly:20261102T090000\nEND:VEVENT\n";
        assert_eq!(parse(&cal(&format!("{zone}{one}")), day(2026, 11, 1), day(2026, 11, 3), &z).unwrap().events.len(), 1);
    }

    #[test]
    fn a_lengthened_series_reaches_into_the_window_from_before_it() {
        let z = fixed();
        // Daily 1 h from Sep 1; from Sep 15 on each one lasts 60 days.
        let text = cal("BEGIN:VEVENT\nUID:long\nDTSTART:20260901T100000Z\nDTEND:20260901T110000Z\nRRULE:FREQ=DAILY\nEND:VEVENT\nBEGIN:VEVENT\nUID:long\nRECURRENCE-ID;RANGE=THISANDFUTURE:20260915T100000Z\nDTSTART:20260915T100000Z\nDTEND:20261114T100000Z\nEND:VEVENT");
        let got = starts(&text, day(2026, 10, 2), day(2026, 10, 3), &z);
        for d in 16..=29 {
            assert!(got.contains(&at(2026, 9, d, 10, 0)), "Sep {d} is still on: {got:?}");
        }
        assert!(got.contains(&at(2026, 9, 15, 10, 0)), "the override itself");
        assert!(!got.contains(&at(2026, 9, 14, 10, 0)), "before it: an hour long");
    }

    #[test]
    fn times_in_every_frame() {
        let z = fixed();
        let (from, to) = (day(2026, 11, 4), day(2026, 11, 8));
        // Zoned, UTC and floating (the PC's zone, +9 here).
        let t = ev("DTSTART;TZID=Asia/Seoul:20261105T093000\nDTEND;TZID=Asia/Seoul:20261105T103000\nSUMMARY:a");
        let e = &parse(&t, from, to, &z).unwrap().events[0];
        assert_eq!((e.start, e.end), (at(2026, 11, 5, 0, 30), at(2026, 11, 5, 1, 30)));
        assert_eq!(starts(&ev("DTSTART:20261105T093000Z"), from, to, &z), [at(2026, 11, 5, 9, 30)]);
        assert_eq!(starts(&ev("DTSTART:20261105T093000"), from, to, &z), [at(2026, 11, 5, 0, 30)]);
        // All-day: a date, no zone; a day long unless said.
        let a = &parse(&ev("DTSTART;VALUE=DATE:20261106\nSUMMARY:b"), from, to, &z).unwrap().events[0];
        assert_eq!((a.start, a.end, a.all_day), (day(2026, 11, 6), day(2026, 11, 7), true));
        let a = &parse(&ev("DTSTART;VALUE=DATE:20261106\nDTEND;VALUE=DATE:20261109"), from, to, &z).unwrap().events[0];
        assert_eq!(a.end, day(2026, 11, 9));
        // DURATION instead of DTEND.
        let d = &parse(&ev("DTSTART:20261105T090000Z\nDURATION:PT1H30M"), from, to, &z).unwrap().events[0];
        assert_eq!(d.end - d.start, 5400);
        assert_eq!(duration("P1W"), Some(7 * DAY));
        assert_eq!(duration("-P1DT2H"), Some(-(DAY + 7200)));
        assert_eq!(duration("+PT15M"), Some(900));
        assert_eq!(duration("P"), None);
        assert_eq!(duration("PT1D"), None);
        assert_eq!(duration("P99999999999999D"), None, "too many digits");
        // An unknown zone without a VTIMEZONE: the PC's.
        assert_eq!(starts(&ev("DTSTART;TZID=Mars/Olympus:20261105T093000"), from, to, &z), [at(2026, 11, 5, 0, 30)]);
        // Out of the window: dropped.
        assert!(starts(&ev("DTSTART:20261201T093000Z"), from, to, &z).is_empty());
        // All-day ones a day beyond the window stay (the caller picks local days).
        assert_eq!(starts(&ev("DTSTART;VALUE=DATE:20261108"), from, to, &z), [day(2026, 11, 8)]);
        assert!(starts(&ev("DTSTART;VALUE=DATE:20261110"), from, to, &z).is_empty());
    }

    #[test]
    fn the_feeds_own_zone_rules_fill_in() {
        // A made-up zone name with US-style rules: second Sunday of March,
        // first Sunday of November, -5 / -4.
        let vtz = "BEGIN:VTIMEZONE\nTZID:Custom East\nBEGIN:STANDARD\nDTSTART:16011104T020000\nRRULE:FREQ=YEARLY;BYDAY=1SU;BYMONTH=11\nTZOFFSETFROM:-0400\nTZOFFSETTO:-0500\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:16010311T020000\nRRULE:FREQ=YEARLY;BYDAY=2SU;BYMONTH=3\nTZOFFSETFROM:-0500\nTZOFFSETTO:-0400\nEND:DAYLIGHT\nEND:VTIMEZONE";
        let text = |dt: &str| cal(&format!("{vtz}\nBEGIN:VEVENT\nDTSTART;TZID=\"Custom East\":{dt}\nEND:VEVENT"));
        let z = fixed();
        let one = |dt: &str, w: i64| starts(&text(dt), w - 3 * DAY, w + 3 * DAY, &z);
        assert_eq!(one("20260115T090000", day(2026, 1, 15)), [at(2026, 1, 15, 14, 0)]);
        assert_eq!(one("20260715T090000", day(2026, 7, 15)), [at(2026, 7, 15, 13, 0)]);
        // The gap and the overlap, as with ICU.
        assert_eq!(one("20260308T023000", day(2026, 3, 8)), [at(2026, 3, 8, 7, 30)]);
        assert_eq!(one("20261101T013000", day(2026, 11, 1)), [at(2026, 11, 1, 5, 30)]);
        // A zone with one offset and no rule.
        let fixed_tz = cal("BEGIN:VTIMEZONE\nTZID:(UTC+09:00) Seoul\nBEGIN:STANDARD\nDTSTART:16010101T000000\nTZOFFSETFROM:+0900\nTZOFFSETTO:+0900\nEND:STANDARD\nEND:VTIMEZONE\nBEGIN:VEVENT\nDTSTART;TZID=\"(UTC+09:00) Seoul\":20261105T090000\nEND:VEVENT");
        assert_eq!(starts(&fixed_tz, day(2026, 11, 4), day(2026, 11, 6), &Fixed { zones: vec![], local: 0 }), [at(2026, 11, 5, 0, 0)]);
        assert_eq!(utc_offset("+053000"), Some(5 * 3600 + 1800));
        assert_eq!(utc_offset("-0430"), Some(-(4 * 3600 + 1800)));
        assert_eq!(utc_offset("0900"), None);
    }

    #[test]
    fn daily_weekly_and_their_limits() {
        let z = fixed();
        let (from, to) = (day(2026, 11, 1), day(2026, 11, 15));
        let r = |rule: &str| starts(&ev(&format!("DTSTART:20261102T080000Z\n{rule}")), from, to, &z);
        assert_eq!(r("RRULE:FREQ=DAILY;COUNT=3"), [at(2026, 11, 2, 8, 0), at(2026, 11, 3, 8, 0), at(2026, 11, 4, 8, 0)]);
        assert_eq!(r("RRULE:FREQ=DAILY;INTERVAL=5").len(), 3, "2, 7, 12");
        assert_eq!(r("RRULE:FREQ=DAILY;UNTIL=20261104T080000Z").len(), 3, "UNTIL is inclusive");
        assert_eq!(r("RRULE:FREQ=DAILY;UNTIL=20261104").len(), 3, "a date UNTIL");
        assert_eq!(r("RRULE:FREQ=DAILY;BYDAY=SA,SU").len(), 1 + 3, "the start, then the 7th, 8th and 14th");
        // 2026-11-02 is a Monday: Mondays and Thursdays.
        assert_eq!(r("RRULE:FREQ=WEEKLY;BYDAY=MO,TH"), [2, 5, 9, 12].map(|d| at(2026, 11, d, 8, 0)));
        assert_eq!(r("RRULE:FREQ=WEEKLY;INTERVAL=2"), [at(2026, 11, 2, 8, 0)]);
        assert_eq!(r("RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE").len(), 2, "2 and 4; the 16th is past the window");
        assert_eq!(r("RRULE:FREQ=WEEKLY;COUNT=2;BYDAY=TU,FR"), [2, 3].map(|d| at(2026, 11, d, 8, 0)), "the start counts first");
        // A rule it cannot expand keeps its first occurrence.
        assert_eq!(r("RRULE:FREQ=HOURLY"), [at(2026, 11, 2, 8, 0)]);
        assert_eq!(r("RRULE:FREQ=MONTHLY;BYDAY=MO;BYSETPOS=1"), [at(2026, 11, 2, 8, 0)]);
        assert_eq!(r("RRULE:garbage"), [at(2026, 11, 2, 8, 0)]);
        // A daily repeat from 1990 still reaches today.
        let old = ev("DTSTART:19900101T080000Z\nRRULE:FREQ=DAILY");
        assert_eq!(starts(&old, from, from + DAY, &z), [at(2026, 11, 1, 8, 0)]);
        // WKST matters for an every-other-week rule (RFC 5545's example).
        let wk = |s: &str| starts(&ev(&format!("DTSTART:19970805T090000Z\nRRULE:FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST={s}")), day(1997, 8, 1), day(1997, 9, 1), &z);
        assert_eq!(wk("MO"), [5, 10, 19, 24].map(|d| at(1997, 8, d, 9, 0)));
        assert_eq!(wk("SU"), [5, 17, 19, 31].map(|d| at(1997, 8, d, 9, 0)));
    }

    #[test]
    fn monthly_and_yearly() {
        let z = fixed();
        let year = |text: &str| starts(text, day(2026, 1, 1), day(2027, 1, 1), &z);
        // The 31st: only in months that have one.
        let m31 = year(&ev("DTSTART:20260131T120000Z\nRRULE:FREQ=MONTHLY"));
        assert_eq!(m31.len(), 7);
        assert!(m31.iter().all(|&s| civil_from_days(s.div_euclid(DAY)).2 == 31));
        // The last day of each month.
        let last = year(&ev("DTSTART:20260131T120000Z\nRRULE:FREQ=MONTHLY;BYMONTHDAY=-1"));
        assert_eq!(last.len(), 12);
        assert_eq!(last[1], at(2026, 2, 28, 12, 0));
        // The second Tuesday and the last Friday.
        let tue = year(&ev("DTSTART:20260113T120000Z\nRRULE:FREQ=MONTHLY;BYDAY=2TU;COUNT=3"));
        assert_eq!(tue, [at(2026, 1, 13, 12, 0), at(2026, 2, 10, 12, 0), at(2026, 3, 10, 12, 0)]);
        let fri = year(&ev("DTSTART:20260130T120000Z\nRRULE:FREQ=MONTHLY;BYDAY=-1FR;UNTIL=20260430T000000Z"));
        assert_eq!(fri, [at(2026, 1, 30, 12, 0), at(2026, 2, 27, 12, 0), at(2026, 3, 27, 12, 0), at(2026, 4, 24, 12, 0)]);
        assert_eq!(month_days(2026, 11, &[(0, 0)], &[], 1), [1, 8, 15, 22, 29], "every Sunday");
        assert_eq!(month_days(2026, 11, &[(5, 1)], &[], 1), [30], "a fifth Monday");
        assert_eq!(month_days(2026, 11, &[(5, 2)], &[], 1), Vec::<u32>::new(), "no fifth Tuesday");
        assert_eq!(month_days(2026, 3, &[(0, 0)], &[8, 9, 10, 11, 12, 13, 14], 1), [8], "old-style second Sunday");
        // Birthdays: same day each year; Feb 29 only in leap years.
        let bday = ev("DTSTART;VALUE=DATE:20000215\nRRULE:FREQ=YEARLY\nSUMMARY:B");
        assert_eq!(year(&bday), [day(2026, 2, 15)]);
        let leap = ev("DTSTART;VALUE=DATE:20000229\nRRULE:FREQ=YEARLY");
        assert!(year(&leap).is_empty());
        assert_eq!(starts(&leap, day(2028, 1, 1), day(2029, 1, 1), &z), [day(2028, 2, 29)]);
        // Yearly in given months, on a weekday of that month.
        let y = year(&ev("DTSTART:20260308T120000Z\nRRULE:FREQ=YEARLY;BYMONTH=3,11;BYDAY=1SU"));
        assert_eq!(y, [at(2026, 3, 8, 12, 0), at(2026, 11, 1, 12, 0)]);
    }

    #[test]
    fn yearly_days_without_months_mean_every_month() {
        let z = fixed();
        let year = |text: &str| starts(text, day(2026, 1, 1), day(2027, 1, 1), &z);
        assert_eq!(year(&ev("DTSTART:20260101T090000Z\nRRULE:FREQ=YEARLY;BYMONTHDAY=1;COUNT=3")), [1, 2, 3].map(|m| at(2026, m, 1, 9, 0)));
        // Every Sunday of the year (no ordinal).
        assert_eq!(year(&ev("DTSTART:20260104T090000Z\nRRULE:FREQ=YEARLY;BYDAY=SU")).len(), 52);
        // "The 20th Monday": still only its first occurrence.
        assert_eq!(year(&ev("DTSTART:20260105T090000Z\nRRULE:FREQ=YEARLY;BYDAY=20MO")), [at(2026, 1, 5, 9, 0)]);
        let b = Budget::new();
        assert_eq!(first_step(&rule("FREQ=DAILY", &b).unwrap(), 0, 100), 99);
        assert_eq!(first_step(&rule("FREQ=DAILY;COUNT=5", &b).unwrap(), 0, 100), 0, "counted from the start");
        assert_eq!(first_step(&rule("FREQ=MONTHLY;INTERVAL=2", &b).unwrap(), 0, 365), 5);
    }

    #[test]
    fn this_and_future_moves_the_rest_of_a_series() {
        let z = fixed();
        let (from, to) = (day(2026, 10, 26), day(2026, 11, 24));
        let base = "BEGIN:VEVENT\nUID:s\nDTSTART:20261029T100000Z\nDTEND:20261029T110000Z\nRRULE:FREQ=WEEKLY;BYDAY=TH\nSUMMARY:Sync\nEND:VEVENT";
        let moved = cal(&format!("{base}\nBEGIN:VEVENT\nUID:s\nRECURRENCE-ID;RANGE=THISANDFUTURE:20261105T100000Z\nDTSTART:20261105T110000Z\nDTEND:20261105T113000Z\nSUMMARY:Sync (later)\nEND:VEVENT"));
        let p = parse(&moved, from, to, &z).unwrap();
        let got: Vec<(i64, i64, &str)> = p.events.iter().map(|e| (e.start, e.end - e.start, e.title.as_str())).collect();
        assert_eq!(
            got,
            [
                (at(2026, 10, 29, 10, 0), 3600, "Sync"),
                (at(2026, 11, 5, 11, 0), 1800, "Sync (later)"),
                (at(2026, 11, 12, 11, 0), 1800, "Sync (later)"),
                (at(2026, 11, 19, 11, 0), 1800, "Sync (later)"),
            ]
        );
        // Cancelled from then on: only the first is left.
        let gone = cal(&format!("{base}\nBEGIN:VEVENT\nUID:s\nRECURRENCE-ID;RANGE=THISANDFUTURE:20261105T100000Z\nDTSTART:20261105T100000Z\nSTATUS:CANCELLED\nEND:VEVENT"));
        assert_eq!(starts(&gone, from, to, &z), [at(2026, 10, 29, 10, 0)]);
    }

    #[test]
    fn exceptions_overrides_and_cancellations() {
        let z = fixed();
        let (from, to) = (day(2026, 11, 1), day(2026, 11, 30));
        let text = cal(
            "BEGIN:VEVENT\nUID:w1\nDTSTART;TZID=Asia/Seoul:20261105T093000\nDTEND;TZID=Asia/Seoul:20261105T100000\nRRULE:FREQ=WEEKLY;BYDAY=TH\nEXDATE;TZID=Asia/Seoul:20261112T093000\nSUMMARY:Weekly\nEND:VEVENT\n\
             BEGIN:VEVENT\nUID:w1\nRECURRENCE-ID;TZID=Asia/Seoul:20261119T093000\nDTSTART;TZID=Asia/Seoul:20261120T140000\nDTEND;TZID=Asia/Seoul:20261120T150000\nSUMMARY:Weekly (moved)\nEND:VEVENT\n\
             BEGIN:VEVENT\nUID:w1\nRECURRENCE-ID;TZID=Asia/Seoul:20261126T093000\nDTSTART;TZID=Asia/Seoul:20261126T093000\nSTATUS:CANCELLED\nEND:VEVENT\n\
             BEGIN:VEVENT\nUID:gone\nDTSTART:20261110T000000Z\nSTATUS:CANCELLED\nEND:VEVENT\n\
             BEGIN:VEVENT\nUID:r\nDTSTART:20261102T000000Z\nRDATE:20261103T000000Z,20261104T000000Z\nSUMMARY:R\nEND:VEVENT",
        );
        let p = parse(&text, from, to, &z).unwrap();
        let got: Vec<(i64, &str)> = p.events.iter().map(|e| (e.start, e.title.as_str())).collect();
        assert_eq!(
            got,
            [
                (at(2026, 11, 2, 0, 0), "R"),
                (at(2026, 11, 3, 0, 0), "R"),
                (at(2026, 11, 4, 0, 0), "R"),
                (at(2026, 11, 5, 0, 30), "Weekly"),
                (at(2026, 11, 20, 5, 0), "Weekly (moved)"),
            ]
        );
        // A date EXDATE on an all-day repeat.
        let d = ev("DTSTART;VALUE=DATE:20261101\nRRULE:FREQ=DAILY;COUNT=3\nEXDATE;VALUE=DATE:20261102");
        assert_eq!(starts(&d, from, to, &z), [day(2026, 11, 1), day(2026, 11, 3)]);
        // A UTC EXDATE on a zoned repeat.
        let x = ev("DTSTART;TZID=Asia/Seoul:20261101T090000\nRRULE:FREQ=DAILY;COUNT=3\nEXDATE:20261102T000000Z");
        assert_eq!(starts(&x, from, to, &z), [at(2026, 11, 1, 0, 0), at(2026, 11, 3, 0, 0)]);
    }

    #[test]
    fn junk_never_panics() {
        let z = fixed();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let pieces = ["BEGIN:VEVENT", "END:VEVENT", "BEGIN:VCALENDAR", "END:VCALENDAR", "DTSTART:2026", "DTSTART;TZID=:20261105T", "RRULE:FREQ=MONTHLY;BYDAY=-99XX", "RRULE:FREQ=YEARLY;BYMONTH=13", "RRULE:FREQ=DAILY;INTERVAL=0;COUNT=99999999999", "DURATION:P", "EXDATE:,,,", "\"\"\";;:", " folded", ":", ";", "\u{1F600}:é", "DTEND:99999999T999999Z", "RRULE:FREQ=WEEKLY;UNTIL=1", "BEGIN:VTIMEZONE", "TZOFFSETTO:+99", "TZID:x", "END:VTIMEZONE", "RECURRENCE-ID:20261105", "STATUS:", "SUMMARY:\\"];
        for _ in 0..400 {
            let mut s = String::new();
            for _ in 0..40 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                s.push_str(pieces[(seed % pieces.len() as u64) as usize]);
                s.push_str(if seed & 64 == 0 { "\r\n" } else { "\n" });
            }
            let _ = parse(&s, day(2026, 11, 1), day(2026, 11, 3), &z);
            let half: String = s.chars().take(s.chars().count() / 2).collect();
            let _ = parse(&half, day(2026, 11, 1), day(2026, 11, 3), &z);
        }
        // A huge line is refused; a long title (under the value cap) cut.
        let huge = ev(&format!("DTSTART:20261102T000000Z\nSUMMARY:{}", "x".repeat(1 << 20)));
        assert!(parse(&huge, day(2026, 11, 1), day(2026, 11, 3), &z).is_err());
        let long = ev(&format!("DTSTART:20261102T000000Z\nSUMMARY:{}", "x".repeat(MAX_VALUE)));
        assert_eq!(parse(&long, day(2026, 11, 1), day(2026, 11, 3), &z).unwrap().events[0].title.chars().count(), MAX_TEXT);
        // A feed cut off is an error (the caller keeps its last good copy).
        let cut = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20261102T000000Z\nSUMMARY:half";
        assert!(parse(cut, day(2026, 11, 1), day(2026, 11, 3), &z).is_err());
        // A runaway rule stays bounded.
        let many = ev("DTSTART:20261101T000000Z\nRRULE:FREQ=DAILY");
        assert_eq!(parse(&many, day(2026, 11, 1), day(2126, 11, 1), &z).unwrap().events.len(), MAX_EVENTS);
    }

    // ---- with the real zones (ICU in Windows) ----

    #[test]
    fn a_new_york_meeting_keeps_its_hour_across_dst() {
        let z = SystemZones::new();
        let t = ev("DTSTART;TZID=America/New_York:20260226T090000\nDTEND;TZID=America/New_York:20260226T093000\nRRULE:FREQ=WEEKLY;BYDAY=TH\nSUMMARY:Sync");
        let p = parse(&t, day(2026, 3, 1), day(2026, 3, 16), &z).unwrap();
        let s: Vec<i64> = p.events.iter().map(|e| e.start).collect();
        assert_eq!(s, [at(2026, 3, 5, 14, 0), at(2026, 3, 12, 13, 0)], "09:00 EST, then 09:00 EDT");
        assert!(p.events.iter().all(|e| e.end - e.start == 1800));
        // Windows and IANA names agree; an unknown one is the PC's zone.
        let w = ev("DTSTART;TZID=W. Europe Standard Time:20260701T120000");
        let b = ev("DTSTART;TZID=Europe/Berlin:20260701T120000");
        assert_eq!(starts(&w, day(2026, 6, 30), day(2026, 7, 2), &z), starts(&b, day(2026, 6, 30), day(2026, 7, 2), &z));
        assert_eq!(starts(&b, day(2026, 6, 30), day(2026, 7, 2), &z), [at(2026, 7, 1, 10, 0)]);
    }

    #[test]
    fn a_repeat_skips_the_hour_the_clocks_jump_over() {
        // 02:30 does not happen in New York on 2026-03-08: that day has no
        // occurrence, and it does not count (RFC 5545 3.3.10).
        let z = SystemZones::new();
        let t = ev("DTSTART;TZID=America/New_York:20260307T023000\nRRULE:FREQ=DAILY;COUNT=3");
        assert_eq!(starts(&t, day(2026, 3, 6), day(2026, 3, 12), &z), [at(2026, 3, 7, 7, 30), at(2026, 3, 9, 6, 30), at(2026, 3, 10, 6, 30)]);
        // A DTSTART in the gap itself is read with the offset before it.
        assert_eq!(starts(&ev("DTSTART;TZID=America/New_York:20260308T023000"), day(2026, 3, 7), day(2026, 3, 9), &z), [at(2026, 3, 8, 7, 30)]);
        // The same with the feed's own rules for the zone.
        let vtz = "BEGIN:VTIMEZONE\nTZID:Custom East\nBEGIN:STANDARD\nDTSTART:16011104T020000\nRRULE:FREQ=YEARLY;BYDAY=1SU;BYMONTH=11\nTZOFFSETFROM:-0400\nTZOFFSETTO:-0500\nEND:STANDARD\nBEGIN:DAYLIGHT\nDTSTART:16010311T020000\nRRULE:FREQ=YEARLY;BYDAY=2SU;BYMONTH=3\nTZOFFSETFROM:-0500\nTZOFFSETTO:-0400\nEND:DAYLIGHT\nEND:VTIMEZONE";
        let own = cal(&format!("{vtz}\nBEGIN:VEVENT\nDTSTART;TZID=Custom East:20260307T023000\nRRULE:FREQ=DAILY;COUNT=3\nEND:VEVENT"));
        assert_eq!(starts(&own, day(2026, 3, 6), day(2026, 3, 12), &fixed()), [at(2026, 3, 7, 7, 30), at(2026, 3, 9, 6, 30), at(2026, 3, 10, 6, 30)]);
    }

    #[test]
    fn a_google_style_feed() {
        let text = "BEGIN:VCALENDAR\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nVERSION:2.0\r\nCALSCALE:GREGORIAN\r\nMETHOD:PUBLISH\r\nX-WR-CALNAME:Kim\r\nX-WR-TIMEZONE:Asia/Seoul\r\n\
BEGIN:VTIMEZONE\r\nTZID:Asia/Seoul\r\nX-LIC-LOCATION:Asia/Seoul\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:+0900\r\nTZOFFSETTO:+0900\r\nTZNAME:KST\r\nDTSTART:19700101T000000\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nDTSTART;TZID=Asia/Seoul:20261029T100000\r\nDTEND;TZID=Asia/Seoul:20261029T103000\r\nRRULE:FREQ=WEEKLY;WKST=SU;BYDAY=TH\r\nEXDATE;TZID=Asia/Seoul:20261112T100000\r\nDTSTAMP:20261105T000000Z\r\nUID:abc123@google.com\r\nCREATED:20261001T000000Z\r\nDESCRIPTION:Weekly team sync\\nJoin: https://meet.example/x\r\nLAST-MODIFIED:20261001T000000Z\r\nLOCATION:Room 3\\, Tower B\r\nSEQUENCE:0\r\nSTATUS:CONFIRMED\r\nSUMMARY:Team sync\r\nTRANSP:OPAQUE\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nDTSTART;TZID=Asia/Seoul:20261105T150000\r\nDTEND;TZID=Asia/Seoul:20261105T153000\r\nDTSTAMP:20261105T000000Z\r\nUID:abc123@google.com\r\nRECURRENCE-ID;TZID=Asia/Seoul:20261105T100000\r\nSUMMARY:Team sync (moved)\r\nLOCATION:Room 3\\, Tower B\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nDTSTART;VALUE=DATE:20261106\r\nDTEND;VALUE=DATE:20261107\r\nUID:holiday@google.com\r\nSUMMARY:Day off\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
        let z = SystemZones::new();
        let p = parse(text, at(2026, 11, 4, 15, 0), at(2026, 11, 20, 15, 0), &z).unwrap();
        assert_eq!(p.name.as_deref(), Some("Kim"));
        let got: Vec<(i64, &str, &str)> = p.events.iter().map(|e| (e.start, e.title.as_str(), e.location.as_str())).collect();
        assert_eq!(
            got,
            [
                (at(2026, 11, 5, 6, 0), "Team sync (moved)", "Room 3, Tower B"),
                (day(2026, 11, 6), "Day off", ""),
                (at(2026, 11, 19, 1, 0), "Team sync", "Room 3, Tower B"),
            ],
            "the 5th moved, the 12th skipped"
        );
    }

    #[test]
    fn an_outlook_style_feed() {
        let text = "BEGIN:VCALENDAR\r\nMETHOD:PUBLISH\r\nPRODID:Microsoft Exchange Server 2010\r\nVERSION:2.0\r\nX-WR-CALNAME:Calendar\r\n\
BEGIN:VTIMEZONE\r\nTZID:Pacific Standard Time\r\nBEGIN:STANDARD\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0700\r\nTZOFFSETTO:-0800\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0800\r\nTZOFFSETTO:-0700\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nRRULE:FREQ=MONTHLY;UNTIL=20270101T000000Z;INTERVAL=1;BYDAY=1MO\r\nUID:0400000082\r\nSUMMARY:Planning\r\nDTSTART;TZID=Pacific Standard Time:20260105T090000\r\nDTEND;TZID=Pacific Standard Time:20260105T100000\r\nCLASS:PUBLIC\r\nPRIORITY:5\r\nTRANSP:OPAQUE\r\nSTATUS:CONFIRMED\r\nLOCATION:Microsoft Teams Meeting\r\nX-MICROSOFT-CDO-BUSYSTATUS:BUSY\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:0400000083\r\nSUMMARY:Thanksgiving\r\nDTSTART;VALUE=DATE:20261126\r\nDTEND;VALUE=DATE:20261128\r\nX-MICROSOFT-CDO-ALLDAYEVENT:TRUE\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
        let p = parse(text, day(2026, 11, 1), day(2026, 12, 31), &SystemZones::new()).unwrap();
        let got: Vec<(i64, i64, bool)> = p.events.iter().map(|e| (e.start, e.end, e.all_day)).collect();
        assert_eq!(
            got,
            [
                (at(2026, 11, 2, 17, 0), at(2026, 11, 2, 18, 0), false),
                (day(2026, 11, 26), day(2026, 11, 28), true),
                (at(2026, 12, 7, 17, 0), at(2026, 12, 7, 18, 0), false),
            ],
            "09:00 PST on the first Mondays; two days off"
        );
        // Without ICU's zone the feed's own rules give the same.
        let custom = text.replace("Pacific Standard Time", "Custom Pacific");
        let p2 = parse(&custom, day(2026, 11, 1), day(2026, 12, 31), &Fixed { zones: vec![], local: 0 }).unwrap();
        assert_eq!(p2.events.iter().map(|e| e.start).collect::<Vec<_>>(), got.iter().map(|g| g.0).collect::<Vec<_>>());
        let summer = parse(&custom, day(2026, 7, 1), day(2026, 7, 31), &Fixed { zones: vec![], local: 0 }).unwrap();
        assert_eq!(summer.events[0].start, at(2026, 7, 6, 16, 0), "09:00 PDT");
    }
}
