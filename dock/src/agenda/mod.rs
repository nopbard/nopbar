//! Today's agenda in the clock panel (topbar/panels.rs): the day's events
//! under the month, from the calendars the user subscribes to (Settings >
//! Top bar > Calendar).
//!
//! Sources:
//! - iCalendar links (ics.rs): Google Calendar's "secret address in iCal
//!   format", Outlook.com's published ICS link, any webcal:// or https://
//!   .ics. Fetched over HTTPS on a worker thread with the update checker's
//!   request (updates::http_get: timeouts, cancelled at shutdown).
//! - Windows' own calendar store (winstore.rs), when its switch is on.
//!
//! When: at most every 15 minutes and only while someone can see the
//! screen (a service watch keeps the time; it rests while the session is
//! idle and is poked when it is not), at once when the sources change, and
//! when the panel opens with data older than 5 minutes. The panel always
//! opens at once from what is known and is redrawn in place when a fetch
//! brings something new (topbar::agenda_changed).
//!
//! Kept: every event from 35 days back to 70 days ahead in memory (a day
//! picked in the month shows its events), and the next 2 days in
//! `%APPDATA%\NopBar\agenda.json`, so the panel has data at once after a
//! restart. The file names sources by a hash of their link, never the link.
//!
//! Privacy: the links are secrets. They are only sent to their own host,
//! only over HTTPS (webcal:// is read as https://; http:// is refused),
//! with no cookies; the log names the host only.

pub mod ics;
pub mod tz;
mod winstore;

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::CalendarCfg;

/// Between fetches while someone can see the screen.
pub const FETCH_EVERY: u64 = 15 * 60;
/// The panel opening fetches when what it shows is older than this.
pub const STALE_ON_OPEN: u64 = 5 * 60;
/// What memory keeps around today (days back, days ahead).
pub const KEEP_BACK: i64 = 35;
pub const KEEP_AHEAD: i64 = 70;
/// What the file keeps: today and tomorrow.
pub const CACHE_DAYS: i64 = 2;
/// A calendar larger than this is refused (bytes).
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
/// The file's format.
/// 2: written after the secret-link scrub (older files are dropped and
/// fetched again).
const CACHE_VERSION: u32 = 2;

/// Why a fetch failed (said in Settings, in the user's language).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Why {
    /// The server answered with this status.
    Http(u32),
    /// No connection, name not found, TLS failed.
    Network,
    Timeout,
    /// Not an iCalendar file.
    Format,
    /// Over MAX_BYTES.
    Size,
    /// The link is not https:// or webcal://.
    Link,
    /// Windows' calendar store cannot be opened.
    Store,
    /// The server sent it to another site (refused: the link is secret).
    Redirect,
}

/// One source's last fetch and its events.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    /// "ics:<hash of the link>" or "windows" (source_key).
    pub key: String,
    /// The calendar's own name (X-WR-CALNAME), if it has one.
    pub name: String,
    /// Unix seconds of the last attempt and of the last success (0: never).
    pub tried: u64,
    pub fetched: u64,
    /// The last attempt failed (its events are the last good ones).
    pub error: Option<Why>,
    /// What it holds: occurrences overlapping [from, to) (UTC seconds).
    pub from: i64,
    pub to: i64,
    pub events: Vec<ics::Event>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct CacheFile {
    version: u32,
    sources: Vec<Source>,
}

/// A source's key: the Windows store, or a hash of the link (the file and
/// Settings name sources by it, never by the link).
pub fn source_key(url: &str) -> String {
    if url == WINDOWS {
        return WINDOWS.into();
    }
    // FNV-1a, 64 bits: stable across versions and processes.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in normalize(url).unwrap_or_default().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("ics:{h:016x}")
}

/// The Windows store's key (and its stand-in "link").
pub const WINDOWS: &str = "windows";

/// The link as fetched: https:// as is, webcal:// (and webcals://) read
/// as https://, surrounding space trimmed. None for anything else (http://
/// would send the secret in the clear).
pub fn normalize(url: &str) -> Option<String> {
    let u = url.trim();
    let lower = u.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &u[8..]
    } else if lower.starts_with("webcals://") {
        &u[10..]
    } else if lower.starts_with("webcal://") {
        &u[9..]
    } else {
        return None;
    };
    let full = format!("https://{rest}");
    crate::updates::split_https(&full).map(|_| full)
}

/// The link's host, the only part that is ever logged or shown.
pub fn host(url: &str) -> String {
    normalize(url).and_then(|u| crate::updates::split_https(&u).map(|(h, _, _)| h)).unwrap_or_default()
}

/// Pure: the calendar page a click on an event without a link of its own
/// opens: the calendar's web page on `day` (y, m, d) where the host is
/// known (Google, Outlook, iCloud), else the host's front page. Never the
/// secret link itself.
pub fn web_page(url: &str, (y, m, d): (i32, u32, u32)) -> String {
    let h = host(url).to_ascii_lowercase();
    if h.is_empty() {
        return String::new();
    }
    if h == "calendar.google.com" || h.ends_with(".google.com") || h == "google.com" {
        return format!("https://calendar.google.com/calendar/r/day/{y}/{m}/{d}");
    }
    if h == "outlook.live.com" || h.ends_with(".live.com") || (h.ends_with("outlook.com") && !h.contains("office")) {
        return format!("https://outlook.live.com/calendar/0/view/day/{y}/{m}/{d}");
    }
    if h.contains("office365.com") || h.contains("office.com") {
        return format!("https://outlook.office.com/calendar/view/day/{y}/{m}/{d}");
    }
    if h.ends_with("icloud.com") {
        return "https://www.icloud.com/calendar/".into();
    }
    format!("https://{h}/")
}

// ---------------------------------------------------------------- state

