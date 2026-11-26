//! Panels that drop down from the top bar instead of Windows' own flyouts
//! (which open at the bottom, where the hidden taskbar is): volume, clock
//! (calendar, under a large live clock), network (Wi-Fi), battery, the
//! system menu on the Windows button (a click: about this PC, settings,
//! recent items, power), and the bar's own menu (right-click on an empty
//! part of it: its settings). Each is a popup menu (popup.rs) with
//! sliders / grids where needed; Windows' full panels stay one click away
//! at the bottom.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND};

use crate::i18n::{t, tf};
use crate::popup::Item;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Panel {
    Volume,
    /// Year, month (1-12), and the day picked in it for the agenda (0:
    /// today).
    Calendar(i32, u32, u32),
    Network,
    Battery,
    /// The system menu (a click on the Windows button), using Windows
    /// system menu: about this PC, settings, recent items, force quit,
    /// power, lock.
    System,
    /// The virtual desktops (right click on the desktop dots).
    Desktops,
    /// The keyboard layouts (right click on the input indicator).
    Input,
    /// The bar's own menu (right-click on an empty part of the bar, or on
    /// a cell with no menu of its own): Settings at the Top bar page.
    Bar,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Ask again for what the panel shows, in place (a list that failed:
    /// its "Try again" button).
    Reload,
    Open(&'static str),
    Notifications,
    ToggleMute,
    Month(i32, u32),
    /// A day picked in the month: its events in the agenda (the calendar
    /// opens again on it, like a month change).
    Day(i32, u32, u32),
    /// An agenda event: its link, its calendar's page or Windows' app.
    Event(crate::agenda::Open),
    Connect { iface: windows::core::GUID, profile: String },
    Lock,
    Logoff,
    Sleep,
    /// Turn the displays off now (display_off), the PC stays on.
    DisplayOff,
    Restart,
    Shutdown,
    /// Shut down or sleep in this many minutes (shutdown.rs), in place of
    /// any other schedule.
    Schedule(super::shutdown::Kind, u32),
    /// Call the scheduled shutdown or sleep off.
    CancelSchedule,
    /// Switch to virtual desktop i.
    Desktop(usize),
    NewDesktop,
    /// Close the desktop with this GUID, only if it is still the current one.
    CloseDesktop([u8; 16]),
    TaskView,
    /// Native <-> Latin input in the window in front.
    InputToggle,
    /// Switch the window in front to this keyboard layout (HKL).
    InputLayout(isize),
    /// NopBar's Input settings page.
    InputSettings,
    /// The focus timer (crate::focus_timer).
    Timer(crate::focus_timer::Command),
    /// Its settings (Settings > Top bar).
    TimerSettings,
    /// NopBar's settings on a page (`--page`: "bar", "dock", "general").
    Settings(&'static str),
    /// A recent file or folder (its path).
    OpenPath(String),
    /// "About this PC" (about.rs) on the bar's monitor.
    AboutPc,
}

#[derive(Default, PartialEq)]
pub struct Built {
    pub items: Vec<Item>,
    pub actions: Vec<Action>,
    /// The panel has a volume slider.
    pub volume_slider: bool,
    /// The calendar's agenda: when (UTC seconds) a row it shows starts or
    /// ends, or the day turns: it is built again then while open.
    pub refresh_at: Option<i64>,
    /// It shows the progress ring for something still being fetched (the
    /// Wi-Fi list): built again once that settles (wifi_settled).
    pub waiting: bool,
}

impl Built {
    fn add(&mut self, label: impl Into<String>, a: Action) -> &mut Item {
        self.actions.push(a);
        self.items.push(Item::new(self.actions.len() as u32, label));
        self.items.last_mut().unwrap()
    }
    /// A line that only says something (a status): read, never picked.
    fn info(&mut self, label: impl Into<String>) {
        self.items.push(Item::info(label));
    }
    /// An empty list's one line (design system 11.1).
    fn quiet(&mut self, label: impl Into<String>) {
        self.items.push(Item::quiet(label));
    }
    /// A space between groups: one, and only after a row (a group left
    /// out, like the calendar's focus timer when it is off the menu,
    /// leaves no double space).
    fn sep(&mut self) {
        if self.items.last().is_some_and(|i| !i.separator) {
            self.items.push(Item::sep());
        }
    }
    /// Adds an action with no row of its own (a grid cell's); its id.
    fn push_action(&mut self, a: Action) -> u32 {
        self.actions.push(a);
        self.actions.len() as u32
    }
    pub fn action(&self, id: u32) -> Option<&Action> {
        self.actions.get((id as usize).checked_sub(1)?)
    }
}

// ---- calendar ---------------------------------------------------------------

fn leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in(y: i32, m: u32) -> u32 {
    match m {
        2 if leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Weekday of the 1st (0 = Sunday), Sakamoto's method.
pub fn first_weekday(y: i32, m: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    ((y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + 1).rem_euclid(7)) as u32
}

/// Weeks of a month, Sunday first; 0 = blank.
pub fn month_grid(y: i32, m: u32) -> Vec<[u32; 7]> {
    let (first, n) = (first_weekday(y, m), days_in(y, m));
    let mut weeks = Vec::new();
    let mut week = [0u32; 7];
    let mut col = first as usize;
    for d in 1..=n {
        week[col] = d;
        col += 1;
        if col == 7 {
            weeks.push(week);
            week = [0; 7];
            col = 0;
        }
    }
    if col > 0 {
        weeks.push(week);
    }
    weeks
}

fn prev_month(y: i32, m: u32) -> (i32, u32) {
    if m == 1 { (y - 1, 12) } else { (y, m - 1) }
}

fn next_month(y: i32, m: u32) -> (i32, u32) {
    if m == 12 { (y + 1, 1) } else { (y, m + 1) }
}

// ---- agenda -------------------------------------------------------------------

/// The time the agenda is built for: now (UTC seconds), today (local day,
/// days since 1970) and the PC's zone both ways (local seconds read as UTC
/// <-> UTC seconds); fixed in tests.
pub struct Now<'a> {
    pub utc: i64,
    pub today: i64,
    pub to_utc: &'a dyn Fn(i64) -> i64,
    pub to_local: &'a dyn Fn(i64) -> i64,
}

/// Most event rows the agenda shows.
pub const AGENDA_EVENTS: usize = 5;
/// Today with fewer events left than this shows tomorrow's too.
const FEW_LEFT: usize = 3;

/// Pure: how many rows the agenda may take in a calendar whose other rows
/// are `base`, in a work area `work_h` DIP tall: what fits with its line at
/// the normal density (at least three), else at the compact one the popup
/// then uses; 1 to 7. `clock`: how tall the clock row's lines are
/// (popup::row_layout).
pub fn agenda_lines(base: &[Item], work_h: f32, clock: f32) -> usize {
    use crate::popup::{row_layout, Density};
    // Agenda rows are cards, taller than a menu row.
    let room = |d: Density| ((work_h - row_layout(base, d, clock).1 - d.sep_h()) / d.event_h()).floor();
    let n = room(Density::Normal);
    let n = if n >= 3.0 { n } else { room(Density::Compact) };
    (n.max(1.0) as usize).min(7)
}

/// An event as one agenda line: its row, whether it is over (dim), on
/// now (marked), or all day (or longer than the day it shows on).
#[derive(Clone, Debug, PartialEq)]
pub struct DayEvent {
    pub index: usize,
    pub past: bool,
    pub now: bool,
    pub all_day: bool,
}

/// Pure: the events of local day `day` (by their index in `events`, which
/// is sorted by start): the all-day ones and those that started before the
/// day first, then the rest by start. On today (`today`), the ones over
/// are left out; on another day they are kept, dim when over.
pub fn day_events(events: &[crate::agenda::Shown], day: i64, clock: &Now) -> Vec<DayEvent> {
    let (d0, d1) = ((clock.to_utc)(day * 86_400), (clock.to_utc)((day + 1) * 86_400));
    let is_today = day == clock.today;
    let mut out: Vec<DayEvent> = Vec::new();
    for (index, s) in events.iter().enumerate() {
        let e = &s.ev;
        let (on_day, all_day, past, now) = if e.all_day {
            let (first, end) = (e.start.div_euclid(86_400), e.end.div_euclid(86_400).max(e.start.div_euclid(86_400) + 1));
            (first <= day && day < end, true, end <= clock.today, false)
        } else {
            // A moment (no length) counts until 15 minutes after it.
            let end = e.end.max(e.start + if e.end <= e.start { 15 * 60 } else { 0 });
            let on = e.start < d1 && end > d0;
            (on, e.start < d0, end <= clock.utc, e.start <= clock.utc && clock.utc < end)
        };
        if !on_day || (is_today && past) {
            continue;
        }
        out.push(DayEvent { index, past, now: now && !all_day, all_day });
    }
    out.sort_by_key(|d| !d.all_day);
    out
}

/// Pure: the next moment the agenda's rows change by themselves: an event
/// of today or tomorrow starts or ends (a moment counts until 15 minutes
/// after it), or the day turns at local midnight.
pub fn next_change(events: &[crate::agenda::Shown], clock: &Now) -> i64 {
    let midnight = (clock.to_utc)((clock.today + 1) * 86_400);
    let horizon = (clock.to_utc)((clock.today + 2) * 86_400);
    let mut next = midnight;
    for s in events {
        let e = &s.ev;
        if e.all_day || e.start >= horizon {
            continue;
        }
        let end = e.end.max(e.start + if e.end <= e.start { 15 * 60 } else { 0 });
        for t in [e.start, end] {
            if t > clock.utc && t < next {
                next = t;
            }
        }
    }
    next
}

/// The time an agenda line shows: the start in the region's format, or
/// "All day".
fn event_time(d: &DayEvent, e: &crate::agenda::ics::Event, clock: &Now) -> String {
    if d.all_day {
        return t("agenda.all_day").to_string();
    }
    menu_time_local((clock.to_local)(e.start))
}

/// Local seconds (read as UTC) as a SYSTEMTIME.
fn local_systemtime(local: i64) -> windows::Win32::Foundation::SYSTEMTIME {
    let (y, m, d) = crate::agenda::tz::civil_from_days(local.div_euclid(86_400));
    let s = local.rem_euclid(86_400);
    windows::Win32::Foundation::SYSTEMTIME {
        wYear: y as u16,
        wMonth: m as u16,
        wDay: d as u16,
        wDayOfWeek: crate::agenda::tz::weekday(local.div_euclid(86_400)) as u16,
        wHour: (s / 3600) as u16,
        wMinute: (s / 60 % 60) as u16,
        wSecond: (s % 60) as u16,
        wMilliseconds: 0,
    }
}

/// The agenda under the month, at most `lines` rows (agenda_lines): the
/// picked day's heading (not on today), its events (at most
/// AGENDA_EVENTS, "N more…" past that), then tomorrow's under a small
/// heading while today has few left; the empty states (nothing left
/// today; a failed fetch). No calendar, or a day the agenda never fetched
/// (far from today): no rows at all, the calendar alone (owner,
/// 2026-11-08). The one place the section's rows are decided; each event
/// row is drawn by popup's draw_event_row.
pub fn agenda_section(b: &mut Built, snap: &crate::agenda::Snapshot, clock: &Now, day: i64, lines: usize) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let info = |s: String| Item::info(s);
    let lines = lines.max(1);
    let none = snap.to <= snap.from;
    let loaded = snap.to > snap.from && (clock.to_utc)(day * 86_400) >= snap.from && (clock.to_utc)((day + 1) * 86_400) <= snap.to;
    if !snap.has_source || (!snap.loading && !none && !loaded) {
        return out;
    }
    let is_today = day == clock.today;
    let mut left = lines;
    if !is_today && left > 1 {
        let st = local_systemtime(day * 86_400);
        out.push(info(crate::i18n::date::long(&st)));
        left -= 1;
    }
    // The note on a failed fetch keeps its line.
    let note = snap.error && left > 1;
    if note {
        left -= 1;
    }
    let row = |b: &mut Built, d: &DayEvent| -> Item {
        let s = &snap.events[d.index];
        let title = if s.ev.title.trim().is_empty() { t("dock.menu.untitled").to_string() } else { s.ev.title.clone() };
        let r = crate::popup::EventRow { time: event_time(d, &s.ev, clock), title, place: s.ev.location.clone(), now: d.now, past: d.past };
        Item::event(b.push_action(Action::Event(s.open.clone())), r)
    };
    let mut shown = 0;
    if snap.loading || (none && !snap.error) {
        out.push(info(t("agenda.loading").into()));
    } else if none {
        // Nothing fetched yet and the fetch failed: the note says it all.
        if !note {
            out.push(info(t("agenda.error").into()));
        }
    } else {
        let list = day_events(&snap.events, day, clock);
        if list.is_empty() {
            out.push(info(t(if is_today { "agenda.none_today" } else { "agenda.none_day" }).into()));
            left = left.saturating_sub(1);
        } else {
            let cap = left.min(AGENDA_EVENTS).max(1);
            let take = if list.len() > cap { cap.saturating_sub(1).max(1) } else { list.len() };
            for d in &list[..take] {
                out.push(row(b, d));
            }
            shown = take;
            left = left.saturating_sub(take);
            if list.len() > take && left > 0 {
                out.push(info(tf("agenda.more", &[("n", &(list.len() - take).to_string())])));
                left -= 1;
            } else if list.len() > take {
                // No line left: the last row says it instead.
                if let Some(last) = out.last_mut() {
                    *last = info(tf("agenda.more", &[("n", &(list.len() - take + 1).to_string())]));
                }
            }
        }
        // Tomorrow, while today has few left.
        if is_today && snap.tomorrow && shown < FEW_LEFT && left >= 2 && shown < AGENDA_EVENTS {
            let next = day_events(&snap.events, day + 1, clock);
            let room = (left - 1).min(AGENDA_EVENTS - shown);
            if !next.is_empty() && room > 0 {
                out.push(info(t("agenda.tomorrow").into()));
                for d in next.iter().take(room) {
                    out.push(row(b, d));
                }
            }
        }
    }
    if note {
        out.push(info(t("agenda.error").into()));
    }
    out
}

// ---- Wi-Fi --------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wifi {
    pub ssid: String,
    pub signal: u32,
    pub secured: bool,
    pub connected: bool,
    pub profile: Option<String>,
    pub iface: windows::core::GUID,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WifiError {
    /// Windows needs location permission for apps to see networks.
    Denied,
    /// WLAN service off / no adapter.
    Service,
    /// Did not answer at all (WIFI_GIVE_UP): the list failed.
    Slow,
    /// Not back yet (design system 11.3: the 300 ms rule): the panel
    /// shows the progress ring and takes the list when it comes
    /// (wifi_settled).
    Pending,
}

/// The one fetch of the list: since when it runs, and what it brought
/// (when).
struct WifiJob {
    since: Option<std::time::Instant>,
    done: Option<WifiResult>,
}

struct WifiResult {
    started: std::time::Instant,
    completed: std::time::Instant,
    list: Result<Vec<Wifi>, WifiError>,
}

impl WifiResult {
    fn usable(&self, since: Option<std::time::Instant>) -> bool {
        self.completed.elapsed() < WIFI_FRESH && since.is_none_or(|at| self.started >= at)
    }
}

static WIFI: std::sync::Mutex<WifiJob> = std::sync::Mutex::new(WifiJob { since: None, done: None });

/// The panel waits this long for the list before it opens with the ring
/// (the 300 ms rule: motion::TIP_DELAY_MS), and this long in all before
/// the list counts as failed.
const WIFI_WAIT: std::time::Duration = std::time::Duration::from_millis(crate::motion::TIP_DELAY_MS as u64);
const WIFI_GIVE_UP: std::time::Duration = std::time::Duration::from_secs(5);
/// A list that came this long ago is not shown as now's.
const WIFI_FRESH: std::time::Duration = std::time::Duration::from_secs(10);

/// Pure: what a fetch running for `running` says (None: none runs, start
/// one): still waited for, or given up on.
pub fn wifi_wait(running: Option<std::time::Duration>) -> Option<WifiError> {
    running.map(|d| if d >= WIFI_GIVE_UP { WifiError::Slow } else { WifiError::Pending })
}

/// The list, fetched on a short-lived thread: the UI waits at most
/// WIFI_WAIT for it. Not back by then: Pending, and the same fetch goes
/// on (one at a time: a WLAN service that never answers keeps one thread,
/// not one per panel opened, audit A05); asked again, what it brought
/// meanwhile, or Slow once it has run WIFI_GIVE_UP.
pub fn wifi_list() -> Result<Vec<Wifi>, WifiError> {
    wifi_list_since(None)
}

/// Excludes a scan started before the adapter turned on.
pub(super) fn wifi_list_since(since: Option<std::time::Instant>) -> Result<Vec<Wifi>, WifiError> {
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let Ok(mut job) = WIFI.lock() else { return Err(WifiError::Slow) };
        if let Some(result) = job.done.take() {
            if result.usable(since) {
                return result.list;
            }
        }
        if let Some(e) = wifi_wait(job.since.map(|t| t.elapsed())) {
            return Err(e);
        }
        let started = std::time::Instant::now();
        let spawned = std::thread::Builder::new().name("wifi".into()).spawn(move || {
            let list = wifi_list_now();
            if let Ok(mut job) = WIFI.lock() {
                job.done = Some(WifiResult { started, completed: std::time::Instant::now(), list });
                job.since = None;
            }
            let _ = tx.send(());
        });
        if spawned.is_err() {
            return Err(WifiError::Slow);
        }
        job.since = Some(started);
    }
    let _ = rx.recv_timeout(WIFI_WAIT);
    let Ok(mut job) = WIFI.lock() else { return Err(WifiError::Slow) };
    job.done.take().filter(|result| result.usable(since)).map_or(Err(WifiError::Pending), |result| result.list)
}

/// A panel showing the ring can take its answer now: the list came, or
/// the fetch is given up on (wifi_list then says which).
pub fn wifi_settled() -> bool {
    WIFI.lock().map(|job| job.done.is_some() || wifi_wait(job.since.map(|t| t.elapsed())) != Some(WifiError::Pending)).unwrap_or(true)
}

/// Networks Windows last saw (no new scan: the cached list).
fn wifi_list_now() -> Result<Vec<Wifi>, WifiError> {
    use windows::Win32::NetworkManagement::WiFi::*;
    let mut out: Vec<Wifi> = Vec::new();
    let mut denied = false;
    unsafe {
        let mut ver = 0u32;
        let mut h = HANDLE::default();
        if WlanOpenHandle(2, None, &mut ver, &mut h) != 0 {
            return Err(WifiError::Service);
        }
        let mut ifs: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(h, None, &mut ifs) == 0 && !ifs.is_null() {
            let n = (*ifs).dwNumberOfItems as usize;
            let infos = std::slice::from_raw_parts((*ifs).InterfaceInfo.as_ptr(), n);
            for info in infos {
                let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
                let r = WlanGetAvailableNetworkList(h, &info.InterfaceGuid, 0, None, &mut list);
                if r == 5 {
                    // ERROR_ACCESS_DENIED: location permission is off.
                    denied = true;
                }
                if r != 0 || list.is_null() {
                    continue;
                }
                let n = (*list).dwNumberOfItems as usize;
                for net in std::slice::from_raw_parts((*list).Network.as_ptr(), n) {
                    let len = (net.dot11Ssid.uSSIDLength as usize).min(32);
                    let ssid = String::from_utf8_lossy(&net.dot11Ssid.ucSSID[..len]).to_string();
                    if ssid.is_empty() {
                        continue;
                    }
                    let pend = net.strProfileName.iter().position(|&c| c == 0).unwrap_or(0);
                    let profile = (pend > 0).then(|| String::from_utf16_lossy(&net.strProfileName[..pend]));
                    let w = Wifi {
                        ssid,
                        signal: net.wlanSignalQuality,
                        secured: net.bSecurityEnabled.as_bool(),
                        connected: net.dwFlags & 1 != 0,
                        profile,
                        iface: info.InterfaceGuid,
                    };
                    // One row per name: prefer the connected / known entry.
                    match out.iter_mut().find(|o| o.ssid == w.ssid) {
                        Some(o) if (w.connected, w.profile.is_some(), w.signal) > (o.connected, o.profile.is_some(), o.signal) => *o = w,
                        Some(_) => {}
                        None => out.push(w),
                    }
                }
                WlanFreeMemory(list as *const _);
            }
            WlanFreeMemory(ifs as *const _);
        }
        let _ = WlanCloseHandle(h, None);
    }
    if out.is_empty() && denied {
        return Err(WifiError::Denied);
    }
    out.sort_by(|a, b| (b.connected, b.signal).cmp(&(a.connected, a.signal)));
    out.truncate(12);
    Ok(out)
}

pub(super) fn wifi_connect(iface: &windows::core::GUID, profile: &str) -> bool {
    if cfg!(test) { return false; }
    use windows::Win32::NetworkManagement::WiFi::*;
    unsafe {
        let mut ver = 0u32;
        let mut h = HANDLE::default();
        if WlanOpenHandle(2, None, &mut ver, &mut h) != 0 {
            return false;
        }
        let name = HSTRING::from(profile);
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(name.as_ptr()),
            pDot11Ssid: std::ptr::null_mut(),
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_any,
            dwFlags: 0,
        };
        let r = WlanConnect(h, iface, &params, None);
        crate::log!("wifi: connect {profile:?} -> {r}");
        let _ = WlanCloseHandle(h, None);
        r == 0
    }
}

pub(super) fn wifi_disconnect(iface: &windows::core::GUID, ssid: &str) -> bool {
    use windows::Win32::NetworkManagement::WiFi::*;
    if cfg!(test) { return false; }
    unsafe {
        let mut version = 0;
        let mut handle = HANDLE::default();
        if WlanOpenHandle(2, None, &mut version, &mut handle) != 0 {
            return false;
        }
        // The interface may have roamed since the row was drawn.
        let mut size = 0;
        let mut data = std::ptr::null_mut();
        let status = WlanQueryInterface(handle, iface, wlan_intf_opcode_current_connection, None, &mut size, &mut data, None);
        let same = if status == 0 && size as usize >= std::mem::size_of::<WLAN_CONNECTION_ATTRIBUTES>() && !data.is_null() {
            let net = &(*data.cast::<WLAN_CONNECTION_ATTRIBUTES>()).wlanAssociationAttributes.dot11Ssid;
            let len = (net.uSSIDLength as usize).min(net.ucSSID.len());
            String::from_utf8_lossy(&net.ucSSID[..len]) == ssid
        } else { false };
        if !data.is_null() {
            WlanFreeMemory(data);
        }
        let accepted = same && WlanDisconnect(handle, iface, None) == 0;
        let _ = WlanCloseHandle(handle, None);
        accepted
    }
}

/// Pure: a network's lit Wi-Fi bars (1..=4) for its signal (0..=100 %).
fn bars(signal: u32) -> u8 {
    match signal {
        75.. => 4,
        50.. => 3,
        25.. => 2,
        _ => 1,
    }
}

/// Pure: a network row's trailing glyphs: its strength, then a lock if it
/// is secured (drawn glyphs, not text symbols).
pub(super) fn wifi_trail(signal: u32, secured: bool) -> Vec<crate::glyphs::Glyph> {
    use crate::glyphs::Glyph;
    let mut v = vec![Glyph::Wifi(bars(signal))];
    if secured {
        v.push(Glyph::Lock);
    }
    v
}

// ---- building ---------------------------------------------------------------

/// What `panel` shows. `volume` = (percent, muted) if known; `work_h`: the
/// height of the work area it opens in (DIP), which the calendar's agenda
/// keeps to. The calendar's clock row counts as the usual line spacing
/// estimates it (popup::clock_text_h); build_measured takes its measured
/// height instead.
pub fn build(panel: Panel, volume: Option<(u8, bool)>, work_h: f32) -> Built {
    build_measured(panel, volume, work_h, &|| crate::popup::clock_text_h(crate::fonts::scale()))
}

/// Like build, with `clock_text_h` giving the calendar clock's two lines
/// as the popup measures and draws them (popup::measured_clock_text_h,
/// DIP; asked only when the agenda needs its room): the agenda's number of
/// events is chosen against the same clock row the popup lays out (fit,
/// row_layout).
pub fn build_measured(panel: Panel, volume: Option<(u8, bool)>, work_h: f32, clock_text_h: &dyn Fn() -> f32) -> Built {
    let mut b = Built::default();
    match panel {
        Panel::Volume => {
            match volume {
                // The slider shows the level; screen readers hear
                // "Volume, 40%" (its name and value).
                Some((p, muted)) => {
                    b.items.push(Item { label: t("control.volume").into(), ..Item::slider(p as f32 / 100.0) });
                    b.volume_slider = true;
                    b.add(t("topbar.panel.volume.mute"), Action::ToggleMute).checked = muted;
                }
                None => b.info(t("topbar.panel.volume.no_device")),
            }
            b.sep();
            b.add(t("topbar.panel.volume.settings"), Action::Open("ms-settings:sound"));
        }
        Panel::Calendar(y, m, picked) => {
            let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
            let day = |y: i32, m: u32, d: u16| windows::Win32::Foundation::SYSTEMTIME { wYear: y as u16, wMonth: m as u16, wDay: d, ..Default::default() };
            let this_month = now.wYear as i32 == y && now.wMonth as u32 == m;
            // The day the agenda shows: the one picked, else today.
            let shown = if picked > 0 { (y, m, picked.min(days_in(y, m))) } else { (now.wYear as i32, now.wMonth as u32, now.wDay as u32) };
            let snap = crate::agenda::Snapshot::read(shown);
            let agenda = snap.enabled;
            // A large clock first (hours, minutes and seconds, the date
            // under them), live while the panel is open, whatever the bar
            // shows.
            b.items.push(Item::clock());
            b.sep();
            // The month with its ‹ › at the end; the title goes back to
            // today when another month or day shows (no rows for these
            // under the grid: owner, 2026-11-08).
            let (py, pm) = prev_month(y, m);
            let (ny, nm) = next_month(y, m);
            let picked_today = picked == 0 || (this_month && picked == now.wDay as u32);
            let nav = crate::popup::MonthNav {
                prev: b.push_action(Action::Month(py, pm)),
                next: b.push_action(Action::Month(ny, nm)),
                today: if this_month && picked_today { 0 } else { b.push_action(Action::Month(now.wYear as i32, now.wMonth as u32)) },
                prev_name: t("topbar.panel.calendar.previous").into(),
                next_name: t("topbar.panel.calendar.next").into(),
                today_name: t("topbar.panel.calendar.today").into(),
            };
            b.items.push(Item::month(crate::i18n::date::year_month(&day(y, m, 1)), nav));
            // The grid starts on Sunday; 2026-11-01 is one.
            let wd: Vec<String> = (1..=7).map(|d| crate::i18n::date::weekday(&day(2026, 11, d))).collect();
            let mut head = Item::grid(wd, None);
            head.checked = true; // dim header
            head.abbrev = true; // cut to "M" in a narrow menu; days never
            b.items.push(head);
            for week in month_grid(y, m) {
                let today = this_month.then(|| week.iter().position(|&d| d == now.wDay as u32)).flatten();
                let mut row = Item::grid(week.iter().map(|&d| if d == 0 { String::new() } else { d.to_string() }).collect(), today);
                // With a calendar to show, a day is picked by a click.
                if agenda && snap.has_source {
                    row.ring = (picked > 0).then(|| week.iter().position(|&d| d == picked)).flatten().filter(|&k| Some(k) != today);
                    row.cell_ids = week.iter().map(|&d| if d == 0 { 0 } else { b.push_action(Action::Day(y, m, d)) }).collect();
                }
                b.items.push(row);
            }
            b.sep();
            // The agenda goes here, once the rest is known (its room).
            let agenda_at = b.items.len();
            // Settings > Features can take it off the menu (a running timer stays).
            if crate::focus_timer::in_menu() {
                let (idle, running, focus, today) = crate::focus_timer::menu_state();
                let mins = [crate::focus_timer::Preset::Short, crate::focus_timer::Preset::Long, crate::focus_timer::Preset::Custom].map(crate::focus_timer::minutes);
                timer_menu(&mut b, TimerMenu { idle, running, focus, today }, mins);
            }
            b.sep();
            b.add(t("topbar.panel.calendar.notifications"), Action::Notifications).shortcut = "Win+N".into();
            b.add(t("topbar.panel.calendar.settings"), Action::Open("ms-settings:dateandtime"));
            // No calendar: no agenda area at all.
            if agenda && snap.has_source {
                // The clock's lines as tall as the popup draws them.
                let lines = agenda_lines(&b.items, work_h, clock_text_h());
                let now_utc = crate::agenda::unix_now() as i64;
                let clock = Now { utc: now_utc, today: crate::agenda::local_day(now_utc), to_utc: &crate::agenda::tz::utc_from_local, to_local: &crate::agenda::tz::local_from_utc };
                let picked_day = crate::agenda::tz::days_from_civil(shown.0, shown.1, shown.2);
                let mut rows = agenda_section(&mut b, &snap, &clock, picked_day, lines);
                if !rows.is_empty() {
                    rows.push(Item::sep());
                }
                b.items.splice(agenda_at..agenda_at, rows);
                b.refresh_at = Some(next_change(&snap.events, &clock));
            }
        }
        Panel::Network => network_menu(&mut b, wifi_list()),
        Panel::Battery => {
            let mut s = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
            let read = unsafe { windows::Win32::System::Power::GetSystemPowerStatus(&mut s) }.is_ok();
            battery_menu(&mut b, read.then_some(s));
        }
        // Read now, not the watcher's cache: the close item carries the
        // current desktop's id.
        Panel::Desktops => desktops_menu(&mut b, &crate::vdesk::read()),
        Panel::Input => input_menu(&mut b),
        Panel::Bar => bar_menu(&mut b, &super::system::recent_files(), super::shutdown::current(), &time_of_day),
        Panel::System => system_menu(&mut b, &super::system::recent_files(), super::shutdown::current(), &time_of_day),
    }
    b
}

/// The network panel for what the Wi-Fi list said (`nets`: wifi_list's
/// answer): the networks, the connected one checked, each with its
/// strength and lock; or why there is no list; then Windows' page.
fn network_menu(b: &mut Built, nets: Result<Vec<Wifi>, WifiError>) {
    let nets = match nets {
        Ok(n) => n,
        Err(WifiError::Denied) => {
            // A short status, then what fixes it.
            b.info(t("topbar.panel.network.location_needed"));
            b.add(t("settings.features.open"), Action::Open("ms-settings:privacy-location"));
            Vec::new()
        }
        Err(WifiError::Service) => {
            b.info(t("topbar.panel.network.unavailable"));
            Vec::new()
        }
        Err(WifiError::Slow) => {
            // It failed (11.4): a warning glyph, one line, and a button
            // that asks again in place.
            b.actions.push(Action::Reload);
            b.items.push(Item::failed(b.actions.len() as u32, t("topbar.panel.network.list_failed"), t("launch.action.retry")));
            Vec::new()
        }
        Err(WifiError::Pending) => {
            // Still waited for (11.3): the ring alone (screen readers hear
            // what it waits for); the list takes its place when it comes
            // (the bar's wifi_poll).
            b.items.push(Item::busy(t("topbar.panel.network.loading")));
            b.waiting = true;
            Vec::new()
        }
    };
    if nets.is_empty() && b.items.is_empty() {
        b.quiet(t("topbar.panel.network.none"));
    }
    for n in nets {
        let label = n.ssid.clone();
        let row = match &n.profile {
            // Known network: connect right here.
            Some(p) if !n.connected => b.add(label, Action::Connect { iface: n.iface, profile: p.clone() }),
            Some(_) => b.add(label, Action::Open("ms-settings:network-wifi")),
            // Windows collects credentials in its network flyout.
            None => b.add(label, Action::Open(if crate::taskbar::hidden_by_us() { "ms-availablenetworks:" } else { "ms-settings:network-wifi" })),
        };
        row.checked = n.connected;
        row.trail = wifi_trail(n.signal, n.secured);
    }
    b.sep();
    b.add(t("topbar.panel.network.settings"), Action::Open("ms-settings:network"));
}

/// The battery panel for Windows' power status (`s`: None when it could
/// not be read): the level, charging or time left, the saver; or "no
/// battery"; then Windows' page.
fn battery_menu(b: &mut Built, s: Option<windows::Win32::System::Power::SYSTEM_POWER_STATUS>) {
    if let Some(s) = s.filter(|s| s.BatteryFlag != 128) {
        if s.BatteryLifePercent <= 100 {
            b.info(tf("topbar.panel.battery.level", &[("p", &s.BatteryLifePercent.to_string())]));
        } else {
            b.info(t("topbar.panel.battery.unknown"));
        }
        let charging = s.BatteryFlag & 8 != 0;
        if s.ACLineStatus == 1 {
            b.info(t(if charging { "topbar.panel.battery.charging" } else if s.BatteryLifePercent >= 100 && s.BatteryLifePercent <= 100 { "topbar.panel.battery.full" } else { "topbar.panel.battery.plugged_in" }));
        } else if s.BatteryLifeTime != u32::MAX {
            let mins = s.BatteryLifeTime / 60;
            b.info(tf("topbar.panel.battery.remaining", &[("h", &(mins / 60).to_string()), ("m", &(mins % 60).to_string())]));
        }
        if s.SystemStatusFlag & 1 != 0 {
            b.info(t("topbar.panel.battery.saver"));
        }
    } else {
        b.info(t("topbar.panel.battery.none"));
    }
    b.sep();
    b.add(t("topbar.panel.battery.settings"), Action::Open("ms-settings:powersleep"));
}

/// The system menu, top to bottom, as the system menu goes: "About this
/// PC" (its own window: what the PC is); Windows' settings; recent items
/// (a submenu); force quit (Task Manager); sleep, turn off display,
/// restart, shut down and a scheduled shutdown or sleep (schedule_rows);
/// lock and sign out. Each power row
/// acts at once, as Windows' own Start menu does: no question in between
/// (owner, 2026-11-08). No NopBar settings and no tools here: the bar's
/// own menu and Win+X have those. `pending`: the scheduled shutdown or sleep;
/// `clock`: a UTC time as the region writes it ("9:30 PM").
fn system_menu(b: &mut Built, recent: &[super::system::Recent], pending: super::shutdown::Pending, clock: &dyn Fn(i64) -> String) {
    b.add(t("topbar.system.about"), Action::AboutPc);
    b.sep();
    b.add(t("topbar.system.settings"), Action::Open("ms-settings:")).shortcut = "Win+I".into();
    b.sep();
    recent_menu(b, recent);
    b.sep();
    b.add(t("topbar.system.force_quit"), Action::Open("taskmgr.exe")).shortcut = "Ctrl+Shift+Esc".into();
    b.sep();
    b.add(t("topbar.panel.power.sleep"), Action::Sleep);
    b.add(t("topbar.system.display_off"), Action::DisplayOff);
    b.add(t("topbar.system.restart"), Action::Restart);
    b.add(t("topbar.system.shut_down"), Action::Shutdown);
    schedule_rows(b, pending, clock);
    b.sep();
    b.add(t("topbar.system.lock"), Action::Lock).shortcut = "Win+L".into();
    b.add(t("topbar.system.sign_out"), Action::Logoff);
}

/// "Schedule shutdown" and "Sleep after", each with its presets (15 min,
/// 30 min, 1 hr, 2 hr); or, while one is scheduled (one at a time), a
/// single row in their place that calls it off at once, with the time it
/// is due when NopBar knows it ("Cancel scheduled sleep (9:30 PM)").
fn schedule_rows(b: &mut Built, pending: super::shutdown::Pending, clock: &dyn Fn(i64) -> String) {
    use super::shutdown::{Kind, Pending, PRESETS};
    match pending {
        Pending::None => {
            for (kind, key) in [(Kind::Shutdown, "topbar.system.schedule"), (Kind::Sleep, "topbar.system.schedule_sleep")] {
                let mut sub = Vec::new();
                for m in PRESETS {
                    let label = if m % 60 == 0 { tf("topbar.stats.dur.h", &[("h", &(m / 60).to_string())]) } else { tf("topbar.stats.dur.m", &[("m", &m.to_string())]) };
                    sub.push(Item::new(b.push_action(Action::Schedule(kind, m)), label));
                }
                b.items.push(Item::new(0, t(key)).submenu(sub));
            }
        }
        Pending::At(kind, due) => {
            let key = match kind {
                Kind::Shutdown => "topbar.system.schedule.cancel_at",
                Kind::Sleep => "topbar.system.schedule_sleep.cancel_at",
            };
            b.add(tf(key, &[("time", &clock(due))]), Action::CancelSchedule);
        }
        Pending::Unknown => {
            b.add(t("topbar.system.schedule.cancel"), Action::CancelSchedule);
        }
    }
}

/// A UTC time (seconds) as the region writes the time of day.
fn time_of_day(utc: i64) -> String {
    menu_time_local(crate::agenda::tz::local_from_utc(utc))
}

pub(crate) fn menu_time_local(local: i64) -> String {
    crate::i18n::date::time(&local_systemtime(local))
}

/// "Recent items" with its submenu: the newest recent files and folders,
/// each opening what it names; a dim "No recent items" when there are none.
fn recent_menu(b: &mut Built, recent: &[super::system::Recent]) {
    let mut sub = Vec::new();
    for r in recent {
        b.actions.push(Action::OpenPath(r.path.clone()));
        // A file's name, not a menu label: UIA reads "R&D.txt" as shown.
        sub.push(Item::new(b.actions.len() as u32, r.name.clone()).literal());
    }
    if sub.is_empty() {
        sub.push(Item::quiet(t("topbar.system.no_recent")));
    }
    b.items.push(Item::new(0, t("topbar.system.recent")).submenu(sub));
}

/// The bar's settings and all former logo/system actions.
fn bar_menu(b: &mut Built, recent: &[super::system::Recent], pending: super::shutdown::Pending, clock: &dyn Fn(i64) -> String) {
    b.add(t("topbar.menu.bar_settings"), Action::Settings("bar"));
    b.sep();
    system_menu(b, recent, pending, clock);
}

/// The desktops (the current one checked), then new / close / Task View.
fn desktops_menu(b: &mut Built, d: &crate::vdesk::Desktops) {
    for i in 0..d.count() {
        b.add(d.name(i), Action::Desktop(i)).checked = d.current == Some(i);
    }
    b.sep();
    b.add(t("topbar.desktops.new"), Action::NewDesktop).shortcut = "Ctrl+Win+D".into();
    // Closing acts at once (owner, 2026-11-08) and carries the desktop's
    // GUID: when it runs, a different current desktop cancels it
    // (vdesk::close).
    let closable = d.current_id().filter(|_| d.current.is_some() && d.count() > 1);
    if let Some(id) = closable {
        b.add(t("topbar.desktops.close"), Action::CloseDesktop(id)).shortcut = "Ctrl+Win+F4".into();
    } else {
        // The last desktop cannot be closed; nor one we cannot identify.
        b.items.push(Item::new(0, t("topbar.desktops.close")).enabled(false));
    }
    b.sep();
    b.add(t("topbar.desktops.task_view"), Action::TaskView).shortcut = "Win+Tab".into();
    b.add(t("topbar.desktops.settings"), Action::Open("ms-settings:multitasking"));
}

/// The indicator's menu: switch 한/영, the layouts (the one in use
/// checked), then the settings. A window above our rights is only shown.
fn input_menu(b: &mut Built) {
    let (layouts, can) = crate::ime::layouts();
    let (shown, _) = crate::ime::shown();
    if !can && shown.locked {
        b.info(t("topbar.menu.locked"));
    }
    if shown.native().is_some() {
        b.add(t("topbar.panel.input.toggle"), Action::InputToggle).enabled = can;
    }
    if !b.items.is_empty() && !layouts.is_empty() {
        b.sep();
    }
    for (hkl, name, current) in layouts {
        let row = b.add(name, Action::InputLayout(hkl));
        row.checked = current;
        row.enabled = can;
    }
    b.sep();
    b.add(t("topbar.panel.input.nopbar"), Action::InputSettings);
    b.add(t("topbar.panel.input.settings"), Action::Open("ms-settings:regionlanguage"));
}

/// What the clock menu knows of the focus timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerMenu {
    pub idle: bool,
    pub running: bool,
    /// In the focus phase (else the break).
    pub focus: bool,
    pub today: u32,
}

