//! The Live Island's model, free of windows and visuals: what is live,
//! in which order, what the pill says, where it goes on the bar and how it
//! moves between hidden, pill and expanded. Everything here is pure (the
//! tests drive it with plain values).

use crate::i18n::{t, tf};

/// What can be live, in priority order (the pill shows the first one).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Progress,
    Media,
    Battery,
    Timer,
    Privacy,
    Screenshot,
}

/// One app reporting progress on its taskbar button (dock/badges.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    /// Stable per app (the dock item's path).
    pub key: String,
    pub name: String,
    /// It is File Explorer: its progress is a file copy or move.
    pub files: bool,
    /// 0..1.
    pub ratio: f32,
    /// One of its windows (to bring to the front), 0 = none.
    pub hwnd: isize,
}

/// What is playing (Windows' media session, control.rs).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_toggle: bool,
    /// The session's app (its SourceAppUserModelId: an AUMID or an exe
    /// name), to find its audio session (the card's volume slider).
    pub app: String,
    /// Where in the track it is (None: the app gives no timeline).
    pub timeline: Option<Timeline>,
    /// Which session and track this is (control.rs): a new one cancels a
    /// seek being dragged or held, and a seek sent for an older one is
    /// dropped by the media worker.
    pub gen: u32,
    /// Which media session this is (control.rs: its bound count), for a
    /// PiP tied to the session it was opened for.
    pub session: u32,
    /// Seeks the app refused so far (a change drops the card's hold).
    pub seek_refused: u32,
}

/// A track's timeline as Windows' media session last reported it: the
/// position `pos_ms` held at the monotonic instant `at`. Between reports
/// the card moves it on by the clock and the playback rate (it never asks
/// again every second).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timeline {
    pub start_ms: i64,
    pub end_ms: i64,
    pub pos_ms: i64,
    pub at: std::time::Instant,
    /// 1.0 = normal speed.
    pub rate: f64,
    /// The app takes a new position (IsPlaybackPositionEnabled).
    pub seekable: bool,
    /// Where a seek may go (MinSeekTime..MaxSeekTime; a live stream's
    /// window can be shorter than the track). Both 0: the app gives none.
    pub seek_min_ms: i64,
    pub seek_max_ms: i64,
}

/// Two timeline reports closer than this (where each puts the track now)
/// are the same course: nothing to draw again.
pub const TIMELINE_SLACK_MS: i64 = 1000;
/// The keyboard's step on the position bar.
pub const SEEK_STEP_MS: i64 = 5000;

impl Timeline {
    /// A timeline worth a bar: it has an end after its start (EndTime 0 =
    /// the app gives none).
    pub fn shown(&self) -> bool {
        self.end_ms > 0 && self.end_ms > self.start_ms
    }

    /// The track's length.
    pub fn length_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }

    /// Where the track is at `now`: moved on from the report while it plays.
    pub fn position(&self, now: std::time::Instant, playing: bool) -> i64 {
        let elapsed = now.saturating_duration_since(self.at).as_millis().min(i64::MAX as u128) as i64;
        interpolate(self.pos_ms, elapsed, self.rate, playing, self.start_ms, self.end_ms)
    }

    /// The played share at position `pos`, 0..1.
    pub fn ratio(&self, pos: i64) -> f32 {
        let len = self.length_ms();
        if len <= 0 {
            return 0.0;
        }
        ((pos - self.start_ms) as f64 / len as f64).clamp(0.0, 1.0) as f32
    }

    /// The position at share `v` (0..1) of the track.
    pub fn at_ratio(&self, v: f32) -> i64 {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        self.start_ms + (self.length_ms() as f64 * v as f64).round() as i64
    }

    /// `pos` moved by `delta`, kept inside the track.
    pub fn step(&self, pos: i64, delta: i64) -> i64 {
        pos.saturating_add(delta).clamp(self.start_ms, self.end_ms.max(self.start_ms))
    }

    /// Where a seek may go: MinSeekTime..MaxSeekTime inside the track (an
    /// app that gives neither, both 0, takes the whole track). None: it
    /// takes no seek (not enabled, or nothing left of the range).
    pub fn seek_range(&self) -> Option<(i64, i64)> {
        if !self.seekable || !self.shown() {
            return None;
        }
        let (lo, hi) = if self.seek_min_ms == 0 && self.seek_max_ms == 0 {
            (self.start_ms, self.end_ms)
        } else {
            (self.seek_min_ms.max(self.start_ms), self.seek_max_ms.min(self.end_ms))
        };
        (hi > lo).then_some((lo, hi))
    }

    /// The bar can be moved.
    pub fn can_seek(&self) -> bool {
        self.seek_range().is_some()
    }

    /// `ms` kept inside where a seek may go (None: no seeking).
    pub fn clamp_seek(&self, ms: i64) -> Option<i64> {
        let (lo, hi) = self.seek_range()?;
        Some(ms.clamp(lo, hi))
    }
}

/// A seek as the card posts it to the bar (lparam): the track's `gen` in
/// the high 32 bits, the position (ms, 0..u32::MAX: 49 days) in the low.
pub fn pack_seek(gen: u32, ms: i64) -> isize {
    (((gen as u64) << 32) | ms.clamp(0, u32::MAX as i64) as u64) as i64 as isize
}

/// The `(gen, ms)` of a packed seek.
pub fn unpack_seek(lp: isize) -> (u32, i64) {
    let v = lp as i64 as u64;
    ((v >> 32) as u32, (v & 0xFFFF_FFFF) as i64)
}

/// Seconds for a screen reader's RangeValue (fractions kept: a short clip
/// still has a range, and a value set reads back as set).
pub fn secs(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

const _: () = assert!(std::mem::size_of::<isize>() == 8, "pack_seek needs a 64-bit lparam");

/// The position `elapsed_ms` after it was `pos_ms`: moved on at `rate`
/// while playing (a rate that is not a number or not above 0 counts as
/// 1), held while paused; always inside `start..end` (an end not after
/// the start only holds it at or after the start).
pub fn interpolate(pos_ms: i64, elapsed_ms: i64, rate: f64, playing: bool, start: i64, end: i64) -> i64 {
    let rate = if rate.is_finite() && rate > 0.0 { rate } else { 1.0 };
    let moved = if playing { (elapsed_ms.max(0) as f64 * rate).round() as i64 } else { 0 };
    let p = pos_ms.saturating_add(moved);
    if end > start { p.clamp(start, end) } else { p.max(start) }
}

/// Do two reports describe the same course? (Both none, or the same
/// track length, rate and seeking, and where each puts the track at the
/// later report's instant within TIMELINE_SLACK_MS.)
pub fn same_course(a: Option<&Timeline>, b: Option<&Timeline>, playing: bool) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            let now = a.at.max(b.at);
            a.start_ms == b.start_ms
                && a.end_ms == b.end_ms
                && a.seekable == b.seekable
                && a.seek_min_ms == b.seek_min_ms
                && a.seek_max_ms == b.seek_max_ms
                && (a.rate - b.rate).abs() < 0.01
                && (a.position(now, playing) - b.position(now, playing)).abs() < TIMELINE_SLACK_MS
        }
        _ => false,
    }
}

/// A time on the position bar: "3:07", or "1:02:07" when `long` (the
/// track is an hour or more, so both ends read alike). Negative is 0.
pub fn time_text(ms: i64, long: bool) -> String {
    let s = ms.max(0) / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if long || h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// The position bar's two ends: time played and the track's length.
pub fn time_pair(tl: &Timeline, pos: i64) -> (String, String) {
    let len = tl.length_ms();
    let long = len >= 3_600_000;
    (time_text(pos - tl.start_ms, long), time_text(len, long))
}

/// The position for pointer `x` on a bar drawn from `x0` to `x1` (right to
/// left: the start is at the right end).
pub fn seek_at(rtl: bool, x: f32, x0: f32, x1: f32, tl: &Timeline) -> i64 {
    tl.at_ratio(crate::i18n::mirror::slider_value(rtl, x, x0, x1))
}

/// Milliseconds until the time played (shown in whole seconds) next
/// changes, at `rate`: the card's tick lands just after it.
pub fn ms_to_next_second(pos_from_start: i64, rate: f64) -> u32 {
    let rate = if rate.is_finite() && rate > 0.0 { rate } else { 1.0 };
    let left = 1000 - pos_from_start.rem_euclid(1000);
    ((left as f64 / rate).ceil() as u32).clamp(50, 1000)
}

/// A short battery notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    /// The charger was just plugged in (percent).
    Charging(u8),
    /// The battery just fell under a warning level (percent).
    Low(u8),
}

/// A countdown: the focus timer (crate::focus_timer), or a plain one.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Timer {
    /// "" = the default name ("Timer"; the focus timer names its phase).
    pub label: String,
    pub remaining_s: u32,
    pub running: bool,
    /// The focus timer's state (its phase, the ring's whole, today's count).
    pub focus: Option<crate::focus_timer::View>,
}

impl Timer {
    /// The focus timer as the island shows it.
    pub fn of_focus(v: crate::focus_timer::View) -> Timer {
        let remaining_s = ((v.left_ms.max(0) + 999) / 1000) as u32;
        Timer { label: String::new(), remaining_s, running: v.running, focus: Some(v) }
    }

    /// The share of the phase still to go (the ring), 0..1.
    pub fn ratio(&self) -> f32 {
        match &self.focus {
            Some(v) if v.total_ms > 0 => (v.left_ms as f64 / v.total_ms as f64).clamp(0.0, 1.0) as f32,
            _ => 0.0,
        }
    }

    /// The focus timer's break is over (a short notice, no countdown).
    pub fn over(&self) -> bool {
        self.focus.as_ref().is_some_and(|v| v.phase == 2)
    }

}