struct State {
    cfg: CalendarCfg,
    /// apply_config ran (the dock knows its sources).
    configured: bool,
    sources: Vec<Source>,
    /// The cache file was read (or found missing).
    loaded: bool,
    /// A fetch is queued or running.
    fetching: bool,
    /// The bar window to tell when the agenda changed (while the calendar
    /// panel is open), else 0.
    notify: isize,
    sched: Option<crate::service::Id>,
    stopped: bool,
}

static STATE: Mutex<State> =
    Mutex::new(State { cfg: CalendarCfg { agenda: true, tomorrow: true, windows: false, feeds: Vec::new() }, configured: false, sources: Vec::new(), loaded: false, fetching: false, notify: 0, sched: None, stopped: false });

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Posted to the bar window that opened the calendar when the agenda
/// changed (topbar handles it).
pub const WM_AGENDA: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 43;

/// Jobs for the worker thread.
enum Job {
    /// Read agenda.json (once, at startup).
    Load,
    /// Fetch every source.
    Fetch,
}

fn queue() -> &'static crate::idle::IdleQueue<Job> {
    static Q: std::sync::OnceLock<crate::idle::IdleQueue<Job>> = std::sync::OnceLock::new();
    Q.get_or_init(|| {
        crate::idle::IdleQueue::new("agenda", Duration::from_secs(5), 4, |jobs: &mut crate::idle::Jobs<Job>| {
            let com = unsafe { windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED) }.is_ok();
            while let Some(job) = jobs.next() {
                match job {
                    Job::Load => load_cache(),
                    Job::Fetch => fetch_all(),
                }
            }
            if com {
                unsafe { windows::Win32::System::Com::CoUninitialize() };
            }
        })
    })
}

/// The dock's config (startup and every change): the sources. New or
/// changed sources are fetched at once; the first call also reads the file
/// and starts the scheduler.
pub fn apply_config(cfg: &crate::config::Config) {
    let new = cfg.calendar.clone();
    let mut g = state();
    if g.stopped {
        return;
    }
    let first = !g.configured;
    let keys = source_keys;
    let changed = first || keys(&g.cfg) != keys(&new);
    let shown_changed = g.cfg.agenda != new.agenda || g.cfg.tomorrow != new.tomorrow;
    let replan = replans(&g.cfg, &new);
    g.cfg = new;
    g.configured = true;
    if first {
        drop(g);
        // The scheduler first: it waits for the file, which pokes it.
        let id = crate::service::add(Box::new(Scheduler));
        state().sched = id;
        let _ = queue().send(Job::Load);
        return;
    }
    if changed {
        // Gone sources go from memory now; new ones are fetched now.
        let want = keys(&g.cfg);
        g.sources.retain(|s| want.contains(&s.key));
        let has = g.cfg.has_source();
        drop(g);
        if has && g_agenda() {
            fetch_now();
        }
        notify();
    } else if shown_changed {
        drop(g);
        notify();
    } else {
        return;
    }
    // The scheduler plans again (it may be resting with no source, or
    // with the agenda off).
    if replan {
        poke_scheduler();
    }
}

/// The sources a config sets, by key.
fn source_keys(c: &CalendarCfg) -> Vec<String> {
    let mut k: Vec<String> = c.feeds.iter().filter(|f| normalize(&f.url).is_some()).map(|f| source_key(&f.url)).collect();
    if c.windows {
        k.push(WINDOWS.into());
    }
    k
}

/// Pure: a config change the scheduler must plan again for (a source came
/// or went, the agenda was switched, tomorrow too): it may be resting.
pub fn replans(old: &CalendarCfg, new: &CalendarCfg) -> bool {
    source_keys(old) != source_keys(new) || old.agenda != new.agenda || old.tomorrow != new.tomorrow
}

fn poke_scheduler() {
    if let Some(id) = state().sched {
        crate::service::poke(id);
    }
}

/// Shutdown: no more fetches (a request in flight is cancelled by
/// updates::cancel).
pub fn stop() {
    let id = {
        let mut g = state();
        g.stopped = true;
        g.sched
    };
    if let Some(id) = id {
        crate::service::poke(id);
    }
}

/// Someone can see the screen again, or no one can: the scheduler looks
/// again (it fetches if one is due, or rests).
pub fn session_idle_changed() {
    if let Some(id) = state().sched {
        crate::service::poke(id);
    }
}

/// Queues a fetch of every source unless one is on its way.
fn fetch_now() {
    {
        let mut g = state();
        if g.fetching || g.stopped || !g.cfg.has_source() {
            return;
        }
        g.fetching = true;
    }
    if queue().send(Job::Fetch).is_err() {
        state().fetching = false;
    }
}

/// The calendar panel opened on bar window `bar` (0: it closed). While it
/// is open, a fetch that brings something new redraws it; opening it with
/// data older than STALE_ON_OPEN fetches now (never waiting for it).
pub fn panel_open(bar: isize) {
    let stale = {
        let mut g = state();
        g.notify = bar;
        bar != 0 && g.cfg.agenda && g.loaded && unix_now().saturating_sub(last_tried(&g)) >= STALE_ON_OPEN
    };
    if stale {
        fetch_now();
    }
}

/// The oldest last attempt among the sources set (0: one never tried).
fn last_tried(g: &State) -> u64 {
    let mut keys: Vec<String> = g.cfg.feeds.iter().filter(|f| normalize(&f.url).is_some()).map(|f| source_key(&f.url)).collect();
    if g.cfg.windows {
        keys.push(WINDOWS.into());
    }
    keys.iter().map(|k| g.sources.iter().find(|s| &s.key == k).map_or(0, |s| s.tried)).min().unwrap_or(0)
}