/// "Focus timer" with its submenu: the plans to start (25 / 5, 50 / 10,
/// the custom one, by `mins`) while idle; Pause or Resume, Skip, "+5 min"
/// and Stop while one runs; today's count and the settings under them.
fn timer_menu(b: &mut Built, st: TimerMenu, mins: [(u32, u32); 3]) {
    use crate::focus_timer::{Command, Preset};
    fn add(b: &mut Built, sub: &mut Vec<Item>, label: String, a: Action) {
        b.actions.push(a);
        sub.push(Item::new(b.actions.len() as u32, label));
    }
    let mut sub = Vec::new();
    if st.idle {
        for (p, (f, br)) in [Preset::Short, Preset::Long, Preset::Custom].into_iter().zip(mins) {
            let words = tf("timer.preset", &[("f", &f.to_string()), ("b", &br.to_string())]);
            let label = if p == Preset::Custom { tf("timer.custom", &[("plan", &words)]) } else { words };
            add(b, &mut sub, label, Action::Timer(Command::Start(p)));
        }
    } else {
        add(b, &mut sub, t(if st.running { "timer.pause" } else { "timer.resume" }).to_string(), Action::Timer(Command::Toggle));
        add(b, &mut sub, t(if st.focus { "timer.skip_focus" } else { "timer.skip_break" }).to_string(), Action::Timer(Command::Skip));
        add(b, &mut sub, t("timer.plus5").to_string(), Action::Timer(Command::Extend));
        add(b, &mut sub, t("timer.stop").to_string(), Action::Timer(Command::Stop));
    }
    sub.push(Item::sep());
    sub.push(Item::info(tf("timer.today", &[("n", &st.today.to_string())])));
    add(b, &mut sub, t("timer.settings").to_string(), Action::TimerSettings);
    b.items.push(Item::new(0, t("timer.menu")).submenu(sub));
}