/// What an app is using right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Sensor {
    Camera,
    Microphone,
    Screen,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Use {
    pub sensor: Sensor,
    /// The app's identity (package family name, or full exe path): uses
    /// are merged and grouped by it, never by the name shown.
    pub id: String,
    /// Its name as shown.
    pub app: String,
}

/// A screenshot just taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shot {
    pub path: String,
}

/// One live activity (several jobs or sensor uses merge into one).
#[derive(Clone, Debug, PartialEq)]
pub enum Activity {
    Progress(Vec<Job>),
    Media(Track),
    Battery(Power),
    Timer(Timer),
    Privacy(Vec<Use>),
    Screenshot(Shot),
}

impl Activity {
    pub fn kind(&self) -> Kind {
        match self {
            Activity::Progress(_) => Kind::Progress,
            Activity::Media(_) => Kind::Media,
            Activity::Battery(_) => Kind::Battery,
            Activity::Timer(_) => Kind::Timer,
            Activity::Privacy(_) => Kind::Privacy,
            Activity::Screenshot(_) => Kind::Screenshot,
        }
    }
}

/// Everything the island hears about, as it is now.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Inputs {
    pub jobs: Vec<Job>,
    pub track: Option<Track>,
    pub power: Option<Power>,
    pub timer: Option<Timer>,
    pub uses: Vec<Use>,
    pub shot: Option<Shot>,
}

/// The per-activity switches (Settings > Top bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Enabled {
    pub progress: bool,
    pub media: bool,
    pub battery: bool,
    pub privacy: bool,
}

impl Enabled {
    pub const ALL: Enabled = Enabled { progress: true, media: true, battery: true, privacy: true };
}

/// What is live, highest priority first. `keep_paused`: a paused track
/// stays while the card is open (its Play button must not vanish under the
/// pointer), or during the caller's five-second pause grace.
pub fn live(inp: &Inputs, on: Enabled, keep_paused: bool) -> Vec<Activity> {
    let mut out = Vec::new();
    if on.progress {
        // One entry per app (the newest value of a key wins). Every job the
        // taskbar reports is live, at 0 % too (the dock tells "progress" from
        // "none" by its state, not its value).
        let mut jobs: Vec<Job> = Vec::new();
        for j in inp.jobs.iter() {
            let r = if j.ratio.is_finite() { j.ratio.clamp(0.0, 1.0) } else { 0.0 };
            let j = Job { ratio: r, ..j.clone() };
            match jobs.iter_mut().find(|x| x.key == j.key) {
                Some(x) => *x = j,
                None => jobs.push(j),
            }
        }
        if !jobs.is_empty() {
            out.push(Activity::Progress(jobs));
        }
    }
    if on.media {
        if let Some(tr) = inp.track.as_ref().filter(|tr| tr.playing || keep_paused) {
            out.push(Activity::Media(tr.clone()));
        }
    }
    if on.battery {
        if let Some(p) = inp.power {
            out.push(Activity::Battery(p));
        }
    }
    if let Some(tm) = &inp.timer {
        out.push(Activity::Timer(tm.clone()));
    }
    if on.privacy && !inp.uses.is_empty() {
        let mut u = inp.uses.clone();
        u.sort();
        u.dedup();
        out.push(Activity::Privacy(u));
    }
    if let Some(s) = &inp.shot {
        out.push(Activity::Screenshot(s.clone()));
    }
    out.sort_by_key(|a| a.kind());
    out
}

/// The overall share of several jobs (their mean).
pub fn overall(jobs: &[Job]) -> f32 {
    if jobs.is_empty() {
        return 0.0;
    }
    (jobs.iter().map(|j| j.ratio.clamp(0.0, 1.0)).sum::<f32>() / jobs.len() as f32).clamp(0.0, 1.0)
}

pub fn percent(r: f32) -> u8 {
    // Floored (99.6 % is not done), with room for float noise (0.42 * 100).
    (r.clamp(0.0, 1.0) * 100.0 + 0.001).floor().min(100.0) as u8
}

/// The apps using sensors, grouped by identity: (name, camera, microphone,
/// screen), in name order. Two different apps with the same name stay two,
/// told apart by their folder or package ("Zoom (bin)", "Zoom (old)").
pub fn by_app(uses: &[Use]) -> Vec<(String, bool, bool, bool)> {
    let mut groups: Vec<(String, String, bool, bool, bool)> = Vec::new();
    for u in uses {
        let i = match groups.iter().position(|g| g.0.eq_ignore_ascii_case(&u.id)) {
            Some(i) => i,
            None => {
                groups.push((u.id.clone(), u.app.clone(), false, false, false));
                groups.len() - 1
            }
        };
        match u.sensor {
            Sensor::Camera => groups[i].2 = true,
            Sensor::Microphone => groups[i].3 = true,
            Sensor::Screen => groups[i].4 = true,
        }
    }
    let names: Vec<String> = groups.iter().map(|g| g.1.to_lowercase()).collect();
    let mut out: Vec<(String, bool, bool, bool)> = groups
        .iter()
        .map(|(id, app, c, m, s)| {
            let clash = names.iter().filter(|n| **n == app.to_lowercase()).count() > 1;
            let name = if clash { format!("{app} ({})", id_hint(id)) } else { app.clone() };
            (name, *c, *m, *s)
        })
        .collect();
    out.sort_by_cached_key(|a| a.0.to_lowercase());
    out
}

/// What tells an app apart from another of the same name: its folder (an
/// exe path, with '#' or '\'), else its whole identifier.
pub fn id_hint(id: &str) -> String {
    let parts: Vec<&str> = id.split(['#', '\\', '/']).filter(|p| !p.is_empty()).collect();
    match parts.len() {
        n if n >= 2 => parts[n - 2].to_string(),
        _ => id.to_string(),
    }
}

/// The privacy color of a set of uses: the camera's green wins over the
/// microphone's orange, the screen's purple comes last.
pub fn privacy_tone(uses: &[Use]) -> Tone {
    if uses.iter().any(|u| u.sensor == Sensor::Camera) {
        Tone::Green
    } else if uses.iter().any(|u| u.sensor == Sensor::Microphone) {
        Tone::Orange
    } else {
        Tone::Purple
    }
}

/// The colored dots and lights the island uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    /// The camera.
    Green,
    /// The microphone.
    Orange,
    /// Screen capture.
    Purple,
    /// Low battery.
    Red,
    /// No hue: the pill's ink (what only informs: media, charging,
    /// progress, timers; the design batch keeps hues for what warns).
    Ink,
}

/// Pure: the tone `a` is shown in: privacy's dot and a low battery keep
/// their color (they warn); everything else is the pill's ink (achromatic
/// chrome; the accent is only for what is on or picked).
pub fn shown_tone(a: &Activity) -> Tone {
    match a {
        Activity::Privacy(u) => privacy_tone(u),
        Activity::Battery(Power::Low(_)) => Tone::Red,
        _ => Tone::Ink,
    }
}

/// "Camera on", "Microphone on", "Camera and microphone on", "Screen
/// capture on" for one app's uses.
pub fn sensor_text(camera: bool, mic: bool, screen: bool) -> &'static str {
    t(match (camera, mic) {
        (true, true) => "island.camera_mic",
        (true, false) => "island.camera",
        (false, true) => "island.mic",
        (false, false) if screen => "island.screen",
        _ => "island.camera",
    })
}

/// "Camera in use by Zoom" (screen readers, the card).
pub fn sensor_sentence(app: &str, camera: bool, mic: bool, screen: bool) -> String {
    let key = match (camera, mic) {
        (true, true) => "island.a11y.camera_mic",
        (true, false) => "island.a11y.camera",
        (false, true) => "island.a11y.mic",
        (false, false) if screen => "island.a11y.screen",
        _ => "island.a11y.camera",
    };
    tf(key, &[("app", app)])
}

/// The name a job goes by: "Copying files" for File Explorer, else the app.
pub fn job_title(j: &Job) -> String {
    if j.files { t("island.files").to_string() } else { j.name.clone() }
}