fn notify() {
    let (h,sched) = {let g=state();(g.notify,g.sched)};
    if let Some(id)=sched {crate::service::poke(id);}
    if h != 0 {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(windows::Win32::Foundation::HWND(h as *mut _)),
                WM_AGENDA,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            );
        }
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------- what the panel shows

/// What clicking an event opens.
#[derive(Clone, Debug, PartialEq)]
pub enum Open {
    /// An https page: the event's own link, else its calendar's web page.
    Url(String),
    /// Windows' calendar app at this time (UTC seconds).
    Windows(i64),
}

/// An event as the panel gets it.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub ev: ics::Event,
    pub open: Open,
}

/// Everything the panel needs, read at once (Snapshot::read).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Settings' switch (off: no agenda at all).
    pub enabled: bool,
    pub has_source: bool,
    pub tomorrow: bool,
    /// Every source's events, sorted by start.
    pub events: Vec<Shown>,
    /// The span every source holds (UTC seconds; empty when none has data).
    pub from: i64,
    pub to: i64,
    /// A source's last fetch failed.
    pub error: bool,
    /// No source has any data yet (the first fetch is on its way).
    pub loading: bool,
}

impl Snapshot {
    /// What the agenda shows now. `day`: the local day picked (y, m, d)
    /// for the calendar pages of events without a link.
    pub fn read(day: (i32, u32, u32)) -> Snapshot {
        let g = state();
        let cfg = &g.cfg;
        let mut events = Vec::new();
        let mut span: Option<(i64, i64)> = None;
        let mut error = false;
        let mut any = false;
        let mut wanted: Vec<(String, Option<String>)> = cfg.feeds.iter().filter_map(|f| normalize(&f.url).map(|_| (source_key(&f.url), Some(f.url.clone())))).collect();
        if cfg.windows {
            wanted.push((WINDOWS.into(), None));
        }
        for (key, url) in &wanted {
            let Some(s) = g.sources.iter().find(|s| &s.key == key) else { continue };
            error |= s.error.is_some();
            if s.fetched == 0 && s.events.is_empty() {
                continue;
            }
            any = true;
            span = Some(match span {
                None => (s.from, s.to),
                Some((a, b)) => (a.max(s.from), b.min(s.to)),
            });
            for e in &s.events {
                let open = match url {
                    _ if updates_https(&e.url) => Open::Url(e.url.clone()),
                    Some(u) => Open::Url(web_page(u, day)),
                    // An all-day one holds its day, not a moment: Windows
                    // gets that day's local midnight.
                    None => Open::Windows(if e.all_day { tz::utc_from_local(e.start) } else { e.start }),
                };
                events.push(Shown { ev: e.clone(), open });
            }
        }
        events.sort_by(|a, b| (a.ev.start, &a.ev.title).cmp(&(b.ev.start, &b.ev.title)));
        let (from, to) = span.unwrap_or((0, 0));
        Snapshot { enabled: cfg.agenda, has_source: cfg.has_source(), tomorrow: cfg.tomorrow, events, from, to, error, loading: !any && cfg.has_source() && (g.fetching || !g.loaded) }
    }
}

fn updates_https(u: &str) -> bool {
    crate::updates::is_https_url(u) && crate::updates::split_https(u).is_some()
}

/// What Settings shows under a link: its last fetch (from the dock's file;
/// Settings never writes it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub key: String,
    pub fetched: u64,
    pub error: Option<Why>,
    /// Events in the file (today and tomorrow).
    pub events: usize,
}

/// Every source's last fetch, the file read once (call it off the UI
/// thread).
pub fn statuses() -> Vec<Status> {
    read_file().sources.into_iter().map(|s| Status { key: s.key, fetched: s.fetched, error: s.error, events: s.events.len() }).collect()
}

// ---------------------------------------------------------------- the file

fn cache_file() -> std::path::PathBuf {
    crate::config::app_dir().join("agenda.json")
}

/// The file is refused past this (it holds two days of events).
const MAX_CACHE: u64 = 4 * 1024 * 1024;

fn read_file() -> CacheFile {
    let path = cache_file();
    if std::fs::metadata(&path).map(|m| m.len() > MAX_CACHE).unwrap_or(true) {
        return CacheFile::default();
    }
    let Ok(text) = std::fs::read_to_string(path) else { return CacheFile::default() };
    read_cache(&text)
}

/// Pure: the file's text read (anything unusable: an empty file).
fn read_cache(text: &str) -> CacheFile {
    if text.len() as u64 > MAX_CACHE {
        return CacheFile::default();
    }
    match serde_json::from_str::<CacheFile>(text) {
        Ok(f) if f.version == CACHE_VERSION => f,
        _ => CacheFile::default(),
    }
}

/// What the file keeps of each event's texts (characters) and of each
/// source's events, so a written file always reads back (MAX_CACHE).
const CACHE_TEXT: usize = 300;
const CACHE_URL: usize = 2048;
const CACHE_EVENTS: usize = 400;

/// Pure: the file's text for `sources` (already cut to its two days):
/// texts and event counts capped, then, while it is still over half of
/// MAX_CACHE, the latest events go first. None if even that does not fit
/// (the old file then stays).
fn cache_text(mut sources: Vec<Source>) -> Option<String> {
    let cut = |s: &mut String, n: usize| {
        if s.chars().count() > n {
            *s = s.chars().take(n).collect();
        }
    };
    for s in &mut sources {
        cut(&mut s.name, CACHE_TEXT);
        s.events.truncate(CACHE_EVENTS);
        for e in &mut s.events {
            cut(&mut e.title, CACHE_TEXT);
            cut(&mut e.location, CACHE_TEXT);
            if e.url.len() > CACHE_URL {
                e.url.clear();
            }
        }
    }
    let budget = (MAX_CACHE / 2) as usize;
    loop {
        let text = serde_json::to_string(&CacheFile { version: CACHE_VERSION, sources: sources.clone() }).ok()?;
        if text.len() <= budget {
            return Some(text);
        }
        // The source with the most events gives up its last one.
        let s = sources.iter_mut().filter(|s| !s.events.is_empty()).max_by_key(|s| s.events.len())?;
        let drop = (s.events.len() / 4).max(1);
        s.events.truncate(s.events.len() - drop);
    }
}

