//! The focus timer (Pomodoro): a focus phase (25, 50 or the user's own
//! minutes) and the break after it, shown on the Live Island as a ring
//! counting down ("Focus 18:42"), with Pause / Resume / Skip / Stop and
//! "+5 min" on the island's card, in the clock menu and on a shortcut.
//!
//! Started from the clock menu (time things live there, and it needs no new
//! cell on the bar) or the "focus_timer" shortcut (off by default: it
//! starts the last plan, or pauses / resumes a running one). Once running
//! the island's card holds the controls.
//!
//! Time: every deadline is absolute (wall clock, Unix ms; model.rs), so a
//! PC that sleeps through one catches up in one step. One waitable timer on
//! the service thread (crate::service) is set to the next deadline as an
//! absolute time, which Windows fires on resume if it passed meanwhile: no
//! polling and no tick counting. The island draws its own seconds while it
//! shows the timer; its ring is a compositor animation (topbar/island).
//!
//! At the end of a phase: a gentle chime (Windows' reminder sound, which
//! the system mute silences; skipped while Windows says notifications are
//! quiet — Focus, presentation, full screen), the island changes with a
//! pop, and optionally a toast. A focus phase that runs to its end while
//! the PC is awake counts for today (focus_timer.json, per local day); one
//! that ended during a sleep is neither counted nor announced on wake. "Turn on Do not disturb"
//! opens Windows Clock and enables Do not disturb for the focus phase.
//! Focus start is a Limited Access Feature, so an isolated QuietHours
//! compatibility adapter provides DND when that contract is available.

pub mod model;
mod dnd;
mod lease;
pub(crate) use dnd::{ours as owns_dnd, resume_after_cancel, set_manually as set_dnd_manually};
pub(crate) mod quiet_hours;

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};

use crate::config::Config;
use crate::service::{self, Next, Wake, Watch};
use model::{Count, Event, Outcome, Phase, Plan, Timer};

/// RegisterHotKey id of the "focus_timer" shortcut (on the dock window).
pub const HOTKEY_ID: i32 = 0x5446;

/// The plans offered (Settings > Top bar holds the custom one).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    #[default]
    Short,
    Long,
    Custom,
}

/// What the clock menu, the card and the shortcut can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Start(Preset),
    /// Pause a running phase, resume a paused one.
    Toggle,
    Skip,
    Extend,
    Stop,
}

/// The settings it follows (Config::focus_timer).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cfg {
    custom: Plan,
    dnd: bool,
    chime: bool,
    toast: bool,
    /// Offered in the clock menu (focus_timer.enabled).
    in_menu: bool,
}

impl Cfg {
    const DEFAULT: Cfg = Cfg { custom: Plan::minutes(40, 8), dnd: true, chime: true, toast: false, in_menu: true };
}

struct Inner {
    timer: Timer,
    count: Count,
    /// The plan last started (the shortcut starts it again).
    last: Preset,
    cfg: Cfg,
    /// The watch while one is added (it ends when the timer is idle).
    watch: Option<service::Id>,
    /// focus_timer.json was read.
    loaded: bool,
}

static STATE: Mutex<Inner> = Mutex::new(Inner { timer: Timer { session: None, notice_until: None }, count: Count { day: 0, n: 0 }, last: Preset::Short, cfg: Cfg::DEFAULT, watch: None, loaded: false });

fn state() -> std::sync::MutexGuard<'static, Inner> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wall clock, Unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Today as yyyymmdd (local time).
fn today() -> u32 {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    t.wYear as u32 * 10_000 + t.wMonth as u32 * 100 + t.wDay as u32
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// yyyymmdd of days since 1970-01-01.
fn civil_from_days(z: i64) -> u32 {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y * 10_000 + m * 100 + d) as u32
}

/// The local day (yyyymmdd) of Unix ms `t`, with the offset from UTC in
/// force now (an end and its notice are minutes apart).
fn local_day(t: i64) -> u32 {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let ms = |s: windows::Win32::Foundation::SYSTEMTIME| {
        days_from_civil(s.wYear as i64, s.wMonth as i64, s.wDay as i64) * 86_400_000 + s.wHour as i64 * 3_600_000 + s.wMinute as i64 * 60_000 + s.wSecond as i64 * 1000
    };
    let offset = unsafe { ms(GetLocalTime()) - ms(GetSystemTime()) };
    civil_from_days((t + offset).div_euclid(86_400_000))
}