/// m:ss (or h:mm:ss).
pub fn clock(s: u32) -> String {
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// The pill's words for an activity: (main text, trailing text). The main
/// text is cut later by its real width; this is what it would say.
pub fn pill_words(a: &Activity) -> (String, String) {
    match a {
        Activity::Progress(jobs) => {
            let p = percent(overall(jobs)).to_string();
            let main = match jobs.as_slice() {
                [j] => job_title(j),
                [j, rest @ ..] => tf("island.more", &[("what", &job_title(j)), ("n", &rest.len().to_string())]),
                [] => String::new(),
            };
            (main, tf("control.percent", &[("p", &p)]))
        }
        Activity::Media(tr) => (track_line(tr), String::new()),
        Activity::Battery(Power::Charging(p)) => (tf("island.charging", &[("p", &p.to_string())]), String::new()),
        Activity::Battery(Power::Low(p)) => (tf("island.battery_low", &[("p", &p.to_string())]), String::new()),
        Activity::Timer(tm) if tm.over() => (timer_label(tm), String::new()),
        Activity::Timer(tm) => (timer_label(tm), clock(tm.remaining_s)),
        Activity::Privacy(uses) => {
            let groups = by_app(uses);
            let (c, m, s) = groups.iter().fold((false, false, false), |acc, g| (acc.0 || g.1, acc.1 || g.2, acc.2 || g.3));
            let apps: Vec<&str> = groups.iter().map(|g| g.0.as_str()).collect();
            (format!("{} — {}", sensor_text(c, m, s), apps.join(", ")), String::new())
        }
        Activity::Screenshot(_) => (t("island.screenshot").to_string(), String::new()),
    }
}

pub fn timer_label(tm: &Timer) -> String {
    let name = match &tm.focus {
        Some(v) => t(match v.phase {
            0 => "timer.focus",
            1 => "timer.break",
            _ => "timer.break_over",
        })
        .to_string(),
        None if tm.label.trim().is_empty() => t("island.timer").to_string(),
        None => tm.label.clone(),
    };
    if tm.focus.is_some() && !tm.running && !tm.over() {
        tf("timer.paused_label", &[("what", &name)])
    } else {
        name
    }
}

/// "Song — Artist" (just the title without an artist; the app's own
/// "Now Playing" without a title).
pub fn track_line(tr: &Track) -> String {
    match (tr.title.trim(), tr.artist.trim()) {
        ("", _) => t("control.now_playing").to_string(),
        (title, "") => title.to_string(),
        (title, artist) => format!("{title} — {artist}"),
    }
}

/// The media session comes from a web browser (its app id: Chrome, Edge,
/// Firefox, Brave, Opera, Vivaldi). Only then can a title be a tab's title.
pub fn is_browser(app: &str) -> bool {
    let a = app.to_ascii_lowercase();
    // Firefox's app id is a hash of its install folder; the release one.
    ["chrome", "msedge", "microsoftedge", "firefox", "308046b0af4a39cb", "brave", "opera", "vivaldi"].iter().any(|b| a.contains(b))
}

/// Text from a media session on one line: line breaks, tabs and runs of
/// white space become one space (a video title written over two lines
/// drew its first line above the pill, cut off).
pub fn one_line(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// A media title as a person would write it. Browsers without media
/// metadata report the YouTube tab's title: "(3) Song - YouTube". Only for
/// a browser's session whose title ends in " - YouTube" (or " - YouTube
/// Music") is that suffix dropped, and then an unread count in front:
/// exactly "(N) " with 1-3 digits. Everything else is kept as it came
/// ("(500) Days of Summer" from a player stays).
pub fn clean_title(s: &str, app: &str) -> String {
    let s = &one_line(s);
    if !is_browser(app) {
        return s.to_string();
    }
    let mut head = None;
    'site: for site in ["YouTube Music", "YouTube"] {
        for dash in [" - ", " – ", " — ", " | "] {
            if let Some(h) = s.strip_suffix(site).and_then(|h| h.strip_suffix(dash)) {
                head = Some(h.trim());
                break 'site;
            }
        }
    }
    let Some(head) = head.filter(|h| !h.is_empty()) else { return s.to_string() };
    if let Some((n, rest)) = head.strip_prefix('(').and_then(|r| r.split_once(") ")) {
        let rest = rest.trim_start();
        if (1..=3).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()) && !rest.is_empty() {
            return rest.to_string();
        }
    }
    head.to_string()
}

/// A media artist without YouTube's " - Topic" (its automatic channels),
/// from a browser's session only.
pub fn clean_artist(s: &str, app: &str) -> String {
    let s = &one_line(s);
    match s.strip_suffix(" - Topic") {
        Some(a) if is_browser(app) && !a.trim().is_empty() => a.trim().to_string(),
        _ => s.to_string(),
    }
}

/// Is a new placement of the pill eased from the last one? Only when there
/// was a last one (the pill was placed, its background and clip are in
/// the same place) and the island moves. A first placement (new, or back
/// from no room) is set at once.
pub fn layout_animates(was_placed: bool, motion: bool) -> bool {
    was_placed && motion
}

/// What a screen reader hears for an activity.
pub fn spoken(a: &Activity) -> String {
    match a {
        Activity::Progress(jobs) => jobs
            .iter()
            .map(|j| tf("island.a11y.progress", &[("what", &job_title(j)), ("p", &percent(j.ratio).to_string())]))
            .collect::<Vec<_>>()
            .join(crate::i18n::t("a11y.list_sep")),
        Activity::Media(tr) => {
            let key = if tr.playing { "island.a11y.playing" } else { "island.a11y.paused" };
            tf(key, &[("what", &track_line(tr))])
        }
        Activity::Privacy(uses) => by_app(uses).iter().map(|(app, c, m, s)| sensor_sentence(app, *c, *m, *s)).collect::<Vec<_>>().join(crate::i18n::t("a11y.list_sep")),
        Activity::Timer(tm) if tm.over() => timer_label(tm),
        Activity::Timer(tm) if tm.focus.is_some() => tf("timer.a11y.left", &[("what", &timer_label(tm)), ("time", &clock(tm.remaining_s))]),
        Activity::Timer(tm) => format!("{} {}", timer_label(tm), clock(tm.remaining_s)),
        _ => pill_words(a).0,
    }
}

// ---------------------------------------------------------------- text

/// Cuts `s` to at most `max` characters, ending in "…" when cut. Never
/// splits a letter from the marks that follow it, and drops spaces and
/// dashes left dangling before the "…".
pub fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    // Room for the ellipsis.
    let mut n = max - 1;
    // Back off combining marks and joiners: keep a letter with its marks.
    while n > 0 && n < chars.len() && joins(chars[n]) {
        n -= 1;
    }
    let mut out: String = chars[..n].iter().collect();
    let trimmed = out.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '—' | '–' | '-' | ',' | '·')).len();
    out.truncate(trimmed);
    out.push('…');
    out
}

/// Belongs to the character before it (combining marks, variation
/// selectors, zero-width joiners, low surrogate-free emoji modifiers).
fn joins(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0x200D | 0x1F3FB..=0x1F3FF | 0xE0100..=0xE01EF)
}

// ---------------------------------------------------------------- battery

/// The warning levels (percent): a notice each time the battery falls
/// under one while on battery.
pub const LOW_LEVELS: [u8; 2] = [20, 10];
/// How long the notices stay (ms).
pub const CHARGING_MS: u32 = 3000;
pub const LOW_MS: u32 = 6000;

/// The notice a battery change brings, if any. `before` None: the first
/// reading (no notice for how things already were).
pub fn battery_notice(before: Option<(u8, bool)>, now: (u8, bool)) -> Option<Power> {
    let (was, was_charging) = before?;
    let (p, charging) = now;
    if charging && !was_charging {
        return Some(Power::Charging(p));
    }
    if !charging && LOW_LEVELS.iter().any(|&l| was >= l && p < l) {
        return Some(Power::Low(p));
    }
    None
}

pub fn notice_ms(p: Power) -> u32 {
    match p {
        Power::Charging(_) => CHARGING_MS,
        Power::Low(_) => LOW_MS,
    }
}

// ---------------------------------------------------------------- states

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Hidden,
    Pill,
    /// The card is out. `pinned`: opened by a click or the keyboard (it
    /// has the keyboard and stays until Esc, a click outside or another
    /// click on the pill); else by hovering (it goes when the pointer
    /// leaves).
    Expanded { pinned: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Something is live and the island may show (on, bar not hidden).
    Shown(bool),
    /// The pointer rested on the pill (HOVER_MS).
    Hover,
    /// The pointer left the pill and the card.
    Leave,
    Click,
    /// Esc, a click outside, the card losing the keyboard.
    Dismiss,
}

/// Hover this long on the pill before the card comes out: the one wait
/// the pointer has anywhere (a label's and a tooltip's: TIP_DELAY_MS; it
/// was 250).
pub const HOVER_MS: u32 = crate::motion::TIP_DELAY_MS as u32;

/// The bar moved to the other screen edge (topbar.position): an open card
/// is dismissed, pinned or not, so the island is a pill again and a
/// hover or a click opens the card from the new place.
pub fn after_bar_moved(p: Phase) -> Phase {
    step(p, Event::Dismiss)
}

pub fn step(p: Phase, e: Event) -> Phase {
    use Phase::*;
    match (p, e) {
        (_, Event::Shown(false)) => Hidden,
        (Hidden, Event::Shown(true)) => Pill,
        (p, Event::Shown(true)) => p,
        (Hidden, _) => Hidden,
        (Pill, Event::Hover) => Expanded { pinned: false },
        (Pill, Event::Click) => Expanded { pinned: true },
        (Pill, _) => Pill,
        (Expanded { pinned: false }, Event::Click) => Expanded { pinned: true },
        (Expanded { pinned: true }, Event::Click) => Pill,
        (Expanded { pinned: false }, Event::Leave) => Pill,
        (Expanded { .. }, Event::Dismiss) => Pill,
        (e @ Expanded { .. }, _) => e,
    }
}

// ---------------------------------------------------------------- geometry

/// The pill's inset from the bar's top and bottom (DIP: the
/// Top bar's pill is the bar's height less 6, 20 on the 26 bar:
/// a hair slimmer than the cells' fill, BAR_INSET), its least and most
/// width.
pub const PILL_INSET: f32 = crate::theme::metrics::size::BAR_INSET + 1.0;
pub const PILL_MIN_W: f32 = 64.0;
pub const PILL_MAX_W: f32 = 400.0;
/// Space kept to the cells on either side.
pub const PILL_GAP: f32 = 12.0;
/// Inside the pill (DIP): the padding at its ends, the gap between parts.
pub const PAD: f32 = 12.0;
pub const GAP: f32 = 8.0;
/// The title's most width (DIP); longer, it fades out (or ends in "…").
pub const TITLE_MAX: f32 = 280.0;
/// A title cut narrower than this is left out (the rest still shows).
pub const TEXT_MIN: f32 = 24.0;
/// The tallest bar (config clamps the bar to 20..48) and the least pill.
pub const BAR_MAX_H: f32 = 48.0;
pub const PILL_MIN_H: f32 = 16.0;
/// The leading picture keeps this far from the pill's top and bottom.
pub const LEAD_INSET: f32 = 2.0;
/// A cut title fades out over its last this many DIP (high contrast: "…").
pub const FADE: f32 = 20.0;