/// Pure: what goes into the file: each source with only the events that
/// overlap [from, to) (today and tomorrow, UTC seconds), the rest kept.
pub fn for_file(sources: &[Source], from: i64, to: i64) -> Vec<Source> {
    sources
        .iter()
        .map(|s| {
            let mut s = s.clone();
            s.events.retain(|e| overlaps(e, from, to));
            s.from = s.from.max(from);
            s.to = s.to.min(to).max(s.from);
            s
        })
        .collect()
}

/// Pure: event `e` overlaps [from, to) (UTC seconds; an all-day event by
/// its local days, given as local midnights read as UTC: see ics::Event).
pub fn overlaps(e: &ics::Event, from: i64, to: i64) -> bool {
    if e.all_day {
        // Its days against the span's local days, a day of slack each way.
        return e.start < to + 86_400 && e.end > from - 86_400;
    }
    e.start < to && (e.end > from || (e.end <= e.start && e.start >= from))
}

fn save_file(sources: &[Source]) {
    let (from, to) = days_span(0, CACHE_DAYS);
    let Some(text) = cache_text(for_file(sources, from, to)) else { return };
    let path = cache_file();
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::create_dir_all(crate::config::app_dir());
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn load_cache() {
    let f = read_file();
    let mut g = state();
    if g.sources.is_empty() {
        g.sources = f.sources;
    }
    g.loaded = true;
    let sched = g.sched;
    drop(g);
    notify();
    // The scheduler waited for the file: it can plan now.
    if let Some(id) = sched {
        crate::service::poke(id);
    }
}

/// The agenda is switched on.
fn g_agenda() -> bool {
    state().cfg.agenda
}

/// UTC seconds of local midnight `a` days from today to local midnight
/// `b` days from today.
pub fn days_span(a: i64, b: i64) -> (i64, i64) {
    let today = local_day(unix_now() as i64);
    (tz::utc_from_local((today + a) * 86_400), tz::utc_from_local((today + b) * 86_400))
}

/// The local day (days since 1970-01-01) of UTC moment `utc`.
pub fn local_day(utc: i64) -> i64 {
    tz::local_from_utc(utc).div_euclid(86_400)
}

// ---------------------------------------------------------------- fetching

/// Pure: what a failed request's message means.
pub fn why(err: &str) -> Why {
    if let Some(code) = err.strip_prefix("HTTP ").and_then(|c| c.trim().parse::<u32>().ok()) {
        return Why::Http(code);
    }
    let e = err.to_ascii_lowercase();
    if e.contains("timed out") || e.contains("timeout") {
        Why::Timeout
    } else if e.contains("too big") {
        Why::Size
    } else if e.contains("redirect") {
        Why::Redirect
    } else if e.contains("not a plain https") {
        Why::Link
    } else {
        Why::Network
    }
}

/// GETs and reads one link (on a worker thread): the calendar's name and
/// the occurrences in [from, to). Logs the host only.
pub fn fetch_feed(url: &str, from: i64, to: i64) -> Result<ics::Parsed, Why> {
    let Some(u) = normalize(url) else { return Err(Why::Link) };
    let host = host(url);
    let mut body = Vec::new();
    let lim = crate::updates::Limits { total: Duration::from_secs(30), step_ms: 15_000, same_site: true };
    let got = crate::updates::http_get(&u, &lim, &mut |n| if n as usize > MAX_BYTES { Err("too big".into()) } else { Ok(()) }, &mut |chunk| {
        body.extend_from_slice(chunk);
        if body.len() > MAX_BYTES {
            return Err("too big".into());
        }
        Ok(())
    });
    if let Err(e) = got {
        let w = why(&e);
        crate::log!("agenda: {host}: fetch failed ({w:?})");
        return Err(w);
    }
    let zones = tz::SystemZones::new();
    // Unfolded as bytes first: a fold may split a character.
    match ics::parse_bytes(&body, from, to, &zones) {
        Ok(mut p) => {
            crate::log!("agenda: {host}: {} bytes, {} events", body.len(), p.events.len());
            if p.dropped > 0 {
                // Their exceptions were cut: shown, they could bring back
                // cancelled events.
                crate::log!("agenda: {host}: {} repeating events left out (too many exceptions)", p.dropped);
            }
            scrub_parsed(&mut p, url);
            Ok(p)
        }
        Err(_) => {
            crate::log!("agenda: {host}: not an iCalendar file ({} bytes)", body.len());
            Err(Why::Format)
        }
    }
}

/// What of a subscription link counts as secret, so feed text that echoes
/// it is scrubbed before it is kept or shown. One rule:
///
/// - The link as entered and as fetched (webcal:// read as https://), each
///   anywhere in the text, in any case: a whole echo is always caught.
/// - Each path segment and query value of 16 or more characters, or of 8
///   or more with both a letter and a digit (a token: "private-3f9a…",
///   "RkMpVtLsQnBxZaYc"), as a whole token (not inside a longer word).
///   A word followed by a year ("team2026", "q3-2026") is a public name.
/// - Nothing shorter, and never the host.
///
/// Accepted risk: a short secret (under 8 characters, or 8 to 15 letters
/// only) echoed by a hostile server is not caught. It then stays only in
/// this PC's own cache and on the user's own screen, which already hold the
/// link itself (config.json); it is never logged or sent anywhere.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Secrets {
    /// The whole link, lowercased (matched anywhere).
    whole: Vec<String>,
    /// Its tokens, lowercased (matched as whole tokens).
    tokens: Vec<String>,
}