// ---------------------------------------------------------------- saved

/// focus_timer.json: today's count and the last plan.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Saved {
    count: Count,
    last: Preset,
}

fn file() -> std::path::PathBuf {
    crate::config::app_dir().join("focus_timer.json")
}

fn load(inner: &mut Inner) {
    if inner.loaded {
        return;
    }
    inner.loaded = true;
    if let Some(s) = std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str::<Saved>(&t).ok()) {
        inner.count = s.count;
        inner.last = s.last;
    }
}

fn save(inner: &Inner) {
    let s = Saved { count: inner.count, last: inner.last };
    let Ok(text) = serde_json::to_string_pretty(&s) else { return };
    let path = file();
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

// ---------------------------------------------------------------- config

/// The settings changed (dock UI thread).
pub fn apply_config(c: &Config) {
    let f = &c.focus_timer;
    let cfg = Cfg { custom: Plan::minutes(f.custom_focus.clamp(1, 360), f.custom_break.min(120)), dnd: f.dnd, chime: f.chime, toast: f.toast, in_menu: f.enabled };
    {
        let mut g = state();
        g.cfg = cfg;
    }
    dnd::kick();
}

/// What Do not disturb should be now: the running focus phase's deadline,
/// or NONE (read by the helper when it runs, under the state's lock).
fn dnd_wish() -> i64 {
    dnd_wanted(&state()).unwrap_or(dnd::NONE)
}

fn plan_of(inner: &Inner, p: Preset) -> Plan {
    match p {
        Preset::Short => model::SHORT,
        Preset::Long => model::LONG,
        Preset::Custom => inner.cfg.custom,
    }
}

/// (focus, break) minutes of a preset, for the menus.
pub fn minutes(p: Preset) -> (u32, u32) {
    let g = state();
    let plan = plan_of(&g, p);
    ((plan.focus_ms / 60_000) as u32, (plan.break_ms / 60_000) as u32)
}

// ---------------------------------------------------------------- commands

/// Runs a command (any thread; the UI never waits on anything but the lock).
pub fn command(c: Command) {
    let now = now_ms();
    let mut g = state();
    load(&mut g);
    let e = match c {
        Command::Start(p) => {
            g.last = p;
            Event::Start(plan_of(&g, p))
        }
        Command::Toggle => {
            if g.timer.running() {
                Event::Pause
            } else {
                Event::Resume
            }
        }
        Command::Skip => Event::Skip,
        Command::Extend => Event::Extend,
        Command::Stop => Event::Stop,
    };
    let start=matches!(c,Command::Start(_));
    let clock = start && g.cfg.dnd;
    let out = g.timer.apply(e, now);
    if start {crate::companion::cue(crate::companion::Cue::FocusStart);}
    if matches!(c, Command::Start(_)) {
        save(&g);
    }
    // Skipping a break by hand ends the timer without a chime.
    settle(g, out, false, c != Command::Skip);
    if clock && !cfg!(test) {
        crate::launch::open_soon(crate::launch::Target::new("ms-clock:", "Clock", crate::launch::Source::TopBar));
    }
}

/// The shortcut: starts the last plan, else pauses or resumes.
pub fn on_hotkey() {
    let busy = {
        let mut g = state();
        load(&mut g);
        g.timer.session.is_some()
    };
    if busy {
        command(Command::Toggle);
    } else {
        let last = state().last;
        command(Command::Start(last));
    }
}

/// After the state changed (lock held): counts, sounds, Do not disturb,
/// the watch and the island. `from_watch`: called by the watch itself
/// (it arms its timer on its own). `sound`: an end is said (not for an
/// end the user just asked for).
fn settle(mut g: std::sync::MutexGuard<'static, Inner>, out: Vec<Outcome>, from_watch: bool, sound: bool) {
    let now = now_ms();
    // A focus phase counts only when it ended while the PC was awake, on
    // today's date (model::counts): one that ended during a sleep is not
    // counted and is not announced on wake.
    let mut counted = false;
    for o in &out {
        if let Outcome::FocusDone(at) = o {
            if model::counts(*at, now, local_day) {
                g.count.add(today());
                counted = true;
            } else {
                crate::log!("focus timer: a focus phase ended while the PC slept; not counted");
            }
        }
    }
    if counted {
        save(&g);
    }
    // What to say: the last thing that ended, if it ended just now (an
    // end found on waking from sleep is not announced).
    let fresh = |at: i64| now - at <= model::LATE_MS;
    let said = out.iter().rev().find_map(|o| match o {
        Outcome::FocusDone(at) => Some((Phase::Focus, fresh(*at))),
        Outcome::BreakDone => Some((Phase::Break, g.timer.notice_until.is_some_and(|u| fresh(u - model::NOTICE_MS)))),
        _ => None,
    });
    crate::companion::calm(crate::companion::Calm::Focus,g.timer.session.is_some_and(|s|s.phase==Phase::Focus)&&g.timer.running());
    if let Some((phase,true))=said {crate::companion::cue(if phase==Phase::Focus {crate::companion::Cue::BreakStart}else{crate::companion::Cue::BreakEnd});}
    let break_min = g.timer.session.map(|s| (s.total / 60_000) as u32).unwrap_or(0);
    let (chime, toast) = (g.cfg.chime, g.cfg.toast);
    if !from_watch {
        match g.watch {
            // Re-armed for the new deadline.
            Some(id) => service::poke(id),
            // A watch while there is anything to wait for (it ends itself
            // when the timer is idle). Added under the lock: one at a time.
            None if !g.timer.idle() => g.watch = service::add(Box::new(TimerWatch::new())),
            None => {}
        }
    }
    drop(g);
    // The helper reads the wish from the state itself when it runs (never
    // a value computed here that a later change could overtake).
    dnd::kick();
    if sound {
        if let Some((phase, true)) = said {
            alert(phase, break_min, chime, toast);
        }
    }
    crate::topbar::island::refresh();
}

/// The phase that ended is said: a toast when asked (it has its own sound),
/// else the chime.
fn alert(ended: Phase, break_min: u32, chime: bool, toast: bool) {
    use crate::i18n::{t, tf};
    crate::log!("focus timer: {} over", if ended == Phase::Focus { "focus" } else { "break" });
    if toast {
        let (title, body) = match ended {
            Phase::Focus => (t("timer.toast.focus_done").to_string(), tf("timer.toast.break_for", &[("b", &break_min.to_string())])),
            Phase::Break => (t("timer.break_over").to_string(), t("timer.toast.next").to_string()),
        };
        crate::updates::notice(title, body);
        return;
    }
    if chime {
        let ours = dnd::ours();
        let _ = std::thread::Builder::new().name("chime".into()).spawn(move || play_chime(ours));
    }
}

/// Windows' reminder sound, as a system sound (the system mute and the
/// system sounds volume apply). Not while Windows keeps notifications
/// quiet, unless that quiet is our own Do not disturb ending now.
fn play_chime(ours: bool) {
    use windows::core::w;
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_NODEFAULT, SND_SYSTEM};
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_ACCEPTS_NOTIFICATIONS};
    let accepts = matches!(unsafe { SHQueryUserNotificationState() }, Ok(s) if s == QUNS_ACCEPTS_NOTIFICATIONS);
    let focus_on = !ours && crate::notification_state::quiet();
    if (!accepts && !ours) || focus_on {
        crate::log!("focus timer: quiet now, no chime");
        return;
    }
    unsafe {
        // On this helper thread: played to the end, then the thread goes.
        let _ = PlaySoundW(w!("Notification.Reminder"), None, SND_ALIAS | SND_NODEFAULT | SND_SYSTEM);
    }
}