/// The pill's height on a bar `bar_h` tall: the bar less PILL_INSET at its
/// top and bottom (at least PILL_MIN_H).
pub fn pill_height(bar_h: f32) -> f32 {
    (bar_h - 2.0 * PILL_INSET).max(PILL_MIN_H)
}

/// Pure: the pill's height at rest on a bar `bar_h` tall on a display
/// scaled `scale`: pill_height on whole device pixels. Its width is too
/// (TopBar::island_layout snaps place()'s), its left end (pill_left) and
/// its root (rest_at), so every straight side of its one-pixel rim lies on
/// a pixel's middle at 125 / 175 % as at 100 % (2026-11-11 last-two review
/// P2: a 22 DIP pill is 27.5 px at 125 %, its bottom rim straddled two
/// rows). Only where it rests: easing from one rest to the next it takes
/// the sizes between.
pub fn rest_height(bar_h: f32, scale: f32) -> f32 {
    snap(pill_height(bar_h), scale)
}

/// Pure: where the pill rests on a bar `bar_h` tall at `scale`, given
/// place()'s span (`x`, `w`): its root (its middle on a device pixel, its
/// top centered on whole pixels: topbar::baseline::cell_top) and its size
/// (on whole device pixels: rest_height): (cx, top, w, h).
/// TopBar::island_layout's.
pub fn rest_at(x: f32, w: f32, bar_h: f32, scale: f32) -> (f32, f32, f32, f32) {
    let (w, h) = (snap(w, scale), rest_height(bar_h, scale));
    (snap(x + w / 2.0, scale), crate::topbar::baseline::cell_top(bar_h, h, scale), w, h)
}

/// Pure: the pill's left end from its root (x 0, the pill's middle) for a
/// pill `w` wide at `scale`: on a device pixel.
pub fn pill_left(w: f32, scale: f32) -> f32 {
    snap(-w / 2.0, scale)
}

/// Pure: the pill on screen (px: left, top, right, bottom) on a bar whose
/// top left is at `bar` px, its root at (`cx`, `top`) DIP on the bar, `w` x
/// `h` DIP: where frame puts it, so the card's plate starts on the pill as
/// drawn (card::pill_shape).
pub fn screen_rect(bar: (i32, i32), cx: f32, top: f32, w: f32, h: f32, scale: f32) -> (i32, i32, i32, i32) {
    let left = bar.0 + ((cx + pill_left(w, scale)) * scale).round() as i32;
    let top = bar.1 + (top * scale).round() as i32;
    (left, top, left + (w * scale).round() as i32, top + (h * scale).round() as i32)
}

/// The leading picture's side in a pill `h` tall: as drawn (`side`), or
/// smaller to keep LEAD_INSET above and below it (a short bar).
pub fn lead_fit(side: f32, h: f32) -> f32 {
    if side <= 0.0 { 0.0 } else { side.min(h - 2.0 * LEAD_INSET).max(1.0) }
}

/// Where a title `w` DIP wide fades out, in its own coordinates (from, to):
/// over its last FADE DIP, never past its end (what follows it, the music
/// bars, is not under the mask).
pub fn fade_span(w: f32) -> (f32, f32) {
    ((w - FADE).max(0.0), w)
}

/// Where the pill goes on a bar `width` wide whose visible cells are
/// `cells` ((x, w), DIP): centered on the bar if it fits there, else
/// centered as near as it can in the free span around the middle, `want`
/// wide or less. None when the span is narrower than `need` (what the pill
/// holds besides its title) or PILL_MIN_W. Works the same mirrored (right
/// to left the cells are mirrored, so is the result).
pub fn place(width: f32, cells: &[(f32, f32)], want: f32, need: f32) -> Option<(f32, f32)> {
    let mid = width / 2.0;
    let mut lo: f32 = 0.0;
    let mut hi: f32 = width;
    for &(x, w) in cells.iter().filter(|c| c.1 > 0.0) {
        if x + w / 2.0 < mid {
            lo = lo.max(x + w);
        } else {
            hi = hi.min(x);
        }
    }
    let (lo, hi) = (lo + PILL_GAP, hi - PILL_GAP);
    let room = hi - lo;
    if room < PILL_MIN_W.max(need) {
        return None;
    }
    let w = want.clamp(PILL_MIN_W, PILL_MAX_W).min(room);
    let x = (mid - w / 2.0).clamp(lo, hi - w);
    Some((x, w))
}

/// A rectangle (DIP).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[cfg(test)]
impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    /// `o` lies within (a hundredth of a DIP of rounding allowed).
    pub fn contains(&self, o: &Rect) -> bool {
        const E: f32 = 0.01;
        o.x >= self.x - E && o.y >= self.y - E && o.right() <= self.right() + E && o.bottom() <= self.bottom() + E
    }
}

/// What the pill holds, measured (DIP; a width of 0: that part is not
/// there).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Content {
    /// The leading picture (album art, icon, glyph): a square.
    pub lead: f32,
    /// The title as drawn, its height.
    pub text: f32,
    pub text_h: f32,
    /// The trailing part (music bars, a ring, a time), its height.
    pub trail: f32,
    pub trail_h: f32,
    /// Dots for the other live activities.
    pub dots: usize,
    /// The title's baseline from its top: the trailing part stands on it
    /// (the music bars); None: centered.
    pub baseline: Option<f32>,
}

fn dots_w(n: usize) -> f32 {
    if n == 0 { 0.0 } else { n as f32 * DOT_STEP - (DOT_STEP - DOT) }
}

/// The parts in reading order with a GAP between those that are there.
fn row(c: &Content) -> [f32; 4] {
    [c.lead, c.text, c.trail, dots_w(c.dots)]
}

fn row_w(parts: &[f32]) -> f32 {
    let on: Vec<f32> = parts.iter().copied().filter(|w| *w > 0.0).collect();
    on.iter().sum::<f32>() + GAP * on.len().saturating_sub(1) as f32
}

/// The content's own width (padding included, not clamped).
pub fn content_width(c: &Content) -> f32 {
    2.0 * PAD + row_w(&row(c))
}

/// The pill's width for this content (DIP, within its least and most).
pub fn pill_width(c: &Content) -> f32 {
    content_width(c).clamp(PILL_MIN_W, PILL_MAX_W)
}

/// What the pill needs at least: everything but the title.
pub fn pill_need(c: &Content) -> f32 {
    content_width(&Content { text: 0.0, ..*c }).max(PILL_MIN_W)
}

/// The most the title may take in a pill `w` wide with the other parts of
/// `c` (0: under TEXT_MIN, it is left out).
pub fn text_room(c: &Content, w: f32) -> f32 {
    let others = row_w(&[c.lead, c.trail, dots_w(c.dots)]);
    let room = w - 2.0 * PAD - others - if others > 0.0 { GAP } else { 0.0 };
    if room < TEXT_MIN { 0.0 } else { room.min(TITLE_MAX) }
}

#[cfg(test)]
/// The width a title `full` DIP wide is drawn at when it may take `max`,
/// at `scale` (crate::gfx::Gfx::text_line's arithmetic: whole pixels).
pub fn drawn_width(full: f32, max: f32, scale: f32) -> f32 {
    let room = (max * scale).floor().max(1.0);
    (full * scale).min(room).ceil().max(1.0) / scale
}

/// Everything of the pill, relative to its root visual (x 0 is the pill's
/// center, y 0 its top). DIP.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    /// The pill: also the content's clip, which shapes the fill.
    pub pill: Rect,
    /// The fill: a rectangle past the widest, tallest pill (the clip cuts
    /// it to the pill; it never moves while the pill eases).
    pub fill: Rect,
    /// The border's shape visual (from the pill's left end, as wide as the
    /// widest pill) and the border in it (its own coordinates). A shape is
    /// drawn only within its visual: the border must lie inside.
    pub edge_box: Rect,
    pub edge: Rect,
    /// The shadow's visual: the pill.
    pub shadow: Rect,
    pub lead: Rect,
    pub text: Rect,
    pub trail: Rect,
    pub dots: Vec<Rect>,
}

/// Pure: where the pill's fall runs in the fill's own DIP (`f.fill` is
/// taller than the pill and starts above it): (its top, its bottom).
pub fn fall_span(f: &Frame) -> (f32, f32) {
    (f.pill.y - f.fill.y, f.pill.y - f.fill.y + f.pill.h)
}

/// Lays out a pill `w` by `h` holding `c`, in reading order (mirrored right
/// to left: the lead at the right end, the dots at the left). Content
/// narrower than the pill (its least width) is centered. Every part's
/// place is snapped to the device pixels of `scale` (the root sits on a
/// whole pixel too: TopBar::island_layout), mirrored or not, so a text
/// surface's whole-pixel baseline stays whole on screen.
pub fn frame(rtl: bool, w: f32, h: f32, c: &Content, scale: f32) -> Frame {
    let px = |v: f32| snap(v, scale);
    let pill = Rect { x: pill_left(w, scale), y: 0.0, w, h };
    let widest = w.max(PILL_MAX_W);
    let fill = Rect { x: -widest / 2.0 - 2.0, y: -2.0, w: widest + 4.0, h: h.max(BAR_MAX_H) + 4.0 };
    let edge_box = Rect { x: pill.x, y: 0.0, w: widest, h: h.max(BAR_MAX_H) };
    let edge = Rect { x: 0.5, y: 0.5, w: w - 1.0, h: h - 1.0 };
    let inner = row_w(&row(c));
    let slack = (w - 2.0 * PAD - inner).max(0.0);
    let mut x = px(pill.x + PAD + slack / 2.0);
    let mut at = |pw: f32| -> f32 {
        if pw <= 0.0 {
            return x;
        }
        let here = x;
        x = px(x + pw + GAP);
        here
    };
    let (lx, tx, rx, dx) = (at(c.lead), at(c.text), at(c.trail), at(dots_w(c.dots)));
    let mid = |hh: f32| px((h - hh) / 2.0);
    let text = Rect { x: tx, y: mid(c.text_h), w: c.text, h: c.text_h };
    let trail_y = match c.baseline {
        Some(b) if c.text > 0.0 => px(text.y + b - c.trail_h).clamp(0.0, (h - c.trail_h).max(0.0)),
        _ => mid(c.trail_h),
    };
    let m = |r: Rect| if rtl { Rect { x: px(-r.x - r.w), ..r } } else { r };
    Frame {
        pill,
        fill,
        edge_box,
        edge,
        shadow: pill,
        lead: m(Rect { x: lx, y: mid(c.lead), w: c.lead, h: c.lead }),
        text: m(text),
        trail: m(Rect { x: rx, y: trail_y, w: c.trail, h: c.trail_h }),
        dots: (0..c.dots).map(|i| m(Rect { x: dx + i as f32 * DOT_STEP, y: mid(DOT), w: DOT, h: DOT })).collect(),
    }
}