/// Pure: a path segment or query value that is a token (see Secrets).
fn is_token(s: &str) -> bool {
    let n = s.chars().count();
    let letter = s.chars().any(|c| c.is_alphabetic());
    let digit = s.bytes().any(|b| b.is_ascii_digit());
    (n >= 16 || (n >= 8 && letter && digit)) && !word_and_year(s)
}

/// Pure: letters (and '-' / '_') then a year, 1900 to 2099: "team2026".
fn word_and_year(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 6 {
        return false;
    }
    let (word, year) = b.split_at(b.len() - 4);
    let word_ok = word.iter().all(|c| c.is_ascii_alphabetic() || matches!(c, b'-' | b'_')) && word.iter().filter(|c| c.is_ascii_alphabetic()).count() >= 2;
    let year_ok = year.iter().all(u8::is_ascii_digit) && (year.starts_with(b"19") || year.starts_with(b"20"));
    word_ok && year_ok
}

/// The secrets of subscription link `url` (Secrets).
pub fn secrets(url: &str) -> Secrets {
    let mut s = Secrets::default();
    let entered = url.trim().to_ascii_lowercase();
    if !entered.is_empty() {
        s.whole.push(entered);
    }
    let Some(u) = normalize(url) else { return s };
    s.whole.push(u.to_ascii_lowercase());
    if let Some((_, _, path)) = crate::updates::split_https(&u) {
        let (p, q) = path.split_once('?').unwrap_or((path.as_str(), ""));
        let values = q.split('&').map(|pair| pair.split_once('=').map_or(pair, |(_, v)| v));
        for piece in p.split('/').chain(values) {
            if is_token(piece) {
                s.tokens.push(piece.to_ascii_lowercase());
            }
        }
    }
    s.whole.sort();
    s.whole.dedup();
    // Longest first: a token goes before one inside it.
    s.tokens.sort_by_key(|t| std::cmp::Reverse(t.len()));
    s.tokens.dedup();
    s
}

/// Pure: where `piece` is in `lower` as a whole token (not inside a word:
/// the bytes either side are not letters or digits).
fn token_at(lower: &str, piece: &str) -> Option<usize> {
    let b = lower.as_bytes();
    lower.match_indices(piece).map(|(i, _)| i).find(|&i| {
        let before = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let end = i + piece.len();
        let after = end >= b.len() || !b[end].is_ascii_alphanumeric();
        before && after
    })
}

/// Pure: `text` holds the link or one of its tokens.
fn has_secret(text: &str, s: &Secrets) -> bool {
    let l = text.to_ascii_lowercase();
    s.whole.iter().any(|w| l.contains(w.as_str())) || s.tokens.iter().any(|t| token_at(&l, t).is_some())
}

/// Pure: `text` with the link (anywhere), each link-like word holding a
/// token, and each token itself replaced by "…". `no_links`: every link
/// goes, whatever it holds (a calendar's name). A word is a link when it
/// has "://" or starts with "www." ("WebCalendar" is a name).
pub fn scrub(text: &str, s: &Secrets, no_links: bool) -> String {
    // ASCII lowercasing keeps byte offsets, so matches map back.
    let mut out = text.to_string();
    for w in &s.whole {
        while let Some(i) = out.to_ascii_lowercase().find(w.as_str()) {
            out.replace_range(i..i + w.len(), "…");
        }
    }
    let linky = |w: &str| {
        let l = w.to_ascii_lowercase();
        l.contains("://") || l.starts_with("www.")
    };
    let words: Vec<String> = out.split(' ').map(|w| if linky(w) && (no_links || has_secret(w, s)) { "…".to_string() } else { w.to_string() }).collect();
    let mut out = words.join(" ");
    for t in &s.tokens {
        while let Some(i) = token_at(&out.to_ascii_lowercase(), t) {
            out.replace_range(i..i + t.len(), "…");
        }
    }
    out
}

/// Settings' names for the links (a calendar's own name, taken from a
/// feed by an older build, may hold its link): scrubbed. True if one
/// changed (config::load then saves it, once).
pub fn scrub_feed_names(feeds: &mut [crate::config::CalendarFeed]) -> bool {
    let mut changed = false;
    for f in feeds {
        let clean = scrub(&f.name, &secrets(&f.url), true).trim().to_string();
        if clean != f.name {
            f.name = clean;
            changed = true;
        }
    }
    changed
}

/// A parsed feed made safe to keep and show: no piece of its own secret
/// link in the calendar's name (no links at all there), titles, places or
/// event links.
pub fn scrub_parsed(p: &mut ics::Parsed, url: &str) {
    let parts = secrets(url);
    if let Some(n) = p.name.as_mut() {
        *n = scrub(n, &parts, true).trim().to_string();
    }
    for e in &mut p.events {
        e.title = scrub(&e.title, &parts, false);
        e.location = scrub(&e.location, &parts, false);
        if has_secret(&e.url, &parts) {
            e.url.clear();
        }
    }
}