/// The focus phase's deadline when Do not disturb should be on.
fn dnd_wanted(g: &Inner) -> Option<i64> {
    let s = g.timer.session?;
    match (g.cfg.dnd, s.phase, s.run) {
        (true, Phase::Focus, model::Run::Running { deadline }) => Some(deadline),
        _ => None,
    }
}

// ---------------------------------------------------------------- the island

/// What the island shows of the timer now.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct View {
    /// 0 focus, 1 break, 2 the break is over (a short notice).
    pub phase: u8,
    pub left_ms: i64,
    pub total_ms: i64,
    pub running: bool,
    /// Wall-clock end (Unix ms; 0 when paused or over).
    pub deadline: i64,
    pub today: u32,
    /// Do not disturb was asked for and Windows does not offer it here.
    pub no_dnd: bool,
}

/// The timer for the island (None: nothing to show).
pub fn view(now: i64) -> Option<View> {
    let mut g = state();
    load(&mut g);
    let today = g.count.today(self::today());
    let no_dnd = g.cfg.dnd && dnd::unsupported();
    if let Some(s) = g.timer.session {
        let (running, deadline) = match s.run {
            model::Run::Running { deadline } => (true, deadline),
            model::Run::Paused { .. } => (false, 0),
        };
        return Some(View { phase: if s.phase == Phase::Focus { 0 } else { 1 }, left_ms: g.timer.left(now), total_ms: s.total.max(1), running, deadline, today, no_dnd: no_dnd && s.phase == Phase::Focus });
    }
    g.timer.notice_until.filter(|u| now < *u).map(|_| View { phase: 2, left_ms: 0, total_ms: 1, running: false, deadline: 0, today, no_dnd: false })
}