/// Turn off display waits this long (ms) after the menu closed: the click
/// (or key) that picked it is still being let go, and that alone would
/// wake the displays again at once.
pub const DISPLAY_OFF_DELAY_MS: u64 = 300;
/// SC_MONITORPOWER's lParam for "off" (1: low power, -1: on).
const MONITOR_OFF: isize = 2;

/// Waits DISPLAY_OFF_DELAY_MS (`wait`), then asks for the displays off
/// with `post` (msg, wParam, lParam): WM_SYSCOMMAND / SC_MONITORPOWER / 2
/// to our own window, whose DefWindowProc does it. Never a broadcast or a
/// SendMessage: no other app's window can hold it up.
fn display_off_with(wait: impl FnOnce(std::time::Duration), post: impl FnOnce(u32, usize, isize)) {
    use windows::Win32::UI::WindowsAndMessaging::{SC_MONITORPOWER, WM_SYSCOMMAND};
    wait(std::time::Duration::from_millis(DISPLAY_OFF_DELAY_MS));
    post(WM_SYSCOMMAND, SC_MONITORPOWER as usize, MONITOR_OFF);
}

/// The row "Turn off display": on a helper thread (the wait never holds
/// the bar), posted to `owner`, the bar the menu came from.
fn display_off(owner: HWND) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    if owner.is_invalid() {
        return;
    }
    // An HWND is not Send; its value is.
    let h = owner.0 as isize;
    let spawned = std::thread::Builder::new().name("display-off".into()).spawn(move || {
        display_off_with(std::thread::sleep, |msg, wp, lp| {
            if unsafe { PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(wp), LPARAM(lp)) }.is_err() {
                crate::log!("display off: not posted");
            }
        })
    });
    if spawned.is_err() {
        crate::log!("display off: no thread");
    }
}