/// Every source, one after the other (worker thread): the results replace
/// what memory holds (a failure keeps the last good events), the file is
/// written, and an open panel is told when something changed.
fn fetch_all() {
    let cfg = state().cfg.clone();
    let (from, to) = days_span(-KEEP_BACK, KEEP_AHEAD);
    let mut targets: Vec<String> = cfg.feeds.iter().filter(|f| normalize(&f.url).is_some()).map(|f| f.url.clone()).collect();
    if cfg.windows {
        targets.push(WINDOWS.into());
    }
    let mut changed = false;
    for url in targets {
        // Nobody to see it, switched off or taken off meanwhile: the rest
        // waits (the scheduler fetches it once someone is back).
        let still = {
            let g = state();
            !g.stopped && g.cfg.agenda && ((url == WINDOWS && g.cfg.windows) || g.cfg.feeds.iter().any(|f| f.url == url))
        };
        if !still || crate::dock::session_idle() {
            break;
        }
        let key = source_key(&url);
        let now = unix_now();
        let got = if url == WINDOWS { winstore::fetch(from, to).map(|events| ics::Parsed { name: None, events, dropped: 0 }) } else { fetch_feed(&url, from, to) };
        let mut g = state();
        let i = match g.sources.iter().position(|s| s.key == key) {
            Some(i) => i,
            None => {
                g.sources.push(Source { key: key.clone(), ..Default::default() });
                g.sources.len() - 1
            }
        };
        let s = &mut g.sources[i];
        let before = s.clone();
        s.tried = now;
        match got {
            Ok(p) => {
                s.error = None;
                s.fetched = now;
                s.name = p.name.unwrap_or_default();
                s.events = p.events;
                s.from = from;
                s.to = to;
            }
            Err(w) => s.error = Some(w),
        }
        changed |= (before.events != s.events) || (before.error != s.error) || before.fetched == 0;
    }
    let mut g = state();
    g.fetching = false;
    // A source taken off meanwhile is not kept.
    let keep: Vec<String> = g.cfg.feeds.iter().map(|f| source_key(&f.url)).chain(g.cfg.windows.then(|| WINDOWS.to_string())).collect();
    g.sources.retain(|s| keep.contains(&s.key));
    let all = g.sources.clone();
    drop(g);
    save_file(&all);
    if changed {
        notify();
    }
    // The next round is planned from this one's time.
    poke_scheduler();
}

// ---------------------------------------------------------------- the scheduler

/// What the scheduler does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Fetch now (then sleep FETCH_EVERY).
    Fetch,
    Sleep(u64),
    /// Until poked (no source, or nobody can see the screen).
    Wait,
}

/// Pure: the plan at unix time `now` for sources last tried at `last` (0:
/// never), while the session is `idle` or not.
pub fn plan(has_source: bool, idle: bool, last: u64, now: u64) -> Plan {
    if !has_source || idle {
        return Plan::Wait;
    }
    let due = last.saturating_add(FETCH_EVERY);
    if last == 0 || now >= due {
        Plan::Fetch
    } else {
        // Wakes at least hourly anyway: timed waits do not count sleep.
        Plan::Sleep((due - now).min(3600))
    }
}

struct Scheduler;

impl crate::service::Watch for Scheduler {
    fn name(&self) -> &'static str {
        "agenda scheduler"
    }
    fn handles(&self) -> Vec<windows::Win32::Foundation::HANDLE> {
        Vec::new()
    }
    fn wake(&mut self, _why: crate::service::Wake) -> crate::service::Next {
        use crate::service::Next;
        let (stopped, loaded, has, last) = {
            let g = state();
            (g.stopped, g.loaded, g.cfg.has_source() && g.cfg.agenda, last_tried(&g))
        };
        if stopped {
            return Next::Done;
        }
        // The file first (what was fetched before a restart counts).
        if !loaded {
            return Next::Wait;
        }
        let idle=crate::dock::session_idle();
        let reminder=if has&&!idle {companion_reminders(unix_now() as i64)}else{None};
        let wake_in=|seconds:u64|Duration::from_secs(reminder.map_or(seconds,|r|r.min(seconds)).max(1));
        match plan(has, idle, last, unix_now()) {
            Plan::Fetch => {
                fetch_now();
                Next::Sleep(wake_in(FETCH_EVERY))
            }
            Plan::Sleep(s) => Next::Sleep(wake_in(s)),
            Plan::Wait => Next::Wait,
        }
    }
}

// ---------------------------------------------------------------- opening