/// For the clock menu: (idle, running, phase is focus, today's count).
/// The clock menu offers the timer: switched on in Settings, or one runs
/// (a running timer can always be paused or stopped).
pub fn in_menu() -> bool {
    let g = state();
    g.cfg.in_menu || g.timer.session.is_some()
}

pub fn menu_state() -> (bool, bool, bool, u32) {
    let mut g = state();
    load(&mut g);
    let s = g.timer.session;
    (s.is_none(), g.timer.running(), s.is_none_or(|s| s.phase == Phase::Focus), g.count.today(today()))
}

/// NopBar is quitting: our Do not disturb ends before the process does
/// (waited for, up to a moment; Windows also ends it at the deadline it
/// was started with).
pub fn shutdown() {
    release_hotkey();
    dnd::shutdown(std::time::Duration::from_millis(1500));
}

// ---------------------------------------------------------------- the watch

/// FILETIME (UTC, 100 ns since 1601) of a Unix ms time.
pub fn filetime_of(unix_ms: i64) -> i64 {
    unix_ms.max(0) * 10_000 + 116_444_736_000_000_000
}

/// One waitable timer on the service thread, set to the next deadline as
/// an absolute time (it fires on resume when a sleep passed it).
struct TimerWatch {
    h: isize,
}

impl TimerWatch {
    fn new() -> Self {
        use windows::Win32::System::Threading::CreateWaitableTimerExW;
        // Synchronization (auto-reset) timer, every access.
        let h = unsafe { CreateWaitableTimerExW(None, windows::core::PCWSTR::null(), 0, 0x001F_0003) }.map(|h| h.0 as isize).unwrap_or(0);
        if h == 0 {
            crate::log!("focus timer: no waitable timer");
        }
        TimerWatch { h }
    }

    fn arm(&self, at: Option<i64>) {
        use windows::Win32::System::Threading::{CancelWaitableTimer, SetWaitableTimer};
        if self.h == 0 {
            return;
        }
        let h = HANDLE(self.h as *mut _);
        unsafe {
            match at {
                Some(ms) => {
                    // Positive: an absolute UTC time.
                    let due = filetime_of(ms);
                    if let Err(e) = SetWaitableTimer(h, &due, 0, None, None, false) {
                        crate::log!("focus timer: timer not set ({e})");
                    }
                }
                None => {
                    let _ = CancelWaitableTimer(h);
                }
            }
        }
    }
}

impl Drop for TimerWatch {
    fn drop(&mut self) {
        if self.h != 0 {
            unsafe {
                let _ = CloseHandle(HANDLE(self.h as *mut _));
            }
        }
    }
}

impl Watch for TimerWatch {
    fn name(&self) -> &'static str {
        "focus-timer"
    }

    fn handles(&self) -> Vec<HANDLE> {
        if self.h == 0 { Vec::new() } else { vec![HANDLE(self.h as *mut _)] }
    }

    fn wake(&mut self, why: Wake) -> Next {
        let now = now_ms();
        let mut g = state();
        let out = g.timer.advance(now);
        if g.timer.idle() {
            g.watch = None;
            self.arm(None);
            drop(g);
            if out.is_empty() {
                // The notice's time is over: the island lets it go.
                crate::topbar::island::refresh();
            } else {
                settle(state(), out, true, true);
            }
            return Next::Done;
        }
        let next = g.timer.next_wake();
        self.arm(next);
        // No timer handle (creation failed): the service's own deadline,
        // which does not count sleep, is the fallback.
        let fallback = (self.h == 0).then(|| next.map(|n| std::time::Duration::from_millis((n - now).max(1) as u64))).flatten();
        if !out.is_empty() || why == Wake::Signaled(0) {
            settle(g, out, true, true);
        }
        match fallback {
            Some(d) => Next::Sleep(d),
            None => Next::Wait,
        }
    }
}