fn open(target: &str, args: Option<&str>) {
    crate::launch::open_soon(crate::launch::Target::new(target, target, crate::launch::Source::TopBar).args(args.unwrap_or(""), ""));
}

/// Runs a picked action. Month changes are handled by the caller (they
/// reopen the calendar).
pub fn run(a: &Action, owner: HWND) {
    match a {
        Action::Open(t) => open(t, None),
        Action::Notifications => crate::keys::notifications(),
        Action::ToggleMute => super::status::toggle_mute(),
        Action::Month(..) | Action::Day(..) | Action::Reload => {}
        Action::Event(o) => crate::agenda::open(o),
        Action::Connect { iface, profile } => { let _ = wifi_connect(iface, profile); }
        Action::Lock => unsafe {
            let _ = windows::Win32::System::Shutdown::LockWorkStation();
        },
        Action::Logoff => unsafe {
            use windows::Win32::System::Shutdown::{ExitWindowsEx, EWX_LOGOFF, SHTDN_REASON_FLAG_PLANNED};
            let _ = ExitWindowsEx(EWX_LOGOFF, SHTDN_REASON_FLAG_PLANNED);
        },
        Action::Sleep => {
            super::shutdown::sleep_now();
        }
        Action::DisplayOff => display_off(owner),
        // Same as the Start menu: shutdown.exe handles privileges and hybrid boot.
        Action::Restart => open("shutdown.exe", Some("/r /t 0")),
        Action::Shutdown => open("shutdown.exe", Some("/s /hybrid /t 0")),
        Action::Schedule(kind, m) => super::shutdown::schedule(*kind, *m),
        Action::CancelSchedule => super::shutdown::cancel(),
        Action::Desktop(i) => desktop_sent("switch", crate::vdesk::go_to(*i)),
        Action::NewDesktop => desktop_sent("new", crate::vdesk::new_desktop()),
        Action::CloseDesktop(id) => desktop_sent("close", crate::vdesk::close(id)),
        Action::TaskView => desktop_sent("task view", crate::vdesk::task_view()),
        Action::InputToggle => crate::ime::toggle(),
        Action::InputLayout(hkl) => crate::ime::switch_layout(*hkl),
        Action::InputSettings => crate::settings::launch_page("input"),
        Action::Timer(c) => crate::focus_timer::command(*c),
        Action::TimerSettings => crate::settings::launch_page("topbar"),
        Action::Settings(page) => crate::settings::launch_page(page),
        // On a helper thread, like every launch: a file on a share that
        // went away never holds the bar; a failure is told (launch.rs).
        Action::OpenPath(p) => {
            let name = std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
            crate::launch::open_soon(crate::launch::Target::new(p, &name, crate::launch::Source::TopBar));
        }
        // Its bar's monitor (none: Launchpad never asks for it).
        Action::AboutPc => super::open_about(owner),
    }
}