/// Opens what a click on an event asks for (off the UI thread where it
/// could wait).
pub fn open(o: &Open) {
    match o {
        Open::Url(u) if updates_https(u) => {
            let h = windows::core::HSTRING::from(u.as_str());
            let _ = crate::launch::environment::launch("open", &h.to_string(), "", "");
        }
        Open::Windows(start) => {
            let start = *start;
            let _ = std::thread::Builder::new().name("agenda-open".into()).spawn(move || winstore::show(start));
        }
        Open::Url(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_read_as_https_only() {
        assert_eq!(normalize(" webcal://p01-calendars.icloud.com/published/2/abc ").as_deref(), Some("https://p01-calendars.icloud.com/published/2/abc"));
        assert_eq!(normalize("WEBCAL://x.example/a.ics").as_deref(), Some("https://x.example/a.ics"));
        assert_eq!(normalize("https://calendar.google.com/calendar/ical/me%40gmail.com/private-abc/basic.ics").as_deref(), Some("https://calendar.google.com/calendar/ical/me%40gmail.com/private-abc/basic.ics"));
        assert_eq!(normalize("http://x.example/a.ics"), None, "the secret would travel in the clear");
        assert_eq!(normalize("file:///c:/a.ics"), None);
        assert_eq!(normalize("https://user:pw@x.example/a.ics"), None);
        assert_eq!(host("webcal://p01-calendars.icloud.com/published/2/abc"), "p01-calendars.icloud.com");
        assert_eq!(host("nope"), "");
    }

    #[test]
    fn sources_are_named_by_a_hash_never_the_link() {
        let a = source_key("https://calendar.google.com/calendar/ical/x/private-secret/basic.ics");
        assert!(a.starts_with("ics:") && !a.contains("secret") && !a.contains("google"));
        assert_eq!(a, source_key("  webcal://calendar.google.com/calendar/ical/x/private-secret/basic.ics"), "the same calendar");
        assert_ne!(a, source_key("https://calendar.google.com/calendar/ical/y/private-secret/basic.ics"));
        assert_eq!(source_key(WINDOWS), "windows");
    }

    #[test]
    fn events_without_a_link_open_their_calendar_page() {
        let d = (2026, 11, 5);
        assert_eq!(web_page("https://calendar.google.com/calendar/ical/x/private-y/basic.ics", d), "https://calendar.google.com/calendar/r/day/2026/11/5");
        assert_eq!(web_page("https://outlook.live.com/owa/calendar/abc/def/calendar.ics", d), "https://outlook.live.com/calendar/0/view/day/2026/11/5");
        assert_eq!(web_page("https://outlook.office365.com/owa/calendar/abc/def/calendar.ics", d), "https://outlook.office.com/calendar/view/day/2026/11/5");
        assert_eq!(web_page("webcal://p01-calendars.icloud.com/published/2/secret", d), "https://www.icloud.com/calendar/");
        // Elsewhere: the host's front page, never the secret path.
        let other = web_page("https://cal.example.org/private/secret-token/cal.ics", d);
        assert_eq!(other, "https://cal.example.org/");
        assert_eq!(web_page("http://cal.example.org/a.ics", d), "");
    }

    #[test]
    fn fetch_errors_are_sorted() {
        assert_eq!(why("HTTP 404"), Why::Http(404));
        assert_eq!(why("receive: timed out after 30000 ms"), Why::Timeout);
        assert_eq!(why("too big"), Why::Size);
        assert_eq!(why("redirect to another site refused"), Why::Redirect);
        assert_eq!(why("send: The server name or address could not be resolved"), Why::Network);
    }

    #[test]
    fn config_changes_wake_the_scheduler() {
        let none = CalendarCfg::default();
        let one = CalendarCfg { feeds: vec![crate::config::CalendarFeed { name: "a".into(), url: "https://a.example/a.ics".into() }], ..Default::default() };
        assert!(replans(&none, &one), "the first source");
        assert!(replans(&one, &none), "the last one gone");
        assert!(replans(&one, &CalendarCfg { agenda: false, ..one.clone() }), "off");
        assert!(replans(&CalendarCfg { agenda: false, ..one.clone() }, &one), "on again");
        assert!(replans(&none, &CalendarCfg { windows: true, ..none.clone() }));
        // A rename alone: nothing to plan.
        let renamed = CalendarCfg { feeds: vec![crate::config::CalendarFeed { name: "b".into(), url: "https://a.example/a.ics".into() }], ..Default::default() };
        assert!(!replans(&one, &renamed));
        // After a fetch the next round is a full interval away; resting while idle.
        assert_eq!(plan(true, false, 1000, 1000), Plan::Sleep(FETCH_EVERY));
        assert_eq!(plan(true, true, 1000, 1000 + FETCH_EVERY), Plan::Wait);
    }

    #[test]
    fn fetches_wait_for_someone_to_see_them() {
        assert_eq!(plan(false, false, 0, 1000), Plan::Wait, "no source: nothing to do");
        assert_eq!(plan(true, true, 0, 1000), Plan::Wait, "idle: no fetch, no timer");
        assert_eq!(plan(true, false, 0, 1000), Plan::Fetch, "never fetched");
        assert_eq!(plan(true, false, 1000, 1000 + FETCH_EVERY), Plan::Fetch);
        assert_eq!(plan(true, false, 1000, 1000 + 60), Plan::Sleep(FETCH_EVERY - 60));
    }

    #[test]
    fn feeds_never_echo_their_secret_link() {
        let url = "https://cal.example/private/SECRET-token-1234/basic.ics?key=abcdefgh1234";
        let mut p = ics::Parsed {
            name: Some("Team https://cal.example/private/SECRET-token-1234/basic.ics www.other.example".into()),
            events: vec![ics::Event {
                title: "see webcal://CAL.example/private/secret-token-1234/basic.ics?key=abcdefgh1234 now".into(),
                location: "token secret-token-1234 here".into(),
                url: "https://cal.example/private/SECRET-token-1234/basic.ics".into(),
                ..Default::default()
            }, ics::Event { title: "Lunch".into(), url: "https://meet.example/room/42".into(), ..Default::default() }],
            ..Default::default()
        };
        scrub_parsed(&mut p, url);
        let all = format!("{:?}", p).to_ascii_lowercase();
        assert!(!all.contains("secret-token-1234") && !all.contains("abcdefgh1234") && !all.contains("/private/"), "{all}");
        assert_eq!(p.name.as_deref(), Some("Team … …"), "no links at all in the name");
        assert_eq!(p.events[0].title, "see … now");
        assert_eq!(p.events[0].location, "token … here");
        assert_eq!(p.events[0].url, "", "the subscription itself is not an event link");
        // Other events and their own links stay.
        assert_eq!((p.events[1].title.as_str(), p.events[1].url.as_str()), ("Lunch", "https://meet.example/room/42"));
        // A cache written from it holds none of the link either.
        let s = Source { key: source_key(url), name: p.name.clone().unwrap_or_default(), events: p.events.clone(), ..Default::default() };
        assert!(!cache_text(vec![s]).unwrap().to_ascii_lowercase().contains("secret-token-1234"));
    }

    #[test]
    fn the_secret_link_rule() {
        // The review's link: a 16-letter token, echoed whole in a title
        // and as the event's link.
        let url = "https://cal.example/private/RkMpVtLsQnBxZaYc/basic.ics";
        let mut p = ics::Parsed {
            events: vec![ics::Event { title: format!("See {url} now"), url: url.into(), ..Default::default() }],
            ..Default::default()
        };
        scrub_parsed(&mut p, url);
        assert_eq!(p.events[0].title, "See … now");
        assert_eq!(p.events[0].url, "");
        let s = secrets(url);
        assert_eq!(s.tokens, ["rkmpvtlsqnbxzayc"], "16 letters: a token; 'private', 'basic.ics' are not");
        assert_eq!(scrub("code RkMpVtLsQnBxZaYc", &s, false), "code …");
        // The link as entered and as fetched, anywhere, any case.
        let w = secrets("webcal://Cal.Example/feed?k=x");
        assert_eq!(scrub("a WEBCAL://cal.example/feed?k=x b https://cal.example/feed?k=x", &w, false), "a … b …");
        // 8+ with a letter and a digit: a token; a word and a year: a name.
        let t = secrets("https://cal.example/team2026/q3-2026/feed?token=ab12cd34");
        assert_eq!(t.tokens, ["ab12cd34"]);
        assert_eq!(scrub("team2026 review, Q3-2026 plan", &t, false), "team2026 review, Q3-2026 plan");
        assert_eq!(scrub("token ab12cd34", &t, false), "token …");
        assert_eq!(scrub("xab12cd34x", &t, false), "xab12cd34x", "inside a word");
        // Shorter pieces and the host are never secret.
        let short = secrets("https://cal.example/feed?token=abcdefgh");
        assert!(short.tokens.is_empty());
        for title in ["Calendar review", "Feed the cat", "Meet at cal.example", "abcdefgh"] {
            assert_eq!(scrub(title, &short, false), title);
        }
        // A link on the same host without the secret is kept; "WebCalendar" is a name.
        assert!(!has_secret("https://cal.example/private/other/meeting", &s));
        assert_eq!(scrub("WebCalendar", &s, true), "WebCalendar");
        // Google's token.
        let g = secrets("https://calendar.google.com/calendar/ical/me%40gmail.com/private-3f9a2b7c1d/basic.ics");
        assert!(g.tokens.contains(&"private-3f9a2b7c1d".to_string()), "{g:?}");
    }

    #[test]
    fn names_taken_from_older_feeds_are_cleaned_once() {
        let url = "https://cal.example/feed?token=abcdefgh";
        let mut feeds = vec![
            crate::config::CalendarFeed { name: "Team https://cal.example/feed?token=abcdefgh".into(), url: url.into() },
            crate::config::CalendarFeed { name: "Work".into(), url: url.into() },
        ];
        assert!(scrub_feed_names(&mut feeds));
        assert_eq!(feeds[0].name, "Team …");
        assert_eq!(feeds[1].name, "Work");
        assert!(!scrub_feed_names(&mut feeds), "nothing left to clean");
        // A file of the older format is dropped (fetched again).
        assert!(read_cache(r#"{"version":1,"sources":[{"key":"ics:1","name":"x"}]}"#).sources.is_empty());
    }

    #[test]
    fn a_written_cache_always_reads_back() {
        // 3,000 events with 1,500-character titles: far over the cap as is.
        let long = "x".repeat(1500);
        let events: Vec<ics::Event> = (0..3000).map(|i| ics::Event { start: i, end: i + 60, title: long.clone(), location: long.clone(), ..Default::default() }).collect();
        let s = Source { key: "ics:1".into(), events, ..Default::default() };
        let text = cache_text(vec![s.clone(), Source { key: "ics:2".into(), ..s }]).unwrap();
        assert!(text.len() as u64 <= MAX_CACHE, "{}", text.len());
        let back = read_cache(&text);
        assert_eq!(back.sources.len(), 2, "read back whole");
        assert!(back.sources.iter().all(|s| !s.events.is_empty() && s.events.len() <= CACHE_EVENTS));
        assert!(back.sources[0].events.iter().all(|e| e.title.chars().count() <= CACHE_TEXT));
        // The earliest events are the ones kept.
        assert_eq!(back.sources[0].events[0].start, 0);
    }

    #[test]
    fn the_file_keeps_two_days() {
        let ev = |start: i64, end: i64| ics::Event { start, end, title: format!("{start}"), ..Default::default() };
        let all_day = |day: i64| ics::Event { start: day * 86_400, end: (day + 1) * 86_400, all_day: true, ..Default::default() };
        let s = Source { key: "ics:1".into(), from: 0, to: 1_000_000, events: vec![ev(100, 200), ev(5_000, 6_000), ev(9_000, 9_500), ev(7_000, 7_000), all_day(0), all_day(30)], ..Default::default() };
        let kept = for_file(&[s], 4_000, 8_000);
        let starts: Vec<i64> = kept[0].events.iter().map(|e| e.start).collect();
        assert_eq!(starts, [5_000, 7_000, 0], "inside, a zero-length one inside, an all-day one on the day");
        assert_eq!((kept[0].from, kept[0].to), (4_000, 8_000));
        // A cache file never holds a link.
        let text = serde_json::to_string(&CacheFile { version: CACHE_VERSION, sources: kept }).unwrap();
        assert!(!text.contains("http"));
    }
}

pub fn companion_day(at:i64)->(i32,u32,u32){tz::civil_from_days(local_day(at))}
fn companion_reminders(now:i64)->Option<u64>{
    let g=state();if !g.cfg.agenda{return None;}
    let mut next=None;
    for source in &g.sources {
        if source.key!=WINDOWS&&!g.cfg.feeds.iter().any(|f|source_key(&f.url)==source.key){continue;}
        if source.key==WINDOWS&&!g.cfg.windows{continue;}
        for e in source.events.iter().filter(|e|!e.all_day&&e.start>now){
            let left=e.start-now;
            if left<=600{crate::companion::cue(crate::companion::Cue::Agenda{key:format!("{}:{}",source.key,e.title),at:e.start});}
            let due=if left>600{left-600}else{left+1} as u64;next=Some(next.map_or(due,|n:u64|n.min(due)));
        }
    }
    next
}