// ---------------------------------------------------------------- the shortcut

static HOTKEY: Mutex<Option<(isize, String, bool)>> = Mutex::new(None);

/// Shortcut "focus_timer" (crate::shortcuts): registers `spec` on `owner`.
pub fn apply_hotkey(owner: HWND, spec: &str) {
    use crate::hotkeys::{record, Status, FOCUS_TIMER};
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT};
    let Ok(mut cur) = HOTKEY.lock() else { return };
    let me = owner.0 as isize;
    if cur.as_ref().is_some_and(|(o, s, _)| *o == me && s == spec) {
        return;
    }
    if let Some((old, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), HOTKEY_ID);
        }
    }
    let ok = match crate::search::hotkey::parse(spec) {
        Ok(None) => {
            record(FOCUS_TIMER, spec, Status::Off);
            false
        }
        Ok(Some((mods, key))) => match unsafe { RegisterHotKey(Some(owner), HOTKEY_ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, key) } {
            Ok(()) => {
                crate::log!("focus timer: hotkey {spec}");
                record(FOCUS_TIMER, spec, Status::Ok);
                true
            }
            Err(e) => {
                crate::log!("focus timer: hotkey {spec} not registered ({e})");
                record(FOCUS_TIMER, spec, Status::from_error(&e));
                false
            }
        },
        Err(e) => {
            crate::log!("focus timer: bad hotkey {e}");
            record(FOCUS_TIMER, spec, Status::Invalid);
            false
        }
    };
    *cur = Some((me, spec.to_string(), ok));
}

/// Gives the hotkey back (the dock window is going away).
pub fn release_hotkey() {
    use windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey;
    let Ok(mut cur) = HOTKEY.lock() else { return };
    if let Some((old, _, true)) = cur.take() {
        unsafe {
            let _ = UnregisterHotKey(Some(HWND(old as *mut _)), HOTKEY_ID);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetimes_are_absolute_utc() {
        // 1970-01-01 is 116444736000000000 in FILETIME.
        assert_eq!(filetime_of(0), 116_444_736_000_000_000);
        assert_eq!(filetime_of(1_000), 116_444_736_000_000_000 + 10_000_000);
    }

    #[test]
    fn civil_days_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), 19700101);
        assert_eq!(civil_from_days(days_from_civil(2026, 11, 2)), 20261102);
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), 20240229);
        assert_eq!(civil_from_days(days_from_civil(2026, 12, 31) + 1), 20270101);
    }

    #[test]
    fn presets_follow_the_settings() {
        let mut inner = Inner { timer: Timer::default(), count: Count::default(), last: Preset::Short, cfg: Cfg::DEFAULT, watch: None, loaded: true };
        assert_eq!(plan_of(&inner, Preset::Short), Plan::minutes(25, 5));
        assert_eq!(plan_of(&inner, Preset::Long), Plan::minutes(50, 10));
        inner.cfg.custom = Plan::minutes(42, 7);
        assert_eq!(plan_of(&inner, Preset::Custom), Plan::minutes(42, 7));
        // Do not disturb only for a running focus phase, when asked.
        assert_eq!(dnd_wanted(&inner), None);
        inner.cfg.dnd = false;
        inner.timer.apply(Event::Start(model::SHORT), 0);
        assert_eq!(dnd_wanted(&inner), None, "not asked");
        inner.cfg.dnd = true;
        assert_eq!(dnd_wanted(&inner), Some(25 * 60_000));
        inner.timer.apply(Event::Pause, 1);
        assert_eq!(dnd_wanted(&inner), None, "paused: notifications may come");
        inner.timer.apply(Event::Resume, 1);
        inner.timer.apply(Event::Skip, 2);
        assert_eq!(dnd_wanted(&inner), None, "not during the break");
        // The saved file reads back (and tolerates missing fields).
        let s: Saved = serde_json::from_str(r#"{"count":{"day":20261102,"n":3},"last":"long"}"#).unwrap();
        assert_eq!((s.count.today(20261102), s.last), (3, Preset::Long));
        let s: Saved = serde_json::from_str("{}").unwrap();
        assert_eq!(s.last, Preset::Short);
    }
}