fn desktop_sent(what: &str, s: crate::keys::Sent) {
    if s != crate::keys::Sent::Ok {
        crate::log!("desktops: {what}: not sent ({s:?})");
    }
}

/// Pictures for the design audit (tests only).
#[cfg(test)]
#[path = "panels_harmony.rs"]
mod harmony;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_ignores_an_empty_result_from_before_the_adapter_was_enabled() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let started = now - Duration::from_millis(20);
        let enabled = now - Duration::from_millis(10);
        let old = WifiResult { started, completed: now, list: Ok(vec![]) };
        assert!(old.usable(None));
        assert!(!old.usable(Some(enabled)));
        let fresh = WifiResult { started: enabled, ..old };
        assert!(fresh.usable(Some(enabled)));
    }

    #[test]
    fn a_slow_wifi_list_shows_the_ring_then_fails() {
        use std::time::Duration;
        // Nothing runs: a fetch starts. One running: waited for (the ring)
        // until WIFI_GIVE_UP, then it failed (the row with its button).
        assert_eq!(wifi_wait(None), None);
        assert_eq!(wifi_wait(Some(Duration::from_millis(400))), Some(WifiError::Pending));
        assert_eq!(wifi_wait(Some(Duration::from_secs(5))), Some(WifiError::Slow));
        // The panel waits the 300 ms rule before it opens with the ring.
        assert_eq!(WIFI_WAIT, Duration::from_millis(300));
        let mut b = Built::default();
        network_menu(&mut b, Err(WifiError::Pending));
        assert!(b.waiting && b.items[0].busy && !b.items[0].enabled);
        let mut b = Built::default();
        network_menu(&mut b, Err(WifiError::Slow));
        assert!(!b.waiting);
        let row = &b.items[0];
        assert_eq!(row.retry.as_deref(), Some(t("launch.action.retry")), "a button that asks again");
        assert_eq!(b.action(row.id), Some(&Action::Reload));
        // Windows' page is still there either way.
        assert!(matches!(b.action(b.items.last().unwrap().id), Some(Action::Open("ms-settings:network"))));
        // No networks: one quiet line.
        let mut b = Built::default();
        network_menu(&mut b, Ok(Vec::new()));
        assert!(b.items[0].quiet);
    }

    /// The design review's P2 (2026-11-11): the ring of a Wi-Fi list
    /// still waited for read as nothing (Item::busy with no name). The
    /// menu the panel builds is made as `track` makes it, and its UIA
    /// node says what it waits for, in every language.
    #[test]
    fn the_wifi_ring_tells_screen_readers_it_is_loading() {
        for lang in ["ko", "en", "ar"] {
            let _l = crate::i18n::test_lock(lang);
            let mut b = Built::default();
            network_menu(&mut b, Err(WifiError::Pending));
            assert!(b.items[0].busy);
            let nodes = crate::popup::harmony::a11y_nodes(&b.items);
            assert_eq!((nodes[0].role, nodes[0].name.as_str()), (crate::a11y::Role::Text, t("topbar.panel.network.loading")), "{lang}");
            assert!(!nodes[0].name.trim().is_empty(), "{lang}");
        }
    }

    #[test]
    fn wifi_rows_say_their_lock_and_strength() {
        // The design B1 review's P2: the lock and the bars went from the
        // label into glyphs, and screen readers lost them. The row's
        // accessible name says them after the network's name.
        let row = |signal: u32, secured: bool| crate::popup::Item { label: "Home".into(), literal: true, trail: wifi_trail(signal, secured), ..Default::default() };
        {
            let _l = crate::i18n::test_lock("en");
            assert_eq!(row(90, true).a11y_name(), "Home, Signal 4 of 4, Secured");
            assert_eq!(row(10, false).a11y_name(), "Home, Signal 1 of 4, Open network");
            // Rows without end glyphs read as before.
            let plain = crate::popup::Item { label: "Network settings…".into(), ..Default::default() };
            assert_eq!(plain.a11y_name(), "Network settings…");
        }
        let _k = crate::i18n::test_lock("ko");
        assert_eq!(row(90, true).a11y_name(), "Home, 신호 4단계 중 4, 보안 네트워크");
    }

    use super::super::system::Recent;

    fn recent() -> Vec<Recent> {
        vec![Recent { name: "plan.txt".into(), path: r"C:\Docs\plan.txt".into() }, Recent { name: "Photos".into(), path: r"C:\Users\me\Pictures\Photos".into() }]
    }

    /// The system menu with a fixed recent list (build reads the real one)
    /// and no shutdown scheduled.
    fn system(recent: &[Recent]) -> Built {
        system_with(recent, super::super::shutdown::Pending::None)
    }

    fn system_with(recent: &[Recent], pending: super::super::shutdown::Pending) -> Built {
        let mut b = Built::default();
        system_menu(&mut b, recent, pending, &|_| "9:30 PM".to_string());
        b
    }

    /// Each row as "label -> action" (dim: "(label)", a submenu: "label >"),
    /// groups by "--".
    fn shape(b: &Built, items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| {
                if i.separator {
                    "--".to_string()
                } else if !i.sub.is_empty() {
                    format!("{} >", i.label)
                } else if i.id == 0 {
                    format!("({})", i.label)
                } else {
                    format!("{} -> {:?}", i.label, b.action(i.id).unwrap())
                }
            })
            .collect()
    }

    #[test]
    fn the_system_menu_uses_windows_names() {
        let _l = crate::i18n::test_lock("en");
        let b = system(&recent());
        assert_eq!(
            shape(&b, &b.items),
            [
                "About this PC -> AboutPc",
                "--",
                "Settings -> Open(\"ms-settings:\")",
                "--",
                "Recent items >",
                "--",
                "End task -> Open(\"taskmgr.exe\")",
                "--",
                "Sleep -> Sleep",
                "Turn off display -> DisplayOff",
                "Restart -> Restart",
                "Shut down -> Shutdown",
                "Schedule shutdown >",
                "Sleep after >",
                "--",
                "Lock screen -> Lock",
                "Sign out -> Logoff",
            ]
        );
        // No facts in the menu any more (they are About's): nothing dim.
        assert!(b.items.iter().all(|i| i.separator || i.enabled));
        let hint = |label: &str| b.items.iter().find(|i| i.label == label).unwrap().shortcut.clone();
        assert_eq!(hint("Settings"), "Win+I");
        assert_eq!(hint("End task"), "Ctrl+Shift+Esc");
        assert_eq!(hint("Lock screen"), "Win+L");
        // No NopBar settings and no tools (Win+X has them): owner, 2026-11-08.
        assert!(!b.actions.iter().any(|a| matches!(a, Action::Settings(_))));
        assert!(!b.items.iter().any(|i| i.label == "Tools" || i.label == "NopBar settings…"));
        // No icons: the rows read like the system menu's.
        assert!(b.items.iter().all(|i| i.glyph.is_none()));
        // Every action is reachable from one row (ids match).
        let mut ids: Vec<u32> = b.items.iter().chain(b.items.iter().flat_map(|i| i.sub.iter())).map(|i| i.id).filter(|&id| id != 0).collect();
        ids.sort_unstable();
        assert_eq!(ids, (1..=b.actions.len() as u32).collect::<Vec<_>>());
    }

    #[test]
    fn the_system_menu_reads_in_korean() {
        let _l = crate::i18n::test_lock("ko");
        let b = system(&[]);
        let top: Vec<&str> = b.items.iter().filter(|i| !i.separator).map(|i| i.label.as_str()).collect();
        assert_eq!(top, ["이 PC 정보", "설정", "최근 항목", "작업 끝내기", "절전", "화면 끄기", "다시 시작", "시스템 종료", "예약 종료", "예약 절전", "화면 잠금", "로그아웃"]);
    }

    #[test]
    fn a_shutdown_or_sleep_is_scheduled_from_the_power_group() {
        use super::super::shutdown::{Kind, Pending};
        /// The power group, Sleep to the row before Lock screen.
        fn power(b: &Built) -> Vec<String> {
            let rows = shape(b, &b.items);
            let from = rows.iter().position(|r| r.starts_with("절전")).unwrap();
            rows[from..].iter().take_while(|r| *r != "--").cloned().collect()
        }
        let at = {
            let _l = crate::i18n::test_lock("ko");
            // Nothing scheduled: "예약 종료" and "예약 절전", each with its
            // four presets, under Shut down.
            let b = system(&[]);
            assert_eq!(power(&b), ["절전 -> Sleep", "화면 끄기 -> DisplayOff", "다시 시작 -> Restart", "시스템 종료 -> Shutdown", "예약 종료 >", "예약 절전 >"]);
            let at = b.items.iter().position(|i| i.label == "예약 종료").unwrap();
            for (row, kind) in [(&b.items[at], Kind::Shutdown), (&b.items[at + 1], Kind::Sleep)] {
                assert_eq!(row.id, 0, "a submenu");
                assert_eq!(row.sub.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["15분", "30분", "1시간", "2시간"]);
                assert_eq!(row.sub.iter().map(|i| b.action(i.id).cloned()).collect::<Vec<_>>(), [15, 30, 60, 120].map(|m| Some(Action::Schedule(kind, m))));
            }
            // A shutdown scheduled: one row in place of both that calls it
            // off at once, with the time it is due; no sleep row.
            let b = system_with(&[], Pending::At(Kind::Shutdown, 1_000));
            assert_eq!(power(&b), ["절전 -> Sleep", "화면 끄기 -> DisplayOff", "다시 시작 -> Restart", "시스템 종료 -> Shutdown", "예약 종료 취소 (9:30 PM) -> CancelSchedule"]);
            assert!(b.items[at].sub.is_empty() && !b.items[at].label.ends_with('…'));
            // A sleep scheduled: the same, as a sleep; no shutdown row.
            let b = system_with(&[], Pending::At(Kind::Sleep, 1_000));
            assert_eq!(power(&b), ["절전 -> Sleep", "화면 끄기 -> DisplayOff", "다시 시작 -> Restart", "시스템 종료 -> Shutdown", "예약 절전 취소 (9:30 PM) -> CancelSchedule"]);
            assert!(b.items[at].sub.is_empty());
            // Windows has a shutdown we did not schedule: its row, no time;
            // no sleep row either.
            let b = system_with(&[], Pending::Unknown);
            assert_eq!(power(&b), ["절전 -> Sleep", "화면 끄기 -> DisplayOff", "다시 시작 -> Restart", "시스템 종료 -> Shutdown", "예약 종료 취소 -> CancelSchedule"]);
            at
        };
        // In English.
        let _e = crate::i18n::test_lock("en");
        let b = system(&[]);
        for label in ["Schedule shutdown", "Sleep after"] {
            let row = b.items.iter().find(|i| i.label == label).unwrap();
            assert_eq!(row.sub.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["15 min", "30 min", "1 hr", "2 hr"]);
        }
        assert_eq!(system_with(&[], Pending::At(Kind::Shutdown, 5)).items[at].label, "Cancel scheduled shutdown (9:30 PM)");
        assert_eq!(system_with(&[], Pending::At(Kind::Sleep, 5)).items[at].label, "Cancel scheduled sleep (9:30 PM)");
        // Every state: each action reachable from exactly one row.
        for p in [Pending::None, Pending::At(Kind::Shutdown, 5), Pending::At(Kind::Sleep, 5), Pending::Unknown] {
            let b = system_with(&[], p);
            let mut ids: Vec<u32> = b.items.iter().chain(b.items.iter().flat_map(|i| i.sub.iter())).map(|i| i.id).filter(|&id| id != 0).collect();
            ids.sort_unstable();
            assert_eq!(ids, (1..=b.actions.len() as u32).collect::<Vec<_>>(), "{p:?}");
        }
    }

    #[test]
    fn recent_items_open_what_they_name() {
        let _l = crate::i18n::test_lock("ko");
        let b = system(&recent());
        let row = b.items.iter().find(|i| i.label == t("topbar.system.recent")).unwrap();
        assert_eq!(row.id, 0, "a submenu");
        assert_eq!(row.sub.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["plan.txt", "Photos"]);
        assert_eq!(b.action(row.sub[0].id), Some(&Action::OpenPath(r"C:\Docs\plan.txt".into())));
        assert_eq!(b.action(row.sub[1].id), Some(&Action::OpenPath(r"C:\Users\me\Pictures\Photos".into())));
        // None: one dim row that says so.
        let e = system(&[]);
        let row = e.items.iter().find(|i| i.label == t("topbar.system.recent")).unwrap();
        assert_eq!(row.sub.len(), 1);
        assert_eq!(row.sub[0].label, "최근 항목 없음");
        assert!(!row.sub[0].enabled && row.sub[0].id == 0);
    }

    #[test]
    fn recent_file_names_keep_their_ampersand_for_screen_readers() {
        let _l = crate::i18n::test_lock("en");
        let files = [Recent { name: "R&D.txt".into(), path: r"C:\Docs\R&D.txt".into() }, Recent { name: "A&&B.txt".into(), path: r"C:\Docs\A&&B.txt".into() }];
        let b = system(&files);
        let row = b.items.iter().find(|i| i.label == t("topbar.system.recent")).unwrap();
        // Read as shown: "R&D.txt" is not "RD.txt" (another file).
        assert_eq!(row.sub.iter().map(|i| i.a11y_name()).collect::<Vec<_>>(), ["R&D.txt", "A&&B.txt"]);
        assert!(row.sub.iter().all(|i| i.literal));
        // The menu's own labels still lose their access key marks.
        assert!(!row.literal);
    }

    #[test]
    fn power_rows_act_at_once() {
        let _l = crate::i18n::test_lock("en");
        let b = system(&recent());
        for (label, a) in [("topbar.system.sign_out", Action::Logoff), ("topbar.system.restart", Action::Restart), ("topbar.system.shut_down", Action::Shutdown)] {
            let row = b.items.iter().find(|i| i.label == t(label)).expect(label);
            // No submenu with a question: the row itself runs it.
            assert!(row.sub.is_empty(), "{label}");
            assert!(row.id != 0 && b.actions[row.id as usize - 1] == a, "{label}");
            // Nothing more to ask, so no "…" after the name.
            assert!(!row.label.ends_with('…') && !row.label.ends_with("..."), "{label}: {}", row.label);
        }
    }

    #[test]
    fn turn_off_display_sits_under_sleep_and_acts_at_once() {
        let _l = crate::i18n::test_lock("ko");
        let b = system(&[]);
        let at = b.items.iter().position(|i| i.label == "화면 끄기").unwrap();
        assert_eq!(b.items[at - 1].label, "절전");
        assert_eq!(b.items[at + 1].label, "다시 시작");
        let row = &b.items[at];
        assert!(row.sub.is_empty() && row.enabled && !row.label.ends_with('…'));
        assert_eq!(b.action(row.id), Some(&Action::DisplayOff));
    }

    #[test]
    fn the_display_goes_off_only_after_the_click_is_let_go() {
        use std::cell::RefCell;
        use windows::Win32::UI::WindowsAndMessaging::{SC_MONITORPOWER, WM_SYSCOMMAND};
        #[derive(Debug, PartialEq)]
        enum Step {
            Wait(u128),
            Post(u32, usize, isize),
        }
        // Fakes for the wait and the post: nothing sleeps, nothing goes dark.
        let steps = RefCell::new(Vec::new());
        display_off_with(|d| steps.borrow_mut().push(Step::Wait(d.as_millis())), |m, w, l| steps.borrow_mut().push(Step::Post(m, w, l)));
        assert_eq!(*steps.borrow(), [Step::Wait(300), Step::Post(WM_SYSCOMMAND, SC_MONITORPOWER as usize, 2)]);
        // Posted to our own window, never broadcast or sent (a hung app
        // could hold a SendMessage).
        let src = include_str!("panels.rs");
        let f = &src[src.find("fn display_off(").unwrap()..];
        let f = &f[..f.find("fn open(").unwrap()];
        assert!(f.contains("PostMessageW(") && !f.contains("HWND_BROADCAST") && !f.contains("SendMessage"));
    }

    #[test]
    fn calendar_math() {
        // 2026-11-01 is a Sunday.
        assert_eq!(first_weekday(2026, 11), 0);
        assert_eq!(first_weekday(2024, 2), 4); // Thursday
        assert_eq!(days_in(2024, 2), 29);
        assert_eq!(days_in(2026, 2), 28);
        let g = month_grid(2026, 11);
        assert_eq!(g[0], [1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(g.last().unwrap()[1], 30);
        assert_eq!(month_grid(2024, 2)[0], [0, 0, 0, 0, 1, 2, 3]);
        assert_eq!(prev_month(2026, 1), (2025, 12));
        assert_eq!(next_month(2026, 12), (2027, 1));
    }

    #[test]
    fn panels_build() {
        let _l = crate::i18n::test_lock("ko");
        let v = build(Panel::Volume, Some((40, false)), f32::INFINITY);
        assert!(v.volume_slider);
        // The slider first: no "Sound 40%" line over it.
        assert_eq!(v.items[0].slider, Some(0.4));
        assert!(!v.items.iter().any(|i| i.label.contains("40")));
        assert_eq!(v.items[0].label, t("control.volume"), "its accessible name");
        // No "Open Quick Settings (Win+A)" in the volume, network or battery panel.
        for p in [Panel::Volume, Panel::Battery] {
            let b = build(p, Some((40, false)), f32::INFINITY);
            assert!(b.items.iter().all(|i| i.shortcut != "Win+A"), "{p:?}");
        }
        let p = system(&[]);
        let off = p.items.iter().find(|i| i.label == t("topbar.system.shut_down")).unwrap();
        assert!(off.sub.is_empty(), "shutting down acts at once");
        assert_eq!(p.action(off.id), Some(&Action::Shutdown));
        let c = build(Panel::Calendar(2026, 11, 0), None, f32::INFINITY);
        assert!(c.items.iter().filter(|i| !i.cells.is_empty()).count() >= 5);
        assert_eq!([bars(80), bars(60), bars(30), bars(5)], [4, 3, 2, 1]);
        use crate::glyphs::Glyph;
        assert_eq!(wifi_trail(80, true), vec![Glyph::Wifi(4), Glyph::Lock]);
        assert_eq!(wifi_trail(10, false), vec![Glyph::Wifi(1)]);
        // No text symbols for strength or locks left in the menus.
        let src = include_str!("panels.rs");
        let code = src.find("#[cfg(test)]").map_or(src, |i| &src[..i]);
        for sym in [0x2582u32, 0x2584, 0x2586, 0x2588, 0x1F512].map(|c| char::from_u32(c).unwrap()) {
            assert!(!code.contains(sym), "{sym:?}");
        }
    }

    #[test]
    fn a_menu_never_has_a_double_space() {
        let mut b = Built::default();
        b.sep();
        assert!(b.items.is_empty(), "nothing to part yet");
        b.info("a");
        b.sep();
        b.sep();
        b.info("b");
        assert_eq!(b.items.iter().map(|i| i.separator).collect::<Vec<_>>(), [false, true, false]);
        // The calendar with or without the focus timer on it.
        for panel in [Panel::Calendar(2026, 11, 0), Panel::Calendar(2027, 2, 0), Panel::Volume, Panel::Battery] {
            let c = build(panel, Some((40, false)), f32::INFINITY);
            assert!(c.items.windows(2).all(|w| !(w[0].separator && w[1].separator)), "{panel:?}");
            assert!(!c.items[0].separator, "{panel:?}");
        }
        // The system menu, with recent items or none.
        for recent in [recent(), Vec::new()] {
            let c = system(&recent);
            assert!(c.items.windows(2).all(|w| !(w[0].separator && w[1].separator)));
            assert!(!c.items[0].separator && !c.items.last().unwrap().separator);
        }
    }

    #[test]
    fn the_calendar_opens_under_a_live_clock() {
        let _l = crate::i18n::test_lock("en");
        let c = build(Panel::Calendar(2026, 11, 0), None, f32::INFINITY);
        // The clock first, a line, then the month and its grid as before.
        assert!(c.items[0].clock && !c.items[0].enabled && c.items[0].id == 0, "never picked");
        assert!(c.items[1].separator);
        assert_eq!(c.items.iter().filter(|i| i.clock).count(), 1);
        assert_eq!(c.items[2].label, crate::i18n::date::year_month(&windows::Win32::Foundation::SYSTEMTIME { wYear: 2026, wMonth: 11, wDay: 1, ..Default::default() }));
        assert!(!c.items[3].cells.is_empty(), "the weekday header");
        // Only the weekday names may be shortened in a narrow menu; the
        // days are drawn whole (popup::grid_texts), as the day they pick.
        assert!(c.items[3].abbrev);
        let days: Vec<&crate::popup::Item> = c.items[4..].iter().take_while(|i| !i.cells.is_empty()).collect();
        assert!(days.len() >= 4 && days.iter().all(|i| !i.abbrev));
        let measure = |t: &str, size: f32| t.chars().count() as f32 * 7.0 * size / 13.0;
        let drawn = crate::popup::grid_texts(&c.items, 160.0, 7, &measure);
        for (it, texts) in c.items.iter().zip(&drawn).filter(|(i, _)| !i.cells.is_empty() && !i.abbrev) {
            assert!(it.cells.iter().zip(texts).all(|(c, (t, _))| c == t), "{:?}", it.cells);
            for (&id, (t, _)) in it.cell_ids.iter().zip(texts).filter(|(&id, _)| id != 0) {
                assert!(matches!(c.action(id), Some(&Action::Day(2026, 11, d)) if d.to_string() == *t));
            }
        }
        // Another month keeps it (it is the time now, not the month's).
        let d = build(Panel::Calendar(2027, 2, 0), None, f32::INFINITY);
        assert!(d.items[0].clock);
        // The months move from the header's ‹ ›; its title goes back to
        // today's month when another shows.
        let nav = c.items[2].nav.as_ref().expect("the month header");
        assert_eq!(c.action(nav.prev), Some(&Action::Month(2026, 10)));
        assert_eq!(c.action(nav.next), Some(&Action::Month(2026, 12)));
        let feb = d.items[2].nav.as_ref().unwrap();
        assert_eq!((d.action(feb.prev), d.action(feb.next)), (Some(&Action::Month(2027, 1)), Some(&Action::Month(2027, 3))));
        let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        assert_eq!(d.action(feb.today), Some(&Action::Month(now.wYear as i32, now.wMonth as u32)));
        assert_eq!((feb.prev_name.as_str(), feb.next_name.as_str(), feb.today_name.as_str()), ("Previous month", "Next month", "Today"));
        // No rows for them under the grid.
        for k in ["topbar.panel.calendar.previous", "topbar.panel.calendar.next", "topbar.panel.calendar.today"] {
            assert!(!c.items.iter().chain(&d.items).any(|i| i.label == t(k)), "{k}");
        }
        // It says the time with seconds and the long date.
        let st = windows::Win32::Foundation::SYSTEMTIME { wYear: 2026, wMonth: 11, wDay: 5, wHour: 21, wMinute: 4, wSecond: 9, ..Default::default() };
        let (time, date) = crate::popup::clock_lines(&st);
        assert!(time.contains("09") && time.contains("PM"), "{time}");
        assert!(date.contains("2026") && date.contains("November"), "{date}");
        // Made wide enough for two-digit hours both sides of noon.
        let [am, pm] = crate::popup::clock_samples(&st);
        assert_eq!((am.wHour, pm.wHour, am.wDay, pm.wYear), (10, 22, 5, 2026));
    }

    use crate::agenda::ics::Event;
    use crate::agenda::{Open, Shown, Snapshot};

    /// A day in a fixed zone (UTC+9, no daylight time): 2026-11-05.
    const NINE: i64 = 9 * 3600;
    fn to_utc(l: i64) -> i64 {
        l - NINE
    }
    fn to_local(u: i64) -> i64 {
        u + NINE
    }
    fn today() -> i64 {
        crate::agenda::tz::days_from_civil(2026, 11, 5)
    }
    /// UTC seconds of hh:mm local, `d` days from today.
    fn at(d: i64, h: i64, m: i64) -> i64 {
        to_utc((today() + d) * 86_400 + h * 3600 + m * 60)
    }
    fn timed(s: i64, e: i64, title: &str, place: &str) -> Shown {
        Shown { ev: Event { start: s, end: e, title: title.into(), location: place.into(), ..Default::default() }, open: Open::Url("https://calendar.google.com/calendar/r/day/2026/11/5".into()) }
    }
    fn all_day(d: i64, title: &str) -> Shown {
        let day = today() + d;
        Shown { ev: Event { start: day * 86_400, end: (day + 1) * 86_400, all_day: true, title: title.into(), ..Default::default() }, open: Open::Windows(0) }
    }
    fn snap(mut events: Vec<Shown>) -> Snapshot {
        events.sort_by_key(|s| s.ev.start);
        Snapshot { enabled: true, has_source: true, tomorrow: true, events, from: at(-35, 0, 0), to: at(70, 0, 0), error: false, loading: false }
    }
    fn rows_of(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match &i.event {
                Some(e) => format!("{}{} {}{}", if e.now { "*" } else { "" }, e.time, e.title, if e.past { " (dim)" } else { "" }),
                None => format!("[{}]", i.label),
            })
            .collect()
    }

    #[test]
    fn the_agenda_shows_what_is_left_today_then_tomorrow() {
        let _l = crate::i18n::test_lock("en");
        let clock = Now { utc: at(0, 10, 0), today: today(), to_utc: &to_utc, to_local: &to_local };
        let s = snap(vec![
            timed(at(0, 8, 0), at(0, 9, 0), "Breakfast", ""),
            timed(at(0, 9, 30), at(0, 10, 30), "Standup", "Room 4"),
            timed(at(0, 14, 0), at(0, 15, 0), "Review", ""),
            timed(at(1, 9, 0), at(1, 10, 0), "Dentist", "Main St"),
            timed(at(2, 9, 0), at(2, 10, 0), "Later", ""),
        ]);
        let mut b = Built::default();
        let rows = agenda_section(&mut b, &s, &clock, today(), 7);
        let shown = rows_of(&rows);
        assert_eq!(shown.len(), 4, "{shown:?}");
        // What is over is left out; what is on now is marked.
        assert!(!shown.iter().any(|r| r.contains("Breakfast")), "{shown:?}");
        assert!(shown[0].starts_with('*') && shown[0].contains("Standup"), "{shown:?}");
        assert!(shown[1].contains("Review") && !shown[1].starts_with('*'));
        // Two left today: tomorrow's under its heading, not the day after.
        assert_eq!(shown[2], "[Tomorrow]");
        assert!(shown[3].contains("Dentist"));
        assert!(!shown.iter().any(|r| r.contains("Later")));
        // Each row is a menu item read as "time, title, place" that opens its event.
        assert!(rows[0].label.contains("Standup") && rows[0].label.ends_with("Room 4"), "{}", rows[0].label);
        assert!(matches!(b.action(rows[0].id), Some(Action::Event(Open::Url(u))) if u.starts_with("https://calendar.google.com/")));
        // Tomorrow off: today only.
        let mut b = Built::default();
        let rows = agenda_section(&mut b, &Snapshot { tomorrow: false, ..s.clone() }, &clock, today(), 7);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn the_agenda_keeps_to_its_room() {
        let _l = crate::i18n::test_lock("en");
        let clock = Now { utc: at(0, 7, 0), today: today(), to_utc: &to_utc, to_local: &to_local };
        let mut events: Vec<Shown> = (8..16).map(|h| timed(at(0, h, 0), at(0, h, 30), &format!("E{h}"), "")).collect();
        events.push(all_day(0, "Holiday"));
        let s = snap(events);
        // All-day first, at most AGENDA_EVENTS events, then "N more…".
        let mut b = Built::default();
        let shown = rows_of(&agenda_section(&mut b, &s, &clock, today(), 7));
        assert_eq!(shown[0], "All day Holiday");
        assert_eq!(shown.iter().filter(|r| !r.starts_with('[')).count(), AGENDA_EVENTS - 1);
        assert_eq!(shown.last().unwrap(), "[and 5 more]", "{shown:?}");
        // Two lines only (a small screen): one event and the rest counted.
        let mut b = Built::default();
        let shown = rows_of(&agenda_section(&mut b, &s, &clock, today(), 2));
        assert_eq!(shown.len(), 2, "{shown:?}");
        assert_eq!(shown[1], "[and 8 more]");
        // One line: still says something.
        let mut b = Built::default();
        assert_eq!(agenda_section(&mut b, &s, &clock, today(), 1).len(), 1);
    }

    #[test]
    fn the_agenda_says_why_it_is_empty() {
        let _l = crate::i18n::test_lock("en");
        let clock = Now { utc: at(0, 20, 0), today: today(), to_utc: &to_utc, to_local: &to_local };
        // No calendar: no agenda at all (no "Add a calendar in Settings…").
        let mut b = Built::default();
        let rows = agenda_section(&mut b, &Snapshot { enabled: true, ..Default::default() }, &clock, today(), 7);
        assert!(rows.is_empty() && b.actions.is_empty());
        // Everything over: nothing more today (tomorrow has nothing).
        let s = snap(vec![timed(at(0, 8, 0), at(0, 9, 0), "Breakfast", "")]);
        let mut b = Built::default();
        assert_eq!(rows_of(&agenda_section(&mut b, &s, &clock, today(), 7)), ["[No more events today]"]);
        // A failed fetch: a dim note under what is known.
        let mut b = Built::default();
        let shown = rows_of(&agenda_section(&mut b, &Snapshot { error: true, ..s.clone() }, &clock, today(), 7));
        assert_eq!(shown.last().unwrap(), &format!("[{}]", t("agenda.error")));
        // The first fetch on its way.
        let mut b = Built::default();
        let shown = rows_of(&agenda_section(&mut b, &Snapshot { loading: true, events: Vec::new(), from: 0, to: 0, ..s.clone() }, &clock, today(), 7));
        assert_eq!(shown, [format!("[{}]", t("agenda.loading"))]);
        // Nothing fetched and the fetch failed: only the note.
        let mut b = Built::default();
        let shown = rows_of(&agenda_section(&mut b, &Snapshot { error: true, events: Vec::new(), from: 0, to: 0, ..s.clone() }, &clock, today(), 7));
        assert_eq!(shown, [format!("[{}]", t("agenda.error"))]);
        // A day far away is not loaded: nothing under the month (no
        // sentence about what is loaded), not even its date.
        let mut b = Built::default();
        assert!(agenda_section(&mut b, &s, &clock, today() + 200, 7).is_empty());
    }

    #[test]
    fn a_picked_day_shows_its_events() {
        let _l = crate::i18n::test_lock("en");
        let clock = Now { utc: at(0, 12, 0), today: today(), to_utc: &to_utc, to_local: &to_local };
        let s = snap(vec![
            timed(at(-1, 9, 0), at(-1, 10, 0), "Yesterday", ""),
            // Overnight: on both days, as all day on the second.
            timed(at(-1, 22, 0), at(0, 2, 0), "Night shift", ""),
            all_day(-1, "Trip"),
        ]);
        let mut b = Built::default();
        let shown = rows_of(&agenda_section(&mut b, &s, &clock, today() - 1, 7));
        // The day's heading first, then its events, dim (over), all-day first.
        assert!(shown[0].starts_with('[') && shown[0].contains("November") && shown[0].contains('4'), "{shown:?}");
        assert_eq!(shown[1], "All day Trip (dim)");
        assert!(shown[2].contains("Yesterday") && shown[2].ends_with("(dim)"));
        assert!(shown[3].contains("Night shift"));
        assert_eq!(day_events(&s.events, today(), &clock).len(), 0, "over by 02:00 today");
        let early = Now { utc: at(0, 1, 0), today: today(), to_utc: &to_utc, to_local: &to_local };
        let on = day_events(&s.events, today(), &early);
        assert_eq!(on.len(), 1);
        assert!(on[0].all_day && !on[0].now, "started the day before: shown as all day, no mark");
    }

    #[test]
    fn an_open_agenda_knows_when_it_changes() {
        let clock = Now { utc: at(0, 9, 59), today: today(), to_utc: &to_utc, to_local: &to_local };
        let s = snap(vec![timed(at(0, 10, 0), at(0, 10, 30), "Standup", ""), timed(at(0, 8, 0), at(0, 9, 0), "Past", ""), all_day(0, "Holiday")]);
        // The meeting starts: it gets its mark.
        assert_eq!(next_change(&s.events, &clock), at(0, 10, 0));
        // On: it ends (then it leaves the list).
        let on = Now { utc: at(0, 10, 5), today: today(), to_utc: &to_utc, to_local: &to_local };
        assert_eq!(next_change(&s.events, &on), at(0, 10, 30));
        // Nothing more: the day turns.
        let late = Now { utc: at(0, 11, 0), today: today(), to_utc: &to_utc, to_local: &to_local };
        assert_eq!(next_change(&s.events, &late), at(1, 0, 0));
    }

    #[test]
    fn the_agenda_fits_the_work_area() {
        let base = system(&recent()).items;
        // Small, Default and Large text: the clock row's height counts.
        for scale in crate::fonts::TEXT_SCALES {
            let clock = crate::popup::clock_text_h(scale);
            assert_eq!(agenda_lines(&base, f32::INFINITY, clock), 7);
            let (_, h) = crate::popup::row_layout(&base, crate::popup::Density::Normal, clock);
            // Room for three more agenda cards and a gap at the normal density.
            assert_eq!(agenda_lines(&base, h + 9.0 + 3.0 * 56.0, clock), 3);
            // Less: the compact rows the popup then uses.
            let (_, hc) = crate::popup::row_layout(&base, crate::popup::Density::Compact, clock);
            assert_eq!(agenda_lines(&base, hc + 7.0 + 2.0 * 48.0, clock), 2);
            assert_eq!(agenda_lines(&base, 10.0, clock), 1, "never nothing");
        }
    }

    #[test]
    fn the_agenda_counts_the_clock_row_the_popup_measures() {
        use crate::popup::{fit, row_layout, Density, EventRow};
        // A calendar's top (the clock row and its line) over other rows.
        let mut base = vec![Item::clock(), Item::sep()];
        base.extend(system(&recent()).items);
        let event = || Item { event: Some(EventRow { time: "09:30".into(), title: "Standup".into(), ..Default::default() }), ..Default::default() };
        let menu = |n: usize| {
            let mut m = base.clone();
            m.push(Item::sep());
            m.extend((0..n).map(|_| event()));
            m
        };
        // A font with tall lines (Javanese Text: 2.27 em each) measures the
        // clock well over the 1.36 em estimate.
        let estimate = crate::popup::clock_text_h(1.0);
        let measured = (48.0f32 * 2.2705).ceil() + 2.0 + (16.0f32 * 2.2705).ceil();
        assert!(measured > estimate + 40.0);
        // Room for exactly three cards under the measured clock row: three,
        // and the whole menu then fits at the normal density as the popup
        // lays it out. (The estimate would have asked for one more, which
        // pushes the popup into compact rows or scrolling.)
        let room = row_layout(&base, Density::Normal, measured).1 + Density::Normal.sep_h() + 3.0 * Density::Normal.event_h();
        assert_eq!(agenda_lines(&base, room, measured), 3);
        assert_eq!(fit(menu(3), room, measured).1, Density::Normal);
        assert!(agenda_lines(&base, room, estimate) > 3, "the estimate overcounts");
        assert_ne!(fit(menu(agenda_lines(&base, room, estimate)), room, measured).1, Density::Normal);
        // A short font: what fits is not sent to "more" either.
        let short = estimate - 12.0;
        let room = row_layout(&base, Density::Normal, short).1 + Density::Normal.sep_h() + 4.0 * Density::Normal.event_h();
        assert_eq!(agenda_lines(&base, room, short), 4);
        assert_eq!(fit(menu(4), room, short).1, Density::Normal);
        // The measured height is asked for only when the agenda needs it.
        let asked = std::cell::Cell::new(0);
        let _ = build_measured(Panel::Bar, None, 500.0, &|| {
            asked.set(asked.get() + 1);
            measured
        });
        assert_eq!(asked.get(), 0);
    }

    #[test]
    fn the_bar_menu_preserves_every_former_system_action() {
        let _l = crate::i18n::test_lock("ko");
        let mut b = Built::default();
        let mut former = Built::default();
        let pending = super::super::shutdown::Pending::None;
        let recent = [super::super::system::Recent { name: "memo.txt".into(), path: "C:\\memo.txt".into() }];
        bar_menu(&mut b, &recent, pending, &|_| String::new());
        system_menu(&mut former, &recent, pending, &|_| String::new());
        assert_eq!(b.action(b.items[0].id), Some(&Action::Settings("bar")));
        assert_eq!(&b.actions[1..], former.actions.as_slice());
    }

    #[test]
    fn timer_menu_starts_or_controls() {
        use crate::focus_timer::{Command, Preset};
        let _l = crate::i18n::test_lock("en");
        let mins = [(25, 5), (50, 10), (40, 8)];
        let mut b = Built::default();
        timer_menu(&mut b, TimerMenu { idle: true, running: false, focus: true, today: 3 }, mins);
        let menu = &b.items[0];
        assert_eq!((menu.label.as_str(), menu.id), ("Focus timer", 0), "a submenu");
        assert_eq!(menu.sub[0].label, "Focus 25 min · break 5 min");
        assert_eq!(b.action(menu.sub[0].id), Some(&Action::Timer(Command::Start(Preset::Short))));
        assert_eq!(b.action(menu.sub[1].id), Some(&Action::Timer(Command::Start(Preset::Long))));
        assert!(menu.sub[2].label.contains("40"));
        assert_eq!(b.action(menu.sub[2].id), Some(&Action::Timer(Command::Start(Preset::Custom))));
        let today = menu.sub.iter().find(|i| i.label == "Sessions today: 3").unwrap();
        assert!(!today.enabled);
        assert_eq!(b.action(menu.sub.last().unwrap().id), Some(&Action::TimerSettings));
        // Running: the controls instead.
        let mut r = Built::default();
        timer_menu(&mut r, TimerMenu { idle: false, running: true, focus: true, today: 0 }, mins);
        let sub = &r.items[0].sub;
        assert_eq!((sub[0].label.as_str(), r.action(sub[0].id)), ("Pause", Some(&Action::Timer(Command::Toggle))));
        assert_eq!(r.action(sub[1].id), Some(&Action::Timer(Command::Skip)));
        assert_eq!(r.action(sub[2].id), Some(&Action::Timer(Command::Extend)));
        assert_eq!(r.action(sub[3].id), Some(&Action::Timer(Command::Stop)));
        let mut p = Built::default();
        timer_menu(&mut p, TimerMenu { idle: false, running: false, focus: false, today: 0 }, mins);
        assert_eq!(p.items[0].sub[0].label, "Resume");
        assert_eq!(p.items[0].sub[1].label, t("timer.skip_break"));
    }

    #[test]
    fn desktops_menu_lists_and_closes_at_once() {
        let _l = crate::i18n::test_lock("en");
        let d = crate::vdesk::Desktops { ids: vec![[1; 16], [2; 16], [3; 16]], names: vec![None, Some("Work".into()), None], current: Some(1) };
        let mut b = Built::default();
        desktops_menu(&mut b, &d);
        let rows: Vec<&Item> = b.items.iter().filter(|i| !i.separator).collect();
        assert_eq!(rows[0].label, "Desktop 1");
        assert_eq!(rows[1].label, "Work");
        assert!(rows[1].checked && !rows[0].checked && !rows[2].checked);
        assert_eq!(b.action(rows[2].id), Some(&Action::Desktop(2)));
        // One row that closes the current desktop: no submenu asking again.
        let close = rows.iter().find(|i| i.label == t("topbar.desktops.close")).unwrap();
        assert!(close.sub.is_empty() && close.enabled, "closing acts at once");
        assert_eq!(b.action(close.id), Some(&Action::CloseDesktop([2; 16])), "the shown desktop's id");
        assert_eq!(close.shortcut, "Ctrl+Win+F4");
        // Nothing in the menu has a submenu.
        assert!(b.items.iter().all(|i| i.sub.is_empty()));
        // Current desktop unknown: nothing to close.
        let mut unknown = Built::default();
        desktops_menu(&mut unknown, &crate::vdesk::Desktops { current: None, ..d.clone() });
        let close = unknown.items.iter().find(|i| i.label == t("topbar.desktops.close")).unwrap();
        assert!(!close.enabled && close.sub.is_empty());
        // One desktop: nothing to close.
        let mut one = Built::default();
        desktops_menu(&mut one, &crate::vdesk::Desktops::default());
        let close = one.items.iter().find(|i| i.label == t("topbar.desktops.close")).unwrap();
        assert!(!close.enabled && close.sub.is_empty());
    }
}