/// Pure: `v` DIP on the nearest device pixel at `scale`.
pub fn snap(v: f32, scale: f32) -> f32 {
    let s = if scale > 0.0 { scale } else { 1.0 };
    (v * s).round() / s
}

/// Dots for the other live activities: size and step (DIP).
pub const DOT: f32 = 6.0;
pub const DOT_STEP: f32 = 8.0;
/// At most this many dots (more activities than that cannot be live).
pub const MAX_DOTS: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    fn job(key: &str, r: f32) -> Job {
        Job { key: key.into(), name: key.into(), files: false, ratio: r, hwnd: 0 }
    }

    #[test]
    fn priority_and_merging() {
        let _l = crate::i18n::test_lock("en");
        let inp = Inputs {
            jobs: vec![job("a", 0.5), job("b", 0.0), job("a", 0.7), job("c", 1.5)],
            track: Some(Track { title: "Song".into(), artist: "Artist".into(), playing: true, ..Default::default() }),
            power: Some(Power::Charging(64)),
            timer: Some(Timer { label: String::new(), remaining_s: 90, running: true, focus: None }),
            uses: vec![
                Use { sensor: Sensor::Microphone, id: "z".into(), app: "Zoom".into() },
                Use { sensor: Sensor::Camera, id: "z".into(), app: "Zoom".into() },
                Use { sensor: Sensor::Camera, id: "z".into(), app: "Zoom".into() },
            ],
            shot: Some(Shot { path: "a.png".into() }),
        };
        let l = live(&inp, Enabled::ALL, false);
        let kinds: Vec<Kind> = l.iter().map(|a| a.kind()).collect();
        assert_eq!(kinds, [Kind::Progress, Kind::Media, Kind::Battery, Kind::Timer, Kind::Privacy, Kind::Screenshot]);
        // One entry per app, the newest value wins, a real 0 % stays (and
        // counts in the mean), over 1 is 1.
        let Activity::Progress(jobs) = &l[0] else { panic!() };
        assert_eq!(jobs.iter().map(|j| (j.key.as_str(), j.ratio)).collect::<Vec<_>>(), [("a", 0.7), ("b", 0.0), ("c", 1.0)]);
        assert_eq!(percent(overall(jobs)), 56);
        let only_zero = Inputs { jobs: vec![job("b", 0.0)], ..Default::default() };
        assert_eq!(live(&only_zero, Enabled::ALL, false).len(), 1, "a job at 0 % is live");
        // Duplicate uses merge; camera and microphone of one app read as one.
        let Activity::Privacy(u) = &l[4] else { panic!() };
        assert_eq!(u.len(), 2);
        assert_eq!(by_app(u), [("Zoom".to_string(), true, true, false)]);
        assert_eq!(pill_words(&l[4]).0, "Camera and microphone on — Zoom");
        assert_eq!(privacy_tone(u), Tone::Green);
        // Switches.
        let off = Enabled { progress: false, media: false, battery: false, privacy: false };
        let kinds: Vec<Kind> = live(&inp, off, false).iter().map(|a| a.kind()).collect();
        assert_eq!(kinds, [Kind::Timer, Kind::Screenshot], "no switch for the hooks");
        // A paused track is live only while the card holds it.
        let paused = Inputs { track: Some(Track { playing: false, title: "x".into(), ..Default::default() }), ..Default::default() };
        assert!(live(&paused, Enabled::ALL, false).is_empty());
        assert_eq!(live(&paused, Enabled::ALL, true).len(), 1);
        assert!(live(&Inputs::default(), Enabled::ALL, true).is_empty());
    }

    #[test]
    fn privacy_groups_by_identity_not_name() {
        let _l = crate::i18n::test_lock("en");
        let u = |s, id: &str| Use { sensor: s, id: id.into(), app: "Zoom".into() };
        // Two different Zoom.exe: one uses the camera, the other the
        // microphone. Not one app using both.
        let g = by_app(&[u(Sensor::Camera, r"C:#Apps#Zoom#Zoom.exe"), u(Sensor::Microphone, r"D:#Old#Zoom.exe")]);
        assert_eq!(g, [("Zoom (Old)".to_string(), false, true, false), ("Zoom (Zoom)".to_string(), true, false, false)]);
        // The same app twice merges.
        let g = by_app(&[u(Sensor::Camera, "C:#Z.exe"), u(Sensor::Microphone, "c:#z.exe")]);
        assert_eq!(g, [("Zoom".to_string(), true, true, false)]);
        assert_eq!(id_hint("MSTeams_8wekyb3d8bbwe"), "MSTeams_8wekyb3d8bbwe");
    }

    #[test]
    fn pill_words_and_speech() {
        let _l = crate::i18n::test_lock("en");
        let files = Job { files: true, ..job("explorer", 0.42) };
        assert_eq!(pill_words(&Activity::Progress(vec![files.clone()])), ("Copying files".into(), "42%".into()));
        assert_eq!(spoken(&Activity::Progress(vec![files.clone()])), "Copying files 42%");
        let two = Activity::Progress(vec![files, job("Edge", 0.2)]);
        assert_eq!(pill_words(&two).0, "Copying files and 1 more");
        let tr = Track { title: "Song".into(), artist: "Artist".into(), playing: true, ..Default::default() };
        assert_eq!(spoken(&Activity::Media(tr.clone())), "Playing: Song — Artist");
        assert_eq!(track_line(&Track { title: "Song".into(), ..Default::default() }), "Song");
        assert_eq!(track_line(&Track::default()), "Now Playing");
        let cam = Activity::Privacy(vec![Use { sensor: Sensor::Camera, id: "z".into(), app: "Zoom".into() }]);
        assert_eq!(spoken(&cam), "Camera in use by Zoom");
        assert_eq!(pill_words(&cam).0, "Camera on — Zoom");
        assert_eq!(pill_words(&Activity::Battery(Power::Charging(64))).0, "Charging 64%");
        assert_eq!(pill_words(&Activity::Battery(Power::Low(15))).0, "Battery low 15%");
        assert_eq!(pill_words(&Activity::Timer(Timer { label: String::new(), remaining_s: 3725, running: true, focus: None })), ("Timer".into(), "1:02:05".into()));
        assert_eq!(clock(59), "0:59");
        assert_eq!(shown_tone(&Activity::Privacy(vec![Use { sensor: Sensor::Microphone, id: "a".into(), app: "a".into() }])), Tone::Orange);
    }

    #[test]
    fn the_focus_timer_on_the_island() {
        let _l = crate::i18n::test_lock("en");
        let v = crate::focus_timer::View { phase: 0, left_ms: 18 * 60_000 + 41_200, total_ms: 25 * 60_000, running: true, deadline: 1, today: 2, no_dnd: false };
        let tm = Timer::of_focus(v.clone());
        assert_eq!(tm.remaining_s, 18 * 60 + 42, "seconds rounded up");
        let a = Activity::Timer(tm.clone());
        assert_eq!(pill_words(&a), ("Focus".into(), "18:42".into()));
        assert_eq!(shown_tone(&a), Tone::Ink);
        assert!((tm.ratio() - (18.0 * 60.0 + 41.2) / 1500.0).abs() < 1e-4);
        assert_eq!(spoken(&a), "Focus, 18:42 left");
        // Paused: says so; the break is ink too (it only informs); the end
        // has no clock.
        let paused = Timer::of_focus(crate::focus_timer::View { running: false, deadline: 0, ..v.clone() });
        assert_eq!(pill_words(&Activity::Timer(paused)).0, "Focus (paused)");
        let brk = Activity::Timer(Timer::of_focus(crate::focus_timer::View { phase: 1, ..v.clone() }));
        assert_eq!((pill_words(&brk).0.as_str(), shown_tone(&brk)), ("Break", Tone::Ink));
        let over = Activity::Timer(Timer::of_focus(crate::focus_timer::View { phase: 2, left_ms: 0, running: false, ..v }));
        assert_eq!(pill_words(&over), ("Break over".into(), String::new()));
        assert_eq!(Timer::default().ratio(), 0.0);
    }

    #[test]
    fn pill_text_truncation() {
        assert_eq!(truncate("Short", 10), "Short");
        assert_eq!(truncate("  padded  ", 6), "padded");
        assert_eq!(truncate("Hello world", 6), "Hello…");
        // No dangling separator before the ellipsis.
        assert_eq!(truncate("Song — Artist", 7), "Song…");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("abcd", 1), "…");
        // A letter keeps its combining mark (e + U+0301): never a bare "e…".
        assert_eq!(truncate("cafe\u{301} au lait", 5), "caf…");
        // Hangul and emoji count as characters, never bytes; a skin tone
        // stays with its emoji.
        assert_eq!(truncate("가나다라마바사", 4), "가나다…");
        assert_eq!(truncate("👍🏽👍🏽👍🏽", 3), "👍🏽…");
        assert_eq!(truncate("👍🏽👍🏽👍🏽", 2), "…");
    }

    #[test]
    fn battery_notices() {
        assert_eq!(battery_notice(None, (50, true)), None, "first reading");
        assert_eq!(battery_notice(Some((63, false)), (64, true)), Some(Power::Charging(64)));
        assert_eq!(battery_notice(Some((64, true)), (64, true)), None);
        assert_eq!(battery_notice(Some((20, false)), (19, false)), Some(Power::Low(19)));
        assert_eq!(battery_notice(Some((19, false)), (18, false)), None, "once per level");
        assert_eq!(battery_notice(Some((10, false)), (9, false)), Some(Power::Low(9)));
        assert_eq!(battery_notice(Some((21, true)), (15, false)), Some(Power::Low(15)), "unplugged low");
        assert_eq!(battery_notice(Some((30, false)), (15, false)), Some(Power::Low(15)), "a jump");
        assert_eq!(notice_ms(Power::Charging(1)), CHARGING_MS);
    }

    #[test]
    fn state_machine() {
        use Event::*;
        use Phase::*;
        let mut p = Hidden;
        for (e, want) in [
            (Hover, Hidden),
            (Click, Hidden),
            (Shown(true), Pill),
            (Shown(true), Pill),
            (Leave, Pill),
            (Hover, Expanded { pinned: false }),
            (Shown(true), Expanded { pinned: false }),
            (Leave, Pill),
            (Hover, Expanded { pinned: false }),
            (Click, Expanded { pinned: true }),
            (Leave, Expanded { pinned: true }),
            (Hover, Expanded { pinned: true }),
            (Click, Pill),
            (Click, Expanded { pinned: true }),
            (Dismiss, Pill),
            (Dismiss, Pill),
            (Click, Expanded { pinned: true }),
            (Shown(false), Hidden),
            (Dismiss, Hidden),
        ] {
            p = step(p, e);
            assert_eq!(p, want, "after {e:?}");
        }
    }

    #[test]
    fn place_centers_and_mirrors() {
        // Room in the middle: centered on the bar.
        assert_eq!(place(1000.0, &[(10.0, 200.0), (700.0, 290.0)], 120.0, 0.0), Some((440.0, 120.0)));
        // Menus reach past the middle: as near the middle as it fits.
        let (x, w) = place(1000.0, &[(10.0, 520.0), (800.0, 190.0)], 120.0, 0.0).unwrap();
        assert_eq!((x, w), (542.0, 120.0));
        // Too wide: as wide as the span.
        assert_eq!(place(1000.0, &[(0.0, 400.0), (600.0, 400.0)], 300.0, 0.0), Some((412.0, 176.0)));
        // ... unless the span cannot hold even what is not the title.
        assert_eq!(place(1000.0, &[(0.0, 400.0), (600.0, 400.0)], 300.0, 177.0), None);
        // No room.
        assert_eq!(place(1000.0, &[(0.0, 480.0), (540.0, 460.0)], 120.0, 0.0), None);
        // Hidden cells take no room.
        assert_eq!(place(1000.0, &[(0.0, 0.0)], 100.0, 0.0), Some((450.0, 100.0)));
        // Right to left: the cells are mirrored, so is the pill.
        let cells = [(10.0, 520.0), (800.0, 190.0)];
        let mirrored: Vec<(f32, f32)> = cells.iter().map(|&(x, w)| (crate::i18n::mirror::span(true, x, w, 0.0, 1000.0), w)).collect();
        let (mx, mw) = place(1000.0, &mirrored, 120.0, 0.0).unwrap();
        assert_eq!((mx, mw), (1000.0 - x - w, w));
    }

    #[test]
    fn widths_add_up() {
        let c = Content { lead: 18.0, text: 100.0, text_h: 17.0, trail: 16.0, trail_h: 10.0, dots: 0, baseline: None };
        // 12 | 18 | 8 | 100 | 8 | 16 | 12
        assert_eq!(content_width(&c), 174.0);
        assert_eq!(pill_width(&c), 174.0);
        // Two dots: 8 + 2 * 8 - 2.
        assert_eq!(content_width(&Content { dots: 2, ..c }), 174.0 + 8.0 + 14.0);
        // No lead: no gap for it either.
        assert_eq!(content_width(&Content { lead: 0.0, ..c }), 174.0 - 26.0);
        assert_eq!(pill_width(&Content::default()), PILL_MIN_W);
        assert_eq!(pill_width(&Content { text: 1000.0, ..c }), PILL_MAX_W);
        assert_eq!(pill_need(&c), PILL_MIN_W.max(12.0 + 18.0 + 8.0 + 16.0 + 12.0));
        // The title gets what the rest leaves, at most TITLE_MAX, nothing
        // under TEXT_MIN.
        assert_eq!(text_room(&c, 174.0), 100.0);
        assert_eq!(text_room(&c, 1000.0), TITLE_MAX);
        assert_eq!(text_room(&c, 74.0 + TEXT_MIN - 1.0), 0.0);
        assert_eq!(text_room(&Content::default(), 100.0), 76.0);
    }

    /// The bug in docs/island-bug.png: the background was a shape at x
    /// -w/2 in a shape visual at x 0, so its left half (left of the pill's
    /// center) was never drawn while the content was. Lays a long title out
    /// at several scales, both directions, roomy and squeezed bars, and
    /// checks that the background is where the pill is, inside the visual
    /// that draws it, and that every part is inside the background.
    #[test]
    fn content_stays_inside_the_pill() {
        // A long title as the media session gives it (DIP at 12.5 semibold,
        // roughly), album art, music bars, with or without other
        // activities' dots.
        let title_full = 520.0_f32;
        let mut laid = 0;
        for &scale in &[1.0_f32, 1.25, 1.5, 1.75, 2.0, 2.25, 3.0] {
            for &rtl in &[false, true] {
                for &dots in &[0_usize, 1, MAX_DOTS] {
                    for &(bar_w, bar_h) in &[(1920.0_f32, 32.0_f32), (1280.0, 24.0), (700.0, 48.0), (520.0, 28.0), (1280.0, 20.0), (1600.0, 22.0)] {
                        // Cells: menus on the left, status on the right.
                        let menus = (8.0, bar_w * 0.22);
                        let status = (bar_w - 8.0 - bar_w * 0.2, bar_w * 0.2);
                        let mut cells = vec![menus, status];
                        if rtl {
                            cells = cells.iter().map(|&(x, w)| (crate::i18n::mirror::span(true, x, w, 0.0, bar_w), w)).collect();
                        }
                        let h = pill_height(bar_h);
                        assert_eq!(h, (bar_h - 6.0).max(PILL_MIN_H), "the pill is the bar less 6");
                        let mut c = Content { lead: lead_fit(18.0, h), text: 0.0, text_h: 17.0, trail: 16.0, trail_h: 10.0, dots, baseline: Some(13.0) };
                        // Drawn first at the most it may take.
                        c.text = drawn_width(title_full, text_room(&c, PILL_MAX_W), scale);
                        let Some((x, w)) = place(bar_w, &cells, pill_width(&c), pill_need(&c)) else { continue };
                        // Squeezed: drawn again narrower (or left out).
                        let room = text_room(&c, w);
                        c.text = if room > 0.0 { drawn_width(title_full, room, scale) } else { 0.0 };
                        let f = frame(rtl, w, h, &c, scale);
                        laid += 1;
                        let what = format!("scale {scale} rtl {rtl} dots {dots} bar {bar_w}x{bar_h} pill {w}");
                        // The pill is where place() put it, on the bar.
                        assert!(x >= 0.0 && x + w <= bar_w, "{what}");
                        assert_eq!(f.pill, Rect { x: snap(-w / 2.0, scale), y: 0.0, w, h }, "{what}");
                        // Every part starts on a device pixel, mirrored or
                        // not (topbar-text review P2: the root is on one too).
                        for r in [f.pill, f.lead, f.text, f.trail].iter().chain(&f.dots) {
                            for v in [r.x, r.y] {
                                assert!((v * scale - (v * scale).round()).abs() < 1e-3, "{what}: {v} off the pixels in {r:?}");
                            }
                        }
                        background_follows_the_pill(&f, &what);
                        // Everything inside the pill, clear of its round ends.
                        let inner = Rect { x: f.pill.x + PAD - 1.5, y: 0.0, w: w - 2.0 * PAD + 3.0, h };
                        let mut parts = vec![("lead", f.lead), ("trail", f.trail)];
                        // The title's line box may be taller than a short
                        // pill (its ink is not): centered on it, then.
                        if c.text > 0.0 && c.text_h <= h {
                            parts.push(("text", f.text));
                        } else if c.text > 0.0 {
                            assert!(((f.text.y + f.text.h / 2.0) - h / 2.0).abs() <= 0.5, "{what}: title not centered");
                            assert!(inner.contains(&Rect { y: 0.0, h, ..f.text }), "{what}: title in the padding");
                        }
                        // The art: inside the pill top to bottom with room
                        // to spare, centered on it.
                        assert!(f.lead.y >= LEAD_INSET - 0.5 && f.lead.bottom() <= h - LEAD_INSET + 0.5, "{what}: lead {:?} in a pill {h} tall", f.lead);
                        assert!(((f.lead.y + f.lead.h / 2.0) - h / 2.0).abs() <= 0.5, "{what}: lead not centered");
                        // The fade covers the title's own end only: the
                        // music bars after it are never under the mask.
                        if c.text > 0.0 {
                            let (from, to) = fade_span(f.text.w);
                            let (a, b) = (f.text.x + from, f.text.x + to);
                            assert!(a >= f.text.x && b <= f.text.right() + 0.01, "{what}: fade outside the title");
                            assert!((b - a - FADE.min(f.text.w)).abs() < 0.01, "{what}: fade width");
                            let clear = if rtl { f.trail.right() <= f.text.x } else { f.trail.x >= b };
                            assert!(clear, "{what}: the bars {:?} under the title's fade {a}..{b}", f.trail);
                        }
                        parts.extend(f.dots.iter().map(|d| ("dot", *d)));
                        for (name, r) in &parts {
                            assert!(f.pill.contains(r), "{what}: {name} {r:?} outside {:?}", f.pill);
                            assert!(inner.contains(r), "{what}: {name} {r:?} in the padding");
                        }
                        // The title within its most, the parts in reading
                        // order and apart.
                        assert!(c.text <= TITLE_MAX + 0.01, "{what}");
                        let order: Vec<f32> = [f.lead, f.text, f.trail].iter().filter(|r| r.w > 0.0).map(|r| r.x).collect();
                        let sorted = order.windows(2).all(|p| if rtl { p[0] > p[1] } else { p[0] < p[1] });
                        assert!(sorted, "{what}: {order:?}");
                        if c.text > 0.0 {
                            let (a, b) = if rtl { (f.text, f.lead) } else { (f.lead, f.text) };
                            assert!(b.x - a.right() >= GAP - 1.0, "{what}: lead and title touch");
                            // The bars stand on the title's baseline.
                            assert_eq!(f.trail.bottom(), snap(f.text.y + 13.0 - 10.0, scale) + 10.0, "{what}");
                        }
                    }
                }
            }
        }
        assert!(laid > 100, "{laid}");
    }

    /// The background is the pill: the fill (under the pill's clip) covers
    /// it and stays clear of nothing, the border lies inside its own visual
    /// and just inside the pill, the shadow's visual is the pill (its mask
    /// a pill too, so the shadow follows the shape, not a rectangle).
    fn background_follows_the_pill(f: &Frame, what: &str) {
        assert!(f.fill.contains(&f.pill), "{what}: fill {:?} short of the pill {:?}", f.fill, f.pill);
        assert_eq!(f.shadow, f.pill, "{what}: shadow");
        let local = Rect { x: 0.0, y: 0.0, ..f.edge_box };
        assert!(local.contains(&f.edge), "{what}: border {:?} outside its visual {:?}", f.edge, f.edge_box);
        let on_root = Rect { x: f.edge_box.x + f.edge.x, y: f.edge_box.y + f.edge.y, ..f.edge };
        assert_eq!(on_root, Rect { x: f.pill.x + 0.5, y: 0.5, w: f.pill.w - 1.0, h: f.pill.h - 1.0 }, "{what}: border");
        // Nothing of the background left of the pill's center only (the
        // two earlier bugs: half a pill, then a half-layer rectangle).
        assert!(f.edge_box.x <= f.pill.x && f.fill.x <= f.pill.x && f.fill.right() >= f.pill.right(), "{what}");
    }

    #[test]
    fn pill_and_lead_fit_every_bar() {
        for bar in [20.0_f32, 22.0, 24.0, 26.0, 28.0, 32.0, 40.0, 48.0] {
            let h = pill_height(bar);
            assert_eq!(h, (bar - 6.0).max(PILL_MIN_H));
            let art = lead_fit(18.0, h);
            assert!(art <= h - 2.0 * LEAD_INSET && art > 0.0, "{bar}: {art}");
            if h >= 18.0 + 2.0 * LEAD_INSET {
                assert_eq!(art, 18.0, "{bar}: art at full size when it fits");
            }
        }
        assert_eq!(lead_fit(18.0, 16.0), 12.0, "the shortest bar: smaller art");
        assert_eq!(lead_fit(0.0, 16.0), 0.0, "no lead stays none");
        assert_eq!(fade_span(100.0), (80.0, 100.0));
        assert_eq!(fade_span(12.0), (0.0, 12.0));
    }

    #[test]
    fn short_content_is_centered() {
        let c = Content { lead: 16.0, ..Default::default() };
        let f = frame(false, PILL_MIN_W, 24.0, &c, 1.0);
        assert_eq!((f.lead.x, f.lead.y), (-8.0, 4.0));
        assert_eq!(frame(true, PILL_MIN_W, 24.0, &c, 1.0).lead.x, -8.0);
    }

    #[test]
    fn media_text_is_one_line() {
        assert_eq!(one_line("Carrionhog's combo:\nGet hit by its curse attack"), "Carrionhog's combo: Get hit by its curse attack");
        assert_eq!(one_line("  a\r\n\tb   c \u{2028} d "), "a b c d");
        assert_eq!(clean_title("Boss guide:\r\nPhase 2 - YouTube", "Chrome"), "Boss guide: Phase 2");
        assert_eq!(clean_artist("Band\n - Topic", "Chrome"), "Band");
        assert_eq!(clean_title("Song", "Spotify.exe"), "Song");
    }

    #[test]
    fn media_titles_are_cleaned_only_from_youtube_tabs() {
        let chrome = "Chrome";
        // A YouTube tab's title from a browser: suffix and count go.
        assert_eq!(clean_title("(1) Song name - YouTube", chrome), "Song name");
        assert_eq!(clean_title("(123) Song – YouTube Music", "MSEdge"), "Song");
        assert_eq!(clean_title("Song | YouTube", "308046B0AF4A39CB"), "Song");
        assert_eq!(clean_title("(2) Song - YouTube", "BraveSoftware.Brave"), "Song");
        // Real titles that look like it are kept: from a player (no
        // browser), or without the YouTube suffix.
        for t in ["(500) Days of Summer", "(2024) Live", "(1)Song", "(1) Song"] {
            assert_eq!(clean_title(t, "Spotify.exe"), t);
            assert_eq!(clean_title(t, chrome), t, "no YouTube suffix");
        }
        assert_eq!(clean_title("(500) Days of Summer - YouTube", "Spotify.exe"), "(500) Days of Summer - YouTube");
        // More than 3 digits, no space after ")", not digits: not a count.
        assert_eq!(clean_title("(2024) Live - YouTube", chrome), "(2024) Live");
        assert_eq!(clean_title("(1)Song - YouTube", chrome), "(1)Song");
        assert_eq!(clean_title("(Live) Song - YouTube", chrome), "(Live) Song");
        assert_eq!(clean_title("(12+) Song - YouTube", chrome), "(12+) Song");
        assert_eq!(clean_title("[EN/JP] [#SMTM11/2회] 'DDC & ME'", chrome), "[EN/JP] [#SMTM11/2회] 'DDC & ME'");
        assert_eq!(clean_title("Watching YouTube", chrome), "Watching YouTube");
        assert_eq!(clean_title(" - YouTube", chrome), "- YouTube");
        assert_eq!(clean_title("(3) - YouTube", chrome), "(3)");
        assert_eq!(clean_artist("Artist - Topic", chrome), "Artist");
        assert_eq!(clean_artist("Artist - Topic", "Spotify.exe"), "Artist - Topic");
        assert_eq!(clean_artist(" - Topic", chrome), "- Topic");
        assert!(!is_browser("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"));
        assert_eq!(track_line(&Track { title: clean_title("(2) Song - YouTube", chrome), artist: clean_artist("Band - Topic", chrome), ..Default::default() }), "Song — Band");
    }

    /// The clip (it shapes the fill), the border and the shadow ease
    /// separately. They stay together only if each is the pill moved by a
    /// constant (the same curve from the same start keeps them so), the
    /// fill never moves, the border's visual is wide enough for the widest
    /// pill at every step, and a first placement is not eased (it would
    /// start from (0, 0)).
    #[test]
    fn placements_ease_together_and_the_first_is_set() {
        assert!(!layout_animates(false, true), "first placement: set at once");
        assert!(!layout_animates(false, false));
        assert!(!layout_animates(true, false), "no motion");
        assert!(layout_animates(true, true));
        let c = Content { lead: 12.0, text: 120.0, text_h: 17.0, trail: 16.0, trail_h: 10.0, dots: 1, baseline: Some(13.0) };
        let first = frame(false, 200.0, 28.0, &c, 1.0);
        for &(w, h, rtl) in &[(PILL_MIN_W, 16.0, false), (300.0, 44.0, true), (PILL_MAX_W, 30.0, false), (150.0, 20.0, true)] {
            let f = frame(rtl, w, h, &Content { text: w / 2.0, ..c }, 1.0);
            assert_eq!((f.fill.x, f.fill.y, f.fill.w), (first.fill.x, first.fill.y, first.fill.w), "the fill stays put: {w}x{h}");
            assert_eq!(f.edge_box.w, first.edge_box.w, "the border's visual: as wide as the widest pill");
            assert_eq!((f.edge_box.x - f.pill.x, f.edge.x, f.edge.y), (0.0, 0.5, 0.5));
            assert_eq!((f.edge.w + 1.0, f.edge.h + 1.0), (f.pill.w, f.pill.h));
            assert_eq!(f.shadow, f.pill);
            background_follows_the_pill(&f, &format!("{w}x{h}"));
        }
    }

    fn tl(start: i64, end: i64, pos: i64, at: std::time::Instant, rate: f64) -> Timeline {
        Timeline { start_ms: start, end_ms: end, pos_ms: pos, at, rate, seekable: true, seek_min_ms: 0, seek_max_ms: 0 }
    }

    #[test]
    fn seeks_stay_where_the_app_takes_them() {
        let t = tl(0, 3_600_000, 0, std::time::Instant::now(), 1.0);
        // No MinSeekTime / MaxSeekTime: the whole track.
        assert_eq!(t.seek_range(), Some((0, 3_600_000)));
        // A live stream's window: only there, wherever the pointer was.
        let live = Timeline { seek_min_ms: 3_000_000, seek_max_ms: 3_590_000, ..t };
        assert_eq!(live.seek_range(), Some((3_000_000, 3_590_000)));
        assert_eq!(live.clamp_seek(1_800_000), Some(3_000_000));
        assert_eq!(live.clamp_seek(3_599_000), Some(3_590_000));
        assert_eq!(live.clamp_seek(3_100_000), Some(3_100_000));
        // A range past the track is cut to it.
        assert_eq!(Timeline { seek_min_ms: -5, seek_max_ms: 9_000_000, ..t }.seek_range(), Some((0, 3_600_000)));
        // Nothing left of it, not enabled, or no bar: no seeking.
        assert_eq!(Timeline { seek_min_ms: 5_000, seek_max_ms: 5_000, ..t }.seek_range(), None);
        assert_eq!(Timeline { seek_min_ms: 0, seek_max_ms: 0, seekable: false, ..t }.clamp_seek(10), None);
        assert!(!Timeline { end_ms: 0, ..t }.can_seek());
        assert!(t.can_seek());
        // Another seek window is news.
        assert!(!same_course(Some(&t), Some(&live), false));
    }

    #[test]
    fn a_seek_carries_its_track() {
        for (gen, ms) in [(0u32, 0i64), (1, 187_000), (u32::MAX, 3_600_000), (0x8000_0000, u32::MAX as i64)] {
            assert_eq!(unpack_seek(pack_seek(gen, ms)), (gen, ms));
        }
        assert_eq!(unpack_seek(pack_seek(7, -5)), (7, 0), "no negative position");
        assert_eq!(unpack_seek(pack_seek(7, i64::MAX)), (7, u32::MAX as i64));
        assert_eq!(secs(1_500), 1.5);
        assert_eq!(secs(500), 0.5, "a short clip keeps a range");
    }

    #[test]
    fn times_read_as_minutes_or_hours() {
        assert_eq!(time_text(0, false), "0:00");
        assert_eq!(time_text(7_999, false), "0:07", "whole seconds, never rounded up");
        assert_eq!(time_text(187_000, false), "3:07");
        assert_eq!(time_text(600_000, false), "10:00");
        assert_eq!(time_text(3_727_000, false), "1:02:07");
        assert_eq!(time_text(5_000, true), "0:00:05", "long: both ends alike");
        assert_eq!(time_text(-3_000, false), "0:00");
        let now = std::time::Instant::now();
        assert_eq!(time_pair(&tl(0, 245_000, 0, now, 1.0), 83_400), ("1:23".to_string(), "4:05".to_string()));
        assert_eq!(time_pair(&tl(0, 3_600_000, 0, now, 1.0), 61_000), ("0:01:01".to_string(), "1:00:00".to_string()));
        // From the timeline's start (a stream may not start at 0).
        assert_eq!(time_pair(&tl(10_000, 70_000, 0, now, 1.0), 40_000), ("0:30".to_string(), "1:00".to_string()));
    }

    #[test]
    fn the_position_moves_by_the_clock_and_the_rate_inside_the_track() {
        assert_eq!(interpolate(10_000, 2_000, 1.0, true, 0, 60_000), 12_000);
        assert_eq!(interpolate(10_000, 2_000, 1.5, true, 0, 60_000), 13_000);
        assert_eq!(interpolate(10_000, 2_000, 0.5, true, 0, 60_000), 11_000);
        assert_eq!(interpolate(10_000, 2_000, 1.0, false, 0, 60_000), 10_000, "paused: held");
        assert_eq!(interpolate(59_000, 5_000, 1.0, true, 0, 60_000), 60_000, "never past the end");
        assert_eq!(interpolate(-500, 0, 1.0, true, 0, 60_000), 0, "never before the start");
        assert_eq!(interpolate(1_000, 1_000, f64::NAN, true, 0, 60_000), 2_000, "a rate that is no number counts as 1");
        assert_eq!(interpolate(1_000, 1_000, 0.0, true, 0, 60_000), 2_000);
        assert_eq!(interpolate(1_000, -5_000, 1.0, true, 0, 60_000), 1_000, "no time going back");
        let at = std::time::Instant::now();
        let t = tl(0, 60_000, 20_000, at, 2.0);
        assert_eq!(t.position(at + std::time::Duration::from_millis(1500), true), 23_000);
        assert_eq!(t.position(at, true), 20_000);
        // A report from after `now` (a clock that is not monotonic between
        // threads) does not move it back.
        assert_eq!(tl(0, 60_000, 20_000, at + std::time::Duration::from_secs(1), 1.0).position(at, true), 20_000);
        assert!(t.shown());
        assert!(!tl(0, 0, 0, at, 1.0).shown(), "EndTime 0: no bar");
        assert!(!tl(5_000, 5_000, 0, at, 1.0).shown());
        assert_eq!(t.step(58_000, SEEK_STEP_MS), 60_000);
        assert_eq!(t.step(3_000, -SEEK_STEP_MS), 0);
        assert_eq!(t.step(30_000, SEEK_STEP_MS), 35_000);
    }

    #[test]
    fn reports_on_the_same_course_are_not_news() {
        let at = std::time::Instant::now();
        let a = tl(0, 200_000, 10_000, at, 1.0);
        let later = |ms: u64, pos: i64| tl(0, 200_000, pos, at + std::time::Duration::from_millis(ms), 1.0);
        assert!(same_course(Some(&a), Some(&later(5_000, 15_000)), true), "it played on");
        assert!(same_course(Some(&a), Some(&later(5_000, 15_600)), true), "a little drift");
        assert!(!same_course(Some(&a), Some(&later(5_000, 60_000)), true), "a seek");
        assert!(!same_course(Some(&a), Some(&later(5_000, 15_000)), false), "paused: it should not have moved");
        assert!(same_course(Some(&a), Some(&later(5_000, 10_000)), false));
        assert!(!same_course(Some(&a), Some(&tl(0, 180_000, 10_000, at, 1.0)), true), "another track");
        assert!(!same_course(Some(&a), Some(&tl(0, 200_000, 10_000, at, 1.5)), true), "another rate");
        assert!(!same_course(Some(&a), Some(&Timeline { seekable: false, ..a }), true));
        assert!(!same_course(Some(&a), None, true));
        assert!(same_course(None, None, true));
    }

    #[test]
    fn the_pointer_picks_a_position_mirrored_right_to_left() {
        let t = tl(0, 100_000, 0, std::time::Instant::now(), 1.0);
        assert_eq!(seek_at(false, 50.0, 50.0, 150.0, &t), 0);
        assert_eq!(seek_at(false, 75.0, 50.0, 150.0, &t), 25_000);
        assert_eq!(seek_at(false, 150.0, 50.0, 150.0, &t), 100_000);
        assert_eq!(seek_at(false, 10.0, 50.0, 150.0, &t), 0, "clamped");
        assert_eq!(seek_at(false, 999.0, 50.0, 150.0, &t), 100_000, "clamped");
        assert_eq!(seek_at(true, 75.0, 50.0, 150.0, &t), 75_000, "the start is at the right end");
        assert_eq!(seek_at(true, 150.0, 50.0, 150.0, &t), 0);
        let s = tl(20_000, 120_000, 0, std::time::Instant::now(), 1.0);
        assert_eq!(seek_at(false, 100.0, 50.0, 150.0, &s), 70_000, "from the timeline's start");
        assert_eq!(t.ratio(25_000), 0.25);
        assert_eq!(t.ratio(-1), 0.0);
        assert_eq!(t.ratio(200_000), 1.0);
        assert_eq!(tl(0, 0, 0, std::time::Instant::now(), 1.0).ratio(5), 0.0);
    }

    #[test]
    fn the_tick_lands_after_the_next_whole_second() {
        assert_eq!(ms_to_next_second(12_300, 1.0), 700);
        assert_eq!(ms_to_next_second(12_000, 1.0), 1000);
        assert_eq!(ms_to_next_second(12_300, 2.0), 350);
        assert_eq!(ms_to_next_second(12_990, 1.0), 50, "never a busy loop");
        assert_eq!(ms_to_next_second(12_000, 0.25), 1000, "never longer than a second");
    }

    /// Expanded -> the bar moves (top -> bottom, then back) -> a pill with
    /// no card -> a hover or the first click opens the card again.
    #[test]
    fn a_moved_bar_leaves_a_pill_that_opens_again() {
        use super::{after_bar_moved, step, Event, Phase};
        for pinned in [false, true] {
            // There (top -> bottom) and back (bottom -> top).
            let mut p = Phase::Expanded { pinned };
            for leg in ["top -> bottom", "bottom -> top"] {
                p = after_bar_moved(p);
                assert_eq!(p, Phase::Pill, "pinned={pinned} {leg}: no card");
                assert_eq!(step(p, Event::Hover), Phase::Expanded { pinned: false }, "{leg}: hover opens it");
                assert_eq!(step(p, Event::Click), Phase::Expanded { pinned: true }, "{leg}: the first click opens it");
                // Open again before the next move.
                p = step(p, Event::Click);
            }
        }
        // A pill or a hidden island stays as it is.
        assert_eq!(after_bar_moved(Phase::Pill), Phase::Pill);
        assert_eq!(after_bar_moved(Phase::Hidden), Phase::Hidden);
        // Without it (the old way, the card window closed but the phase
        // kept): the first click only closed it.
        assert_eq!(step(Phase::Expanded { pinned: true }, Event::Click), Phase::Pill);
    }
}
