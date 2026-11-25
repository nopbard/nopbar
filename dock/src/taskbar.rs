//! Hiding the Windows taskbar and *always* getting it back.
//!
//! State needed to restore is written to disk before anything is changed, so
//! a crash, a kill from Task Manager or a power loss can all be undone by the
//! guardian process or the next launch.
//!
//! Hiding is two steps: auto-hide first (frees the reserved strip), then,
//! once the work area shows the strip really is free, the windows. A
//! taskbar that refuses or ignores auto-hide (third-party taskbars, some
//! Windows 10 setups) is given back and kept: NopBar sits next to it
//! instead of leaving an empty band nothing can use (placement.rs).
//! Restoring is checked too (state back, windows visible) and tried twice;
//! the marker stays until it went through, so the guardian or the next
//! start tries again.

use crate::config::Position;
use crate::placement::{self, BarSeen, Edge, HideCheck, Restore};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{
    SHAppBarMessage, ABM_GETSTATE, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABM_SETSTATE,
    ABE_BOTTOM, ABE_LEFT, ABE_RIGHT, ABE_TOP, ABS_AUTOHIDE, APPBARDATA,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::app_dir;

fn state_file() -> PathBuf {
    app_dir().join("taskbar.state")
}

/// What this run did to the taskbar, as one value so every transition is
/// in one place (and unit-tested: hide -> keep -> hide, explorer restarts
/// in either mode). Only `may_hide_windows` / `may_rehide` ever let a
/// taskbar window be hidden; a restore clears both before it is queued.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HideFlags {
    /// The dock wants the taskbar hidden (set by hide(), cleared by restore()).
    want: bool,
    /// Auto-hide was seen freeing the strip and the windows were hidden: the
    /// re-hide hooks may hide them again. Never set before that check.
    windows_hidden: bool,
    /// Auto-hide did not free the taskbar's strip: the taskbar is kept for
    /// the rest of this run (until the setting is switched off and on again).
    refused: bool,
    /// The explorer (pid) whose taskbar `windows_hidden` was verified on:
    /// an exit notice for any other explorer is stale and changes nothing.
    explorer: u32,
}

impl HideFlags {
    /// hide(): auto-hide asked for; the windows wait for the freed check
    /// (after an explorer restart, or a guardian restore during a hang, the
    /// state may not be auto-hide any more).
    pub(crate) fn hide(&mut self) {
        self.windows_hidden = false;
        self.want = true;
    }
    /// The freed check passed on explorer `pid`'s taskbar and the windows
    /// go: true the first time.
    pub(crate) fn freed(&mut self, pid: u32) -> bool {
        self.explorer = pid;
        !std::mem::replace(&mut self.windows_hidden, true)
    }
    /// restore() (keep mode, exit, refusal): nothing hides a window again.
    pub(crate) fn restore(&mut self) {
        self.want = false;
        self.windows_hidden = false;
    }
    /// give_up_hiding(): true the first time in this run.
    pub(crate) fn refuse(&mut self) -> bool {
        !std::mem::replace(&mut self.refused, true)
    }
    pub(crate) fn forget_refusal(&mut self) {
        self.refused = false;
    }
    /// explorer `pid` exited: its successor's taskbar is a new window whose
    /// auto-hide state is not known yet, so nothing re-hides it until the
    /// freed check passes again. Hidden earlier with the strip still
    /// reserved, it would leave an empty band. A late notice about an older
    /// explorer (the new one already checked) is ignored: true if it counted.
    pub(crate) fn explorer_gone(&mut self, pid: u32) -> bool {
        if pid != self.explorer {
            return false;
        }
        self.windows_hidden = false;
        true
    }
    /// Auto-hide is seen `on` or off while the windows are hidden: off
    /// (a display change or explorer put the taskbar back to always shown,
    /// so its strip is reserved again under hidden windows, an empty band
    /// under maximized windows) means asking for auto-hide again, the
    /// windows waiting for the freed check like after hide(). True then.
    pub(crate) fn autohide_lost(&mut self, on: bool) -> bool {
        if on || !self.may_rehide() {
            return false;
        }
        self.windows_hidden = false;
        true
    }
    /// Hiding asked for and not refused (the check and the top bar's place).
    pub(crate) fn hiding_now(&self) -> bool {
        self.want && !self.refused
    }
    /// check_now may hide the windows once the strip is seen free.
    pub(crate) fn may_hide_windows(&self) -> bool {
        self.hiding_now()
    }
    /// The re-hide paths (SHOW hook, explorer-back timer, re-hide timer,
    /// session resync) may hide windows explorer showed on its own.
    pub(crate) fn may_rehide(&self) -> bool {
        self.hiding_now() && self.windows_hidden
    }
}

static FLAGS: Mutex<HideFlags> = Mutex::new(HideFlags { want: false, windows_hidden: false, refused: false, explorer: 0 });

/// The flags now (a poisoned lock still holds the last value).
fn flags() -> HideFlags {
    *FLAGS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Explorer has freed its strip and this instance owns the hidden taskbar.
pub fn hidden_by_us() -> bool {
    HIDDEN_BY_US.load(std::sync::atomic::Ordering::Acquire)
}
static HIDDEN_BY_US: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Changes the flags, returning what `f` returns.
fn with_flags<R>(f: impl FnOnce(&mut HideFlags) -> R) -> R {
    let mut flags = FLAGS.lock().unwrap_or_else(|e| e.into_inner());
    let before = flags.may_rehide();
    let value = f(&mut flags);
    let changed = before != flags.may_rehide();
    HIDDEN_BY_US.store(flags.may_rehide(), std::sync::atomic::Ordering::Release);
    drop(flags);
    if changed {
        crate::corners::taskbar_changed();
        crate::launchpad::wintap::taskbar_changed();
    }
    value
}

/// SHAppBarMessage straight to explorer: our tray spy (tray.rs) steps back
/// in the Z-order for the call so FindWindow lands on explorer's taskbar.
/// (Relayed through the spy, explorer acknowledges AppBar positions but
/// never reserves the space.)
unsafe fn appbar_msg(msg: u32, data: *mut APPBARDATA) -> usize {
    let _aside = crate::tray::Aside::new();
    SHAppBarMessage(msg, data)
}

/// Title of our own `Shell_TrayWnd` (the tray spy, see tray.rs), so every
/// lookup of explorer's taskbar can skip it, in any process.
pub const SPY_TITLE: &str = "NopBarTray";

/// explorer's taskbar window (not our tray spy of the same class).
pub fn explorer_tray() -> Option<HWND> {
    unsafe {
        let mut after: Option<HWND> = None;
        while let Ok(h) = FindWindowExW(None, after, w!("Shell_TrayWnd"), None) {
            after = Some(h);
            // Our own spy: skip by process first. (GetWindowText on a window
            // of our own process sends WM_GETTEXT to its thread, which could
            // be waiting on us.)
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == std::process::id() {
                continue;
            }
            let mut buf = [0u16; 16];
            let n = GetWindowTextW(h, &mut buf).max(0) as usize;
            if String::from_utf16_lossy(&buf[..n]) != SPY_TITLE {
                return Some(h);
            }
        }
        None
    }
}

unsafe fn tray_windows() -> Vec<HWND> {
    let mut out = Vec::new();
    if let Some(h) = explorer_tray() {
        out.push(h);
    }
    let mut after: Option<HWND> = None;
    while let Ok(h) = FindWindowExW(None, after, w!("Shell_SecondaryTrayWnd"), None) {
        out.push(h);
        after = Some(h);
    }
    out
}

unsafe fn get_state() -> u32 {
    let mut abd = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
    appbar_msg(ABM_GETSTATE, &mut abd) as u32
}

unsafe fn set_state(state: u32) {
    let mut abd = APPBARDATA { cbSize: std::mem::size_of::<APPBARDATA>() as u32, ..Default::default() };
    if let Some(h) = explorer_tray() {
        abd.hWnd = h;
    }
    abd.lParam = LPARAM(state as isize);
    appbar_msg(ABM_SETSTATE, &mut abd);
}

/// Present while the taskbar is hidden by us and not yet restored.
fn marker_file() -> PathBuf {
    app_dir().join("taskbar.hidden")
}

/// Step one of hiding: auto-hide (state and marker recorded first). The
/// windows stay until check_hide() sees the strip freed.
pub fn hide() {
    // Not re-hidden by the hooks until this request is seen to free the
    // strip too.
    if !with_flags(|f| {
        f.hide();
        f.hiding_now()
    }) {
        return; // refused in this run
    }
    shell::call("hide taskbar", shell::UI_DEADLINE, hide_now);
}

fn hide_now() {
    unsafe {
        if !flags().hiding_now() {
            return; // restored meanwhile (queued behind a slow call)
        }
        let path = state_file();
        let marker = marker_file();
        // A marker left behind means the last hide was never undone (crash,
        // forced reboot, a restore that did not go through): the taskbar
        // may still be in *our* auto-hide state, so the original recorded
        // back then stays. Otherwise the current state is the user's own
        // (they may have changed it since): record it.
        if !(marker.exists() && path.exists()) {
            let _ = std::fs::create_dir_all(app_dir());
            let _ = std::fs::write(&path, get_state().to_string());
        }
        let _ = std::fs::write(&marker, b"1");
        // Auto-hide frees the reserved screen space. The windows are hidden
        // only once that is seen (check_hide), never before: hidden with
        // the strip still reserved, they leave an empty band behind.
        set_state(ABS_AUTOHIDE);
        log_environment("hide");
    }
}

/// Step two: once auto-hide freed every taskbar strip, hide the windows
/// (they would slide in when the cursor touches the edge). The dock calls
/// this a few times after hide(); `Waiting` every time means auto-hide did
/// not take (then retry_hide, then give_up_hiding).
pub fn check_hide() -> HideCheck {
    if !flags().may_hide_windows() {
        return HideCheck::NotWanted;
    }
    shell::call("check taskbar hide", shell::UI_DEADLINE, || unsafe { check_now() }).unwrap_or(HideCheck::Waiting)
}

unsafe fn check_now() -> HideCheck {
    if !flags().may_hide_windows() {
        return HideCheck::NotWanted;
    }
    let verdict = placement::hide_verdict(&bars_seen());
    if verdict == HideCheck::Freed {
        // Under the lock: a restore that came in meanwhile wins.
        let pid = explorer_pid();
        let Some(first) = with_flags(|f| f.may_hide_windows().then(|| f.freed(pid))) else {
            return HideCheck::NotWanted;
        };
        if first {
            crate::log!("taskbar: auto-hide freed the strip; hiding the taskbar windows");
        }
        for h in tray_windows() {
            // Posted to explorer: never waits on it (audit A03).
            let _ = ShowWindowAsync(h, SW_HIDE);
        }
    }
    verdict
}

/// Auto-hide once more (the first request did not free the strip in time).
pub fn retry_hide() {
    if !flags().hiding_now() {
        return;
    }
    shell::call("hide taskbar again", shell::UI_DEADLINE, || unsafe {
        crate::log!("taskbar: auto-hide did not free the taskbar's strip; asking again");
        set_state(ABS_AUTOHIDE);
    });
}

/// Auto-hide was refused or ignored: give the taskbar back as it was and
/// keep it for this run (NopBar sits next to it). True the first time in
/// this run.
pub fn give_up_hiding() -> bool {
    let first = with_flags(|f| f.refuse());
    shell::call("log taskbar refusal", shell::UI_DEADLINE, || unsafe {
        let left: Vec<String> = bars_seen()
            .iter()
            .filter(|s| placement::strip_reserved(s))
            .filter_map(|s| placement::bar_edge(s.mon, s.bar).map(|(e, t)| format!("{} {t}px, work inset {}", e.name(), placement::work_inset(s.mon, s.work, e))))
            .collect();
        crate::log!("taskbar: auto-hide was refused or ignored (still reserved: {}); keeping the taskbar, NopBar sits next to it", left.join("; "));
        log_environment("refused");
    });
    restore();
    first
}

/// Tells the user, once (ever: a file remembers it), that the taskbar
/// could not be hidden and NopBar sits next to it.
pub fn notice_refused() {
    let seen = app_dir().join("taskbar.notice");
    if seen.exists() {
        return;
    }
    let _ = std::fs::write(&seen, b"1");
    crate::updates::notice(crate::i18n::t("taskbar.refused.title").into(), crate::i18n::t("taskbar.refused.body").into());
}

/// NopBar hides the taskbar right now (asked for and not refused): the
/// top bar's place does not depend on it then.
pub fn hiding_now() -> bool {
    flags().hiding_now()
}

/// Hiding was refused in this run (see give_up_hiding).
pub fn refused() -> bool {
    flags().refused
}

/// explorer `pid` exited (see HideFlags::explorer_gone): true if that was
/// the explorer whose taskbar we hid (a stale notice is ignored).
pub fn explorer_gone(pid: u32) -> bool {
    with_flags(|f| f.explorer_gone(pid))
}

/// The user switched hiding on again: try once more.
pub fn forget_refusal() {
    with_flags(|f| f.forget_refusal());
}

/// A hide not yet undone left its marker on disk.
pub fn marker_present() -> bool {
    marker_file().exists()
}

/// Restores in a row that left a taskbar window off its place, after which
/// the marker is let go (one exit + the guardian's tries + one next start).
pub const RESEAT_LIMIT: u32 = 4;
/// The guardian stops retrying a restore that only failed to re-seat a
/// window once this many were counted; the rest is the next start's.
pub const GUARDIAN_RESEATS: u32 = 3;

/// The off-place restore count a marker records ("reseat N"; a plain
/// hide marker, "1", counts none).
pub(crate) fn reseats_in(marker: &str) -> u32 {
    marker.trim().strip_prefix("reseat ").and_then(|n| n.trim().parse().ok()).unwrap_or(0)
}

pub(crate) fn reseat_marker(n: u32) -> String {
    format!("reseat {n}")
}

/// After a restore that left a window off its place, `before` such restores
/// already counted: Some(count to record, marker kept, a failure) or None
/// (limit reached: let go).
pub(crate) fn after_not_in_place(before: u32) -> Option<u32> {
    let n = before.saturating_add(1);
    (n < RESEAT_LIMIT).then_some(n)
}

/// Off-place restores counted in the marker on disk (0: none / no marker).
pub fn reseats_on_disk() -> u32 {
    std::fs::read_to_string(marker_file()).map(|t| reseats_in(&t)).unwrap_or(0)
}

/// PID of explorer's taskbar (0 if there is none right now).
pub fn explorer_pid() -> u32 {
    unsafe {
        let Some(h) = explorer_tray() else { return 0 };
        let mut pid = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        pid
    }
}

/// Hide the taskbar windows again without touching the saved state. Used
/// when explorer shows them on its own (it does so right after a restart).
pub fn rehide() {
    // Only once auto-hide was seen to free the strip (check_hide), and
    // never in keep mode (restore cleared it).
    if !flags().may_rehide() {
        return;
    }
    unsafe {
        for h in tray_windows() {
            if IsWindowVisible(h).as_bool() {
                let _ = ShowWindowAsync(h, SW_HIDE);
            }
        }
    }
}

/// While NopBar hides the taskbar: if auto-hide was switched off behind our
/// back (monitors came or went, explorer re-read its settings), asks for it
/// again. True then: the caller runs the freed check again (HIDE_CHECK_TIMER),
/// which hides the windows once the strip is free.
pub fn reassert() -> bool {
    if !flags().may_rehide() {
        return false;
    }
    shell::call("check taskbar auto-hide", shell::UI_DEADLINE, || unsafe {
        let on = get_state() & ABS_AUTOHIDE != 0;
        if !with_flags(|f| f.autohide_lost(on)) {
            return false;
        }
        crate::log!("taskbar: auto-hide was switched off (display change or explorer); asking again");
        set_state(ABS_AUTOHIDE);
        true
    })
    .unwrap_or(false)
}

/// Undoes hide(): puts back the state recorded when we hid the taskbar and
/// shows its windows. Without the marker the taskbar state is the user's
/// own and is left alone (only the windows are shown; nothing is ever
/// hidden here, so this is safe with hiding switched off).
pub fn restore() {
    restore_within(shell::UI_DEADLINE);
}

/// restore() waiting at most `deadline` for explorer (the guardian uses its
/// own limit, so a stuck explorer cannot stop its crash / hang handling).
/// True if it finished in time.
pub fn restore_within(deadline: Duration) -> bool {
    restore_checked(deadline).is_some()
}

/// restore_within, with the outcome: None = not finished in time (it
/// still runs, in order), Some(confirmed).
fn restore_checked(deadline: Duration) -> Option<bool> {
    // From now on nothing hides the windows again (hooks, timers, a check
    // queued behind a slow call): before the restore is even queued.
    with_flags(|f| f.restore());
    shell::call("restore taskbar", deadline, restore_now)
}

/// restore() for a process about to exit (every exit path ends here): the
/// restore is confirmed (state back, windows visible, tried twice) and a
/// restore explorer was slow to take is waited for (up to `max`), so the
/// exit does not cut it off. True if the taskbar is confirmed back; if not,
/// the marker stays and the guardian (or the next start) restores it.
pub fn restore_before_exit(max: Duration) -> bool {
    let r = restore_checked(max.min(shell::UI_DEADLINE * 2));
    if !shell::wait_idle(max) {
        crate::log!("taskbar: explorer did not take the restore before exit; the guardian / next start restores it");
        return false;
    }
    match r {
        Some(ok) => ok,
        // Ran late but ran: did it go through?
        None => !marker_present() && unsafe { all_visible() },
    }
}

/// How long one restore try waits to see the taskbar back.
const RESTORE_CONFIRM: Duration = Duration::from_millis(1200);

/// Puts the saved state back and shows the windows, then checks both
/// (placement::restore_verdict), trying once more if needed. The marker is
/// removed only when it went through, so a failed restore is retried by
/// the guardian or the next start, and the next hide keeps the user's
/// original state instead of recording our auto-hide as theirs.
fn restore_now() -> bool {
    unsafe {
        let marker = marker_file();
        let saved = if marker.exists() {
            Some(std::fs::read_to_string(state_file()).ok().and_then(|t| t.trim().parse::<u32>().ok()).unwrap_or(0))
        } else {
            None
        };
        let mut last = Restore::NotBack;
        for attempt in 1..=2 {
            // Windows first, state second: the state change then reaches a
            // visible taskbar (as when the user flips auto-hide in
            // Settings) and explorer slides it into place and reserves its
            // strip. Changed while hidden, the window can stay where
            // auto-hide slid it, off the screen: visible, strip reserved,
            // nothing in it (the "empty band" report).
            for h in tray_windows() {
                let _ = ShowWindowAsync(h, SW_SHOWNA);
            }
            // The show is posted; the state call is sent and would overtake it.
            wait_shown(SHOW_WAIT);
            if let Some(state) = saved {
                if attempt > 1 && last == Restore::NotInPlace {
                    // Explorer thinks it is shown already and skips the
                    // re-seat: go through auto-hide once to make it move.
                    set_state(state | ABS_AUTOHIDE);
                }
                set_state(state);
            }
            let (verdict, r) = wait_restored(saved);
            last = verdict;
            if verdict == Restore::Done {
                if saved.is_some() {
                    let _ = std::fs::remove_file(&marker);
                    crate::log!("taskbar: restored ({r}){}", if attempt > 1 { " on the second try" } else { "" });
                }
                return true;
            }
            crate::log!("taskbar: restore try {attempt} not confirmed (saved state {saved:?}, {r}: {verdict:?})");
        }
        match last {
            // State back and every window shown, but a window left off its
            // place (an empty band): not given back. The marker stays,
            // counting such restores, so the guardian / next start try
            // again; after RESEAT_LIMIT in a row it is let go (logged), so
            // a taskbar explorer never re-seats cannot make every start
            // restore again forever.
            Restore::NotInPlace if saved.is_some() => {
                let before = std::fs::read_to_string(&marker).map(|t| reseats_in(&t)).unwrap_or(0);
                match after_not_in_place(before) {
                    Some(n) => {
                        let _ = std::fs::write(&marker, reseat_marker(n));
                        crate::log!("taskbar: restore left a taskbar window off its place ({n} time(s) in a row); the marker stays for the guardian / next start");
                        false
                    }
                    None => {
                        let _ = std::fs::remove_file(&marker);
                        crate::log!("taskbar: a taskbar window is still off its place after {RESEAT_LIMIT} restores in a row; giving up (state and windows are back)");
                        true
                    }
                }
            }
            Restore::NotInPlace => true,
            _ => {
                if saved.is_some() {
                    crate::log!("taskbar: restore not confirmed; the marker stays for the guardian / next start");
                }
                false
            }
        }
    }
}

/// How long a restore waits for the posted show before changing the state.
const SHOW_WAIT: Duration = Duration::from_millis(250);

/// Waits (at most `max`) until every taskbar window is visible.
unsafe fn wait_shown(max: Duration) {
    let start = Instant::now();
    while !all_visible() && start.elapsed() < max {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What a restore try saw last (for the log).
struct Seen {
    now: Option<u32>,
    windows: usize,
    visible: usize,
    seated: usize,
}

impl std::fmt::Display for Seen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "state {:?}, {}/{} window(s) visible, {} in place", self.now, self.visible, self.windows, self.seated)
    }
}

/// The taskbar window sits wholly on its monitor (not slid away).
unsafe fn seated(h: HWND) -> bool {
    let mut r = RECT::default();
    if GetWindowRect(h, &mut r).is_err() {
        return false;
    }
    monitor_of_bar(r).is_some_and(|m| placement::bar_seated(m.rect, r))
}

/// Polls until the restore shows, at most RESTORE_CONFIRM. Visibility
/// first (no explorer call); the state and the windows' places are looked
/// at once the windows are back.
unsafe fn wait_restored(saved: Option<u32>) -> (Restore, Seen) {
    let start = Instant::now();
    loop {
        let wins = tray_windows();
        let vis = wins.iter().filter(|&&h| IsWindowVisible(h).as_bool()).count();
        let back = !wins.is_empty() && vis == wins.len();
        let now = if back { Some(get_state()) } else { None };
        let in_place = if back { wins.iter().filter(|&&h| seated(h)).count() } else { 0 };
        let verdict = now.map_or(Restore::NotBack, |n| placement::restore_verdict(saved, n, wins.len(), vis, in_place));
        if verdict == Restore::Done || start.elapsed() >= RESTORE_CONFIRM {
            return (verdict, Seen { now, windows: wins.len(), visible: vis, seated: in_place });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every taskbar window is visible (and there is one).
unsafe fn all_visible() -> bool {
    let wins = tray_windows();
    !wins.is_empty() && wins.iter().all(|&h| IsWindowVisible(h).as_bool())
}

#[cfg(test)]
mod mode_tests {
    //! The hide / keep state machine as the dock drives it (dock/mod.rs:
    //! apply_taskbar_mode, check_taskbar_hide, the TaskbarCreated handler,
    //! the SHOW hook, EXPLORER_WAIT / REHIDE timers), against a model
    //! explorer. The flags are the real HideFlags; only explorer is faked.
    use super::HideFlags;

    #[derive(Debug, Default)]
    struct Explorer {
        visible: bool,
        autohide: bool,
        pid: u32,
    }

    impl Explorer {
        /// The strip is reserved (the work area leaves it out).
        fn reserved(&self) -> bool {
            !self.autohide
        }
        /// "Empty band": strip reserved, nothing shown in it.
        fn empty_band(&self) -> bool {
            self.reserved() && !self.visible
        }
    }

    struct Dock {
        f: HideFlags,
        /// HIDE_TASKBAR (dock/mod.rs): the hook callbacks' mirror of the mode.
        hide_flag: bool,
        cfg_hide: bool,
        ex: Explorer,
    }

    impl Dock {
        fn new(cfg_hide: bool) -> Self {
            let mut d = Dock { f: HideFlags::default(), hide_flag: false, cfg_hide, ex: Explorer { visible: true, autohide: false, pid: 100 } };
            d.apply_mode();
            d
        }
        /// apply_taskbar_mode for the primary dock.
        fn apply_mode(&mut self) {
            let hide = self.cfg_hide && !self.f.refused; // hides_taskbar()
            self.hide_flag = hide;
            if hide {
                self.f.hide();
                if self.f.hiding_now() {
                    self.ex.autohide = true; // hide_now
                }
            } else {
                // restore(): flags first, then restore_now (show, then state).
                self.f.restore();
                self.ex.visible = true;
                self.ex.autohide = false;
            }
        }
        /// HIDE_CHECK_TIMER -> check_hide / check_now.
        fn hide_check(&mut self) {
            if self.f.may_hide_windows() && !self.ex.reserved() {
                self.f.freed(self.ex.pid);
                self.ex.visible = false;
            }
        }
        /// taskbar::rehide() as every re-hide path calls it (each path also
        /// checks HIDE_TASKBAR first).
        fn rehide(&mut self) {
            if self.hide_flag && self.f.may_rehide() {
                self.ex.visible = false;
            }
        }
        /// Config reload with a new hide_taskbar.
        fn set_cfg(&mut self, hide: bool) {
            if hide != self.cfg_hide {
                self.cfg_hide = hide;
                if hide {
                    self.f.forget_refusal();
                }
                self.apply_mode();
            }
        }
        /// explorer.exe killed and started again: a new, shown taskbar with
        /// the auto-hide state explorer keeps in the registry. Then, in the
        /// order they arrive: the SHOW hook, EXPLORER_WAIT_TIMER, the
        /// TaskbarCreated broadcast (apply_mode) and the REHIDE timer steps.
        fn restart_explorer(&mut self) {
            let old = self.ex.pid;
            self.f.explorer_gone(old); // on_explorer_gone
            self.new_explorer(self.ex.autohide);
            self.apply_mode(); // TaskbarCreated
            for _ in 0..5 {
                self.rehide(); // REHIDE_TIMER steps
                self.hide_check();
            }
        }
    }

    impl Dock {
        /// A new explorer is up (shown taskbar, `autohide` as it kept it):
        /// the SHOW hook, then EXPLORER_WAIT_TIMER (re-hide, and a hide
        /// check scheduled while hiding).
        fn new_explorer(&mut self, autohide: bool) {
            self.ex = Explorer { visible: true, autohide, pid: self.ex.pid + 1 };
            self.rehide(); // SHOW hook
            self.rehide(); // EXPLORER_WAIT_TIMER ...
            if self.hide_flag {
                self.hide_check(); // ... and its HIDE_CHECK_TIMER
            }
        }
    }

    impl Dock {
        /// WORKAREA_TIMER / DISPLAY_TIMER -> reassert_taskbar: auto-hide
        /// asked again if it was lost, then the freed check.
        fn reassert(&mut self) {
            if self.hide_flag && self.f.autohide_lost(self.ex.autohide) {
                self.ex.autohide = true; // set_state
                self.hide_check();
            }
        }
    }

    #[test]
    fn autohide_lost_on_a_display_change_is_asked_again() {
        let mut d = Dock::new(true);
        d.hide_check();
        assert!(!d.ex.visible && d.ex.autohide);
        // Monitors change; explorer puts the taskbar back to always shown
        // and keeps its windows hidden: the strip is reserved again.
        d.ex.autohide = false;
        assert!(d.ex.empty_band());
        d.reassert();
        assert!(d.ex.autohide && !d.ex.visible && !d.ex.empty_band());
        // Nothing lost: nothing changes.
        let before = d.f;
        d.reassert();
        assert_eq!(d.f, before);
        // Keep mode: the user's taskbar is never touched.
        d.set_cfg(false);
        d.reassert();
        assert!(d.ex.visible && !d.ex.autohide);
    }

    #[test]
    fn hide_then_keep_then_hide() {
        let mut d = Dock::new(true);
        // Auto-hide asked, windows only once the strip is free.
        assert!(d.ex.visible && d.ex.autohide);
        d.rehide();
        assert!(d.ex.visible, "no re-hide before the freed check");
        d.hide_check();
        assert!(!d.ex.visible && !d.ex.empty_band());
        // Keep: shown at once, and no path hides it again.
        d.set_cfg(false);
        assert!(d.ex.visible && !d.ex.autohide && !d.hide_flag);
        assert!(!d.f.may_rehide() && !d.f.may_hide_windows());
        d.rehide();
        d.hide_check();
        assert!(d.ex.visible && !d.ex.empty_band(), "keep mode stays shown");
        // Hide again works.
        d.set_cfg(true);
        d.hide_check();
        assert!(!d.ex.visible && d.ex.autohide && !d.ex.empty_band());
        d.ex.visible = true; // explorer shows it on its own
        d.rehide();
        assert!(!d.ex.visible, "re-hidden in hide mode");
    }

    #[test]
    fn explorer_restart_in_keep_mode_never_hides() {
        let mut d = Dock::new(true);
        d.hide_check();
        d.set_cfg(false);
        for _ in 0..3 {
            d.restart_explorer();
            assert!(d.ex.visible && !d.ex.empty_band(), "kept taskbar after a restart");
            assert!(!d.f.may_rehide());
        }
        // Started in keep mode right away.
        let mut d = Dock::new(false);
        d.restart_explorer();
        assert!(d.ex.visible && !d.ex.autohide);
    }

    #[test]
    fn explorer_restart_in_hide_mode_hides_again_without_a_band() {
        let mut d = Dock::new(true);
        d.hide_check();
        for _ in 0..3 {
            d.restart_explorer();
            assert!(!d.ex.visible && d.ex.autohide && !d.ex.empty_band());
        }
        // A restarted explorer that came back NOT in auto-hide (state
        // lost): the early re-hide paths wait for the strip to be free
        // again, then TaskbarCreated hides it properly.
        assert!(d.f.explorer_gone(d.ex.pid));
        d.new_explorer(false);
        assert!(!d.ex.empty_band(), "never an empty band");
        assert!(d.ex.visible);
        d.apply_mode(); // TaskbarCreated
        d.hide_check();
        assert!(!d.ex.visible && d.ex.autohide && !d.ex.empty_band());
    }

    /// Review 2026-11-02: the old explorer's exit notice arrives only after
    /// the new explorer's taskbar was checked and hidden. It must not undo
    /// that (re-hiding would stop for the rest of the run).
    #[test]
    fn a_late_exit_notice_of_the_old_explorer_is_ignored() {
        let mut d = Dock::new(true);
        d.hide_check();
        let old = d.ex.pid;
        // The new explorer comes up and is hidden before the notice lands.
        d.new_explorer(true);
        d.apply_mode(); // TaskbarCreated
        d.hide_check();
        assert!(!d.ex.visible && d.f.may_rehide());
        assert!(!d.f.explorer_gone(old), "stale notice ignored");
        assert!(d.f.may_rehide());
        d.ex.visible = true; // the new explorer shows it on its own
        d.rehide();
        assert!(!d.ex.visible, "still re-hidden");
        // The current explorer's own exit does count.
        assert!(d.f.explorer_gone(d.ex.pid));
        assert!(!d.f.may_rehide());
        // Before anything was ever hidden, no notice counts.
        let mut f = HideFlags::default();
        f.hide();
        assert!(!f.explorer_gone(42));
    }

    /// EXPLORER_WAIT_TIMER alone (TaskbarCreated late or lost) still hides
    /// a new taskbar whose auto-hide was kept, and never makes a band.
    #[test]
    fn a_new_explorer_is_hidden_without_taskbar_created() {
        let mut d = Dock::new(true);
        d.hide_check();
        assert!(d.f.explorer_gone(d.ex.pid));
        d.new_explorer(true);
        assert!(!d.ex.visible && !d.ex.empty_band());
        assert!(d.f.explorer_gone(d.ex.pid));
        d.new_explorer(false);
        assert!(d.ex.visible && !d.ex.empty_band(), "strip reserved: shown until auto-hide is back");
    }

    /// Review 2026-11-02: a restore that leaves the taskbar off its place
    /// is a failure (marker kept, the guardian / next start retry), but a
    /// bounded one: exit, the guardian's tries, then one next start.
    #[test]
    fn off_place_restores_are_retried_a_bounded_number_of_times() {
        use super::{after_not_in_place, reseat_marker, reseats_in, GUARDIAN_RESEATS, RESEAT_LIMIT};
        assert_eq!(reseats_in("1"), 0, "a plain hide marker");
        assert_eq!(reseats_in(""), 0);
        assert_eq!(reseats_in("reseat 2
"), 2);
        assert_eq!(reseats_in(&reseat_marker(3)), 3);
        // Walk it: exit, then the guardian while it may retry, then starts.
        let mut count = 0;
        let mut who = Vec::new();
        let mut marker = true;
        // Exit.
        match after_not_in_place(count) {
            Some(n) => count = n,
            None => marker = false,
        }
        who.push("exit");
        assert!(marker, "the exit restore is a failure: the guardian runs");
        // The guardian retries while the marker is there and below its cap.
        while marker && count < GUARDIAN_RESEATS {
            match after_not_in_place(count) {
                Some(n) => count = n,
                None => marker = false,
            }
            who.push("guardian");
        }
        assert!(marker, "the guardian leaves the marker for the next start");
        // Next start(s): until let go.
        let mut starts = 0;
        while marker {
            starts += 1;
            assert!(starts < 10, "never loops forever");
            match after_not_in_place(count) {
                Some(n) => count = n,
                None => marker = false,
            }
        }
        assert_eq!(starts, 1, "the next start tries once, then lets go");
        assert_eq!(who.len() as u32 + starts, RESEAT_LIMIT);
        assert!(who.iter().filter(|w| **w == "guardian").count() >= 2, "the guardian retries more than once");
    }

    #[test]
    fn refusal_keeps_the_taskbar_until_switched_on_again() {
        let mut f = HideFlags::default();
        f.hide();
        assert!(f.refuse(), "first refusal");
        f.restore(); // give_up_hiding restores
        assert!(!f.refuse(), "only once");
        assert!(!f.hiding_now() && !f.may_rehide());
        f.hide(); // explorer restart in a refused run: still kept
        assert!(!f.may_hide_windows() && !f.may_rehide());
        f.forget_refusal(); // switched off and on again
        f.hide();
        assert!(f.may_hide_windows() && !f.may_rehide());
        assert!(f.freed(5) && !f.freed(5), "first freed only once");
        assert!(f.may_rehide());
        f.restore();
        assert!(!f.hiding_now() && !f.may_rehide() && !f.refused);
    }
}

// ---- What the taskbar looks like (placement and logs) ----------------------

/// The visible taskbar windows (explorer's, plus Shell_SecondaryTrayWnd on
/// other monitors) with the monitor each is on. Sends no messages: fine
/// from any thread, also while the dock is borrowed.
pub fn visible_bars() -> Vec<(isize, RECT)> {
    unsafe {
        tray_windows()
            .into_iter()
            .filter(|&h| IsWindowVisible(h).as_bool())
            .filter_map(|h| {
                let mut r = RECT::default();
                GetWindowRect(h, &mut r).ok()?;
                Some((monitor_of_bar(r)?.handle, r))
            })
            .collect()
    }
}

/// The monitor a taskbar window belongs to. Not MonitorFromRect: a
/// slid-away auto-hide taskbar hangs mostly over the next monitor and
/// would be counted there (placement::bar_monitor).
fn monitor_of_bar(r: RECT) -> Option<crate::monitors::Monitor> {
    let all = crate::monitors::all();
    let rects: Vec<RECT> = all.iter().map(|m| m.rect).collect();
    placement::bar_monitor(r, &rects).map(|i| all[i])
}

/// The visible taskbar rects on monitor `mon` (for placement::dock_area).
pub fn bars_on(mon: isize) -> Vec<RECT> {
    visible_bars().into_iter().filter(|(m, _)| *m == mon).map(|(_, r)| r).collect()
}

/// Every taskbar window with its monitor, that monitor's work area, and
/// how much of its edge NopBar's own AppBars reserve (the hide check).
unsafe fn bars_seen() -> Vec<BarSeen> {
    let own = OWN.lock().map(|o| o.clone()).unwrap_or_default();
    tray_windows()
        .into_iter()
        .filter_map(|h| {
            let mut bar = RECT::default();
            GetWindowRect(h, &mut bar).ok()?;
            let m = monitor_of_bar(bar)?;
            let edge = placement::bar_edge(m.rect, bar).map(|(e, _)| e);
            let own_px = edge.map(|e| own_px_on(&own, m.rect, e)).unwrap_or(0);
            Some(BarSeen { mon: m.rect, work: m.work, bar, own_px })
        })
        .collect()
}

/// Taskbar tools that replace or patch explorer's taskbar (they may ignore
/// auto-hide or show the taskbar again on their own), found by their
/// modules in explorer or their settings keys. For the log only.
fn third_party(explorer: u32) -> Vec<&'static str> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE};
    use windows::Win32::System::Registry::{RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ};
    const TOOLS: [(&str, &str, &str); 4] = [
        ("StartAllBack", "startallback", "Software\\StartIsBack"),
        ("ExplorerPatcher", "explorerpatcher", "Software\\ExplorerPatcher"),
        ("Windhawk", "windhawk", "Software\\Windhawk"),
        ("7+ Taskbar Tweaker", "7tt", "Software\\7 Taskbar Tweaker"),
    ];
    let mut modules: Vec<String> = Vec::new();
    unsafe {
        if explorer != 0 {
            if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, explorer) {
                let mut e = MODULEENTRY32W { dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32, ..Default::default() };
                let mut more = Module32FirstW(snap, &mut e).is_ok();
                while more {
                    let n = e.szModule.iter().position(|&c| c == 0).unwrap_or(e.szModule.len());
                    modules.push(String::from_utf16_lossy(&e.szModule[..n]).to_ascii_lowercase());
                    more = Module32NextW(snap, &mut e).is_ok();
                }
                let _ = windows::Win32::Foundation::CloseHandle(snap);
            }
        }
    }
    let key_exists = |path: &str| unsafe {
        let mut k = HKEY::default();
        let ok = RegOpenKeyExW(HKEY_CURRENT_USER, &windows::core::HSTRING::from(path), None, KEY_READ, &mut k).is_ok();
        if ok {
            let _ = RegCloseKey(k);
        }
        ok
    };
    TOOLS
        .iter()
        .filter(|(_, module, key)| modules.iter().any(|m| m.contains(module)) || key_exists(key))
        .map(|(name, ..)| *name)
        .collect()
}

/// The Windows build number ("19045": Windows 10, 22000 and up: 11).
pub fn windows_build() -> String {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let mut buf = [0u16; 64];
    let mut size = std::mem::size_of_val(&buf) as u32;
    let key = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let ok = unsafe { RegGetValueW(HKEY_LOCAL_MACHINE, key, w!("CurrentBuild"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)).is_ok() };
    if !ok {
        return "?".into();
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// One line about the taskbar for bug reports: the Windows build, each
/// taskbar's edge / thickness / visibility / reserved work area, the
/// auto-hide state, and known taskbar tools. Once per `why` per run. Runs
/// on the shell helper (it asks explorer for the state).
unsafe fn log_environment(why: &'static str) {
    static DONE: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    if let Ok(mut d) = DONE.lock() {
        if d.contains(&why) {
            return;
        }
        d.push(why);
    }
    let state = get_state();
    let bars: Vec<String> = tray_windows()
        .into_iter()
        .filter_map(|h| {
            let mut r = RECT::default();
            GetWindowRect(h, &mut r).ok()?;
            let m = monitor_of_bar(r)?;
            let found = placement::bar_edge(m.rect, r);
            let (edge, t) = found.map(|(e, t)| (e.name(), t)).unwrap_or(("?", 0));
            let inset = found.map(|(e, _)| placement::work_inset(m.rect, m.work, e)).unwrap_or(0);
            Some(format!("{edge} {t}px visible={} work-inset={inset}", IsWindowVisible(h).as_bool()))
        })
        .collect();
    let tools = third_party(explorer_pid());
    crate::log!(
        "taskbar ({why}): Windows build {}, autohide={}, bars [{}], tools [{}]",
        windows_build(),
        state & ABS_AUTOHIDE != 0,
        bars.join("; "),
        if tools.is_empty() { "none".to_string() } else { tools.join(", ") }
    );
}

/// Logs what the kept taskbar looks like (coexistence), once per run.
pub fn log_kept() {
    shell::call("log taskbar", shell::UI_DEADLINE, || unsafe { log_environment("kept") });
}

/// The guardian this dock started (0 = none): the updater waits for it.
static GUARDIAN_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn guardian_pid() -> u32 {
    GUARDIAN_PID.load(std::sync::atomic::Ordering::SeqCst)
}

/// Starts the guardian. It never shows a window, so it is started with
/// the "app starting" cursor turned off (otherwise Windows shows the busy
/// cursor for seconds, waiting for a window that never comes).

pub fn spawn_guardian() {
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTF_FORCEOFFFEEDBACK, STARTUPINFOW};
    let Ok(exe) = std::env::current_exe() else { return };
    let cmd = format!("\"{}\" --guardian {}", exe.display(), std::process::id());
    let mut wide: Vec<u16> = cmd.encode_utf16().chain(Some(0)).collect();
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, dwFlags: STARTF_FORCEOFFFEEDBACK, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        if CreateProcessW(None, Some(PWSTR(wide.as_mut_ptr())), None, None, false, PROCESS_CREATION_FLAGS(0), None, None, &si, &mut pi).is_ok() {
            GUARDIAN_PID.store(pi.dwProcessId, std::sync::atomic::Ordering::SeqCst);
            let _ = windows::Win32::Foundation::CloseHandle(pi.hProcess);
            let _ = windows::Win32::Foundation::CloseHandle(pi.hThread);
        }
    }
}

// ---- Reserving screen space for the dock (like the taskbar does) ----------

pub const WM_APPBAR: u32 = WM_APP + 2;

fn edge_of(pos: Position) -> u32 {
    match pos {
        Position::Bottom => ABE_BOTTOM,
        Position::Left => ABE_LEFT,
        Position::Right => ABE_RIGHT,
    }
}

fn abd(hwnd: HWND, edge: u32) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: WM_APPBAR,
        uEdge: edge,
        ..Default::default()
    }
}

/// The reserved strip of `thickness` px along `edge` on `monitor`.
fn strip(monitor: RECT, thickness: i32, edge: u32) -> RECT {
    match edge {
        ABE_TOP => RECT { bottom: monitor.top + thickness, ..monitor },
        ABE_LEFT => RECT { right: monitor.left + thickness, ..monitor },
        ABE_RIGHT => RECT { left: monitor.right - thickness, ..monitor },
        _ => RECT { top: monitor.bottom - thickness, ..monitor },
    }
}

pub fn appbar_register(hwnd: HWND) {
    let h = hwnd.0 as isize;
    shell::call("register AppBar", shell::UI_DEADLINE, move || unsafe {
        // A new registration: the shell has no strip from it yet (on the
        // helper thread, in order with the reservations queued around it).
        forget_seat(h);
        let mut a = abd(HWND(h as *mut _), ABE_BOTTOM);
        appbar_msg(ABM_NEW, &mut a);
    });
}

/// Reserve `thickness` px on the `pos` edge (0 = registered, nothing
/// reserved: windows may cover the dock, fullscreen notices still arrive).
pub fn appbar_reserve(hwnd: HWND, monitor: RECT, thickness: i32, pos: Position) {
    reserve("dock", hwnd, monitor, thickness, edge_of(pos), 0, thickness);
}

/// The dock's strip as placement::dock_reserve has it: reaching past a
/// top bar at the bottom that reserves nothing, that strip counted as ours.
pub fn appbar_reserve_dock(hwnd: HWND, monitor: RECT, thickness: i32, pos: Position, r: placement::DockReserve) {
    reserve("dock", hwnd, monitor, thickness, edge_of(pos), r.reach, r.own);
}

/// The top bar's strip on any ABE_* edge (ABE_TOP, or ABE_BOTTOM at the
/// bottom), reaching at least `reach` px in from the monitor's edge (past
/// an auto-hide taskbar on that edge, which the shell does not step
/// around: see placement::reserve_rect). Only its thickness is its own.
pub fn appbar_reserve_reach(hwnd: HWND, monitor: RECT, thickness: i32, edge: u32, reach: i32) {
    reserve("top bar", hwnd, monitor, thickness, edge, reach, thickness);
}

fn reserve(who: &'static str, hwnd: HWND, monitor: RECT, thickness: i32, edge: u32, reach: i32, own: i32) {
    let h = hwnd.0 as isize;
    // The hide check subtracts only what is ours (`own`): what the shell
    // stacks in front of us (a taskbar still reserving) must still show.
    note_own(h, monitor, edge, thickness, reach, own);
    shell::call("reserve AppBar edge", shell::UI_DEADLINE, move || unsafe { reserve_now(who, HWND(h as *mut _), monitor, thickness, edge, reach) });
}

fn abe_edge(edge: u32) -> Edge {
    match edge {
        ABE_TOP => Edge::Top,
        ABE_LEFT => Edge::Left,
        ABE_RIGHT => Edge::Right,
        _ => Edge::Bottom,
    }
}

/// What one of our own AppBars reserves (the hide check must not take it
/// for the taskbar's strip).
#[derive(Clone, Copy, Debug)]
struct Own {
    hwnd: isize,
    mon: RECT,
    edge: Edge,
    /// How far in from the monitor's edge windows must stay out
    /// (thickness, or more for a strip below a taskbar on that edge).
    reach: i32,
    /// How much of that edge is ours: the thickness, plus a top bar's
    /// strip the dock reserves on its behalf (never a taskbar's).
    own: i32,
}

impl Own {
    /// What one reservation counts as (None: nothing reserved).
    fn new(hwnd: isize, mon: RECT, edge: u32, thickness: i32, reach: i32, own: i32) -> Option<Self> {
        (thickness > 0).then(|| Own { hwnd, mon, edge: abe_edge(edge), reach: reach.max(thickness), own: own.max(thickness) })
    }
}

static OWN: Mutex<Vec<Own>> = Mutex::new(Vec::new());

fn note_own(hwnd: isize, mon: RECT, edge: u32, thickness: i32, reach: i32, own: i32) {
    if let Ok(mut o) = OWN.lock() {
        o.retain(|x| x.hwnd != hwnd);
        o.extend(Own::new(hwnd, mon, edge, thickness, reach, own));
    }
}

/// How much of `mon`'s `edge` our AppBars reserve (the hide check's own_px).
fn own_px_on(own: &[Own], mon: RECT, edge: Edge) -> i32 {
    let list: Vec<(RECT, Edge, i32)> = own.iter().map(|o| (o.mon, o.edge, o.own)).collect();
    placement::own_inset(&list, mon, edge)
}

/// Where the shell put one of our AppBars (its last ABM_SETPOS): for the
/// unchanged-strip check and the log. Touched on the shell helper thread
/// only, in order with the calls.
#[derive(Clone, Copy, Debug)]
struct Seat {
    hwnd: isize,
    who: &'static str,
    mon: RECT,
    edge: Edge,
    asked: RECT,
    got: RECT,
}

static SEATS: Mutex<Vec<Seat>> = Mutex::new(Vec::new());
/// The last edge line logged per monitor (one line per change).
static LOGGED: Mutex<Vec<(RECT, String)>> = Mutex::new(Vec::new());

fn forget_seat(hwnd: isize) {
    if let Ok(mut s) = SEATS.lock() {
        s.retain(|x| x.hwnd != hwnd);
    }
}

/// Logs monitor `mon`'s work area and the strips our AppBars got there,
/// when that differs from the last line for it (a change, not every
/// notification).
pub fn log_edges(mon: RECT) {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONULL};
    let seats: Vec<Seat> = SEATS.lock().map(|s| s.iter().filter(|x| x.mon == mon).copied().collect()).unwrap_or_default();
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let h = unsafe { MonitorFromRect(&mon, MONITOR_DEFAULTTONULL) };
    if h.is_invalid() || !unsafe { GetMonitorInfoW(h, &mut mi) }.as_bool() {
        return;
    }
    let r = |r: RECT| format!("({},{})-({},{})", r.left, r.top, r.right, r.bottom);
    let mut line = format!("display {} work {}", r(mon), r(mi.rcWork));
    for s in seats.iter().filter(|s| s.got.right > s.got.left && s.got.bottom > s.got.top) {
        line.push_str("; ");
        line.push_str(&placement::seat_text(s.who, s.edge, s.asked, s.got));
    }
    let Ok(mut logged) = LOGGED.lock() else { return };
    if logged.iter().any(|(m, l)| *m == mon && *l == line) {
        return;
    }
    logged.retain(|(m, _)| *m != mon);
    logged.push((mon, line.clone()));
    drop(logged);
    crate::log!("appbar: {line}");
}

/// The work area `work` of monitor `mon` keeps windows out of the `px`
/// px along `edge` (what an AppBar reserving that strip makes the shell do).
pub fn strip_excluded(mon: RECT, work: RECT, edge: Edge, px: i32) -> bool {
    match edge {
        Edge::Top => work.top >= mon.top + px,
        Edge::Bottom => work.bottom <= mon.bottom - px,
        Edge::Left => work.left >= mon.left + px,
        Edge::Right => work.right <= mon.right - px,
    }
}

/// Our AppBars (window handles) whose strip the shell does not keep out
/// of their monitor's work area: the shell forgot them (explorer restarted
/// and a re-registration was missed or came too early). A monitor that is
/// gone is not reported.
pub fn lost_reservations() -> Vec<isize> {
    let own = OWN.lock().map(|o| o.clone()).unwrap_or_default();
    if own.is_empty() {
        return Vec::new();
    }
    let monitors = crate::monitors::all();
    own.iter()
        .filter(|o| monitors.iter().find(|m| m.rect == o.mon).is_some_and(|m| !strip_excluded(m.rect, m.work, o.edge, o.reach)))
        .map(|o| o.hwnd)
        .collect()
}

unsafe fn reserve_now(who: &'static str, hwnd: HWND, monitor: RECT, thickness: i32, edge: u32, reach: i32) {
    let mut a = abd(hwnd, edge);
    a.rc = strip(monitor, thickness, edge);
    appbar_msg(ABM_QUERYPOS, &mut a);
    // The shell may move the far edge; keep our thickness from where it
    // starts us (past the taskbar and other bars), reaching `reach` at least.
    let reach = if thickness > 0 { reach } else { 0 };
    let e = abe_edge(edge);
    let asked = placement::reserve_rect(monitor, a.rc, e, thickness, reach);
    let h = hwnd.0 as isize;
    let last = SEATS.lock().ok().and_then(|s| s.iter().find(|x| x.hwnd == h && x.mon == monitor).map(|x| (x.edge, x.asked)));
    if !placement::setpos_needed(last, e, asked) {
        // The shell already has this strip from us (an ABN_POSCHANGED that
        // changed nothing for us): a SETPOS would only notify the others
        // again.
        return;
    }
    a.rc = asked;
    let reserved = appbar_msg(ABM_SETPOS, &mut a) != 0;
    // What the shell finally gave us (it may adjust it again): kept, and
    // logged with the work area when it changes.
    let got = a.rc;
    if let Ok(mut s) = SEATS.lock() {
        s.retain(|x| x.hwnd != h);
        s.push(Seat { hwnd: h, who, mon: monitor, edge: e, asked, got });
    }
    log_edges(monitor);
    if reserved && who == "top bar" && thickness > 0 && edge == ABE_TOP {
        crate::topbar::caption::refit_after_reserve(monitor, got.bottom);
    }
}

pub fn appbar_remove(hwnd: HWND) {
    let h = hwnd.0 as isize;
    note_own(h, RECT::default(), ABE_BOTTOM, 0, 0, 0);
    shell::call("remove AppBar", shell::UI_DEADLINE, move || unsafe {
        forget_seat(h);
        let mut a = abd(HWND(h as *mut _), ABE_BOTTOM);
        appbar_msg(ABM_REMOVE, &mut a);
    });
}

/// Calls into explorer (AppBar messages, taskbar state, tray icons) on one
/// helper thread, each with a deadline (audit A03). The caller waits for
/// the answer as before (and answers messages sent to it meanwhile, as it
/// did inside SHAppBarMessage), but a stuck explorer costs it at most the
/// deadline once: later calls queue behind the stuck one without waiting,
/// so order is kept and they happen when explorer answers again. No
/// explorer call ever runs on a caller's own thread.
pub(crate) mod shell {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use crate::idle::IdleQueue;

    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
    use windows::Win32::UI::WindowsAndMessaging::{
        MsgWaitForMultipleObjectsEx, PeekMessageW, MSG, MWMO_INPUTAVAILABLE, PM_NOREMOVE, PM_QS_SENDMESSAGE, QS_SENDMESSAGE,
    };

    /// How long the dock's UI thread waits for one explorer call (the
    /// guardian's hang check gives the taskbar back after ~15 s).
    pub const UI_DEADLINE: Duration = Duration::from_secs(3);
    /// Calls waiting behind a stuck one; more are dropped (logged).
    const QUEUE: usize = 32;

    type Job = Box<dyn FnOnce() + Send>;
    /// The helper thread ends after this long without a call.
    const HELPER_IDLE: Duration = Duration::from_secs(30);
    /// A call ran past its caller's deadline and has not finished.
    static STUCK: AtomicBool = AtomicBool::new(false);
    /// Calls queued or running.
    static PENDING: AtomicUsize = AtomicUsize::new(0);
    thread_local! {
        /// This thread is the helper itself: nested calls run inline.
        static WORKER: Cell<bool> = const { Cell::new(false) };
        /// This thread already waits in call(): a call made from a message
        /// answered meanwhile is queued, not waited for (waiting would
        /// sit behind the outer call that waits on us).
        static WAITING: Cell<bool> = const { Cell::new(false) };
    }

    struct Event(HANDLE);
    // A kernel event handle: usable from any thread.
    unsafe impl Send for Event {}
    unsafe impl Sync for Event {}
    impl Drop for Event {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// The helper's queue. Its thread starts with the first call and ends
    /// after a quiet while (crate::idle); a stuck call keeps it, and the
    /// calls behind it wait in order.
    fn sender() -> Option<&'static IdleQueue<Job>> {
        static Q: OnceLock<IdleQueue<Job>> = OnceLock::new();
        Some(Q.get_or_init(|| {
            IdleQueue::new("shell-calls", HELPER_IDLE, QUEUE, |jobs: &mut crate::idle::Jobs<Job>| {
                WORKER.with(|w| w.set(true));
                while let Some(job) = jobs.next() {
                    job();
                    STUCK.store(false, Ordering::SeqCst);
                    PENDING.fetch_sub(1, Ordering::SeqCst);
                }
            })
        }))
    }

    fn enqueue(tx: &IdleQueue<Job>, job: Job) -> bool {
        PENDING.fetch_add(1, Ordering::SeqCst);
        if tx.send(job).is_ok() {
            return true;
        }
        PENDING.fetch_sub(1, Ordering::SeqCst);
        false
    }

    /// Queues `f` without waiting for it.
    fn fire<R: Send + 'static>(what: &str, tx: &IdleQueue<Job>, f: impl FnOnce() -> R + Send + 'static, why: &str) {
        let job: Job = Box::new(move || {
            let _ = f();
        });
        if enqueue(tx, job) {
            crate::log!("shell: {why}; \"{what}\" queued");
        } else {
            crate::log!("shell: {why}; \"{what}\" dropped (queue full)");
        }
    }

    /// Runs `f` on the helper and waits up to `deadline` for its result.
    /// None: it did not finish in time (it still runs, in order), was only
    /// queued (see above) or could not be queued.
    pub fn call<R: Send + 'static>(what: &str, deadline: Duration, f: impl FnOnce() -> R + Send + 'static) -> Option<R> {
        if WORKER.with(|w| w.get()) {
            return Some(f());
        }
        let tx = sender()?;
        if WAITING.with(|w| w.get()) {
            fire(what, tx, f, "asked while waiting for explorer");
            return None;
        }
        if STUCK.load(Ordering::SeqCst) {
            fire(what, tx, f, "explorer is not answering");
            return None;
        }
        let Ok(ev) = (unsafe { CreateEventW(None, true, false, None) }) else {
            fire(what, tx, f, "no event to wait on");
            return None;
        };
        let ev = Arc::new(Event(ev));
        let slot: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
        let (ev2, slot2) = (ev.clone(), slot.clone());
        let job: Job = Box::new(move || {
            let r = f();
            if let Ok(mut s) = slot2.lock() {
                *s = Some(r);
            }
            unsafe {
                let _ = SetEvent(ev2.0);
            }
        });
        if !enqueue(tx, job) {
            crate::log!("shell: \"{what}\" could not be queued");
            return None;
        }
        WAITING.with(|w| w.set(true));
        let done = wait(ev.0, deadline);
        WAITING.with(|w| w.set(false));
        if !done {
            STUCK.store(true, Ordering::SeqCst);
            // Finished right at the deadline: not stuck after all.
            if unsafe { WaitForSingleObject(ev.0, 0) } != WAIT_OBJECT_0 {
                crate::log!("shell: \"{what}\" took over {} ms; going on without it", deadline.as_millis());
                return None;
            }
            STUCK.store(false, Ordering::SeqCst);
        }
        let r = slot.lock().ok().and_then(|mut s| s.take());
        r
    }

    /// A lane of its own for optional calls (answers only a cache wants:
    /// where our tray icon is): never queued in front of the calls the UI
    /// waits for, so one explorer is slow to answer never holds those up.
    /// At most one waits (the newest: an older one would only be replaced
    /// by its answer), behind at most one running.
    pub(crate) struct Lane {
        next: Arc<Mutex<Option<Job>>>,
        /// Wakes its thread (one wake at most waits: it sees the newest).
        wake: IdleQueue<()>,
    }

    impl Lane {
        pub(crate) fn new(name: &'static str) -> Self {
            let next: Arc<Mutex<Option<Job>>> = Arc::new(Mutex::new(None));
            let slot = next.clone();
            let wake = IdleQueue::new(name, HELPER_IDLE, 1, move |jobs: &mut crate::idle::Jobs<()>| {
                while jobs.next().is_some() {
                    while let Some(job) = slot.lock().ok().and_then(|mut g| g.take()) {
                        job();
                    }
                }
            });
            Self { next, wake }
        }

        /// Queues `job` in place of the one waiting (if any) and returns at
        /// once. False if no thread could take it.
        pub(crate) fn post(&self, job: Job) -> bool {
            match self.next.lock() {
                Ok(mut g) => *g = Some(job),
                Err(_) => return false,
            }
            // Full: a wake already waits and will find the job.
            self.wake.send(()).is_ok() || self.wake.running()
        }
    }

    /// Queues `f` on the optional lane (see Lane) and returns at once (`f`
    /// hands its result on itself); a call of this kind still waiting is
    /// replaced. Never waits and never marks explorer as stuck. False if
    /// it could not be queued.
    pub fn post_latest(what: &str, f: impl FnOnce() + Send + 'static) -> bool {
        static LANE: OnceLock<Lane> = OnceLock::new();
        if LANE.get_or_init(|| Lane::new("shell-optional")).post(Box::new(f)) {
            return true;
        }
        crate::log!("shell: \"{what}\" dropped (no thread)");
        false
    }

    /// No call queued or running.
    pub fn idle() -> bool {
        PENDING.load(Ordering::SeqCst) == 0
    }

    /// Waits (without a message loop: for the guardian) until every queued
    /// call has run, at most `max`. True if they all did.
    pub fn wait_idle(max: Duration) -> bool {
        let start = Instant::now();
        while !idle() {
            if start.elapsed() >= max {
                return false;
            }
            // Not a plain sleep: messages sent to this thread meanwhile are
            // answered (on the dock's UI thread at exit, explorer may send
            // to our windows while it handles the restore; a sleeping
            // thread would hold it, and the restore, up until we exit).
            unsafe {
                if MsgWaitForMultipleObjectsEx(None, 50, QS_SENDMESSAGE, MWMO_INPUTAVAILABLE).0 == WAIT_OBJECT_0.0 {
                    let mut msg = MSG::default();
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
                }
            }
        }
        true
    }

    /// Waits for `ev`, answering messages other threads send to this one
    /// (explorer and our hooks do, during AppBar calls) like SendMessage.
    fn wait(ev: HANDLE, deadline: Duration) -> bool {
        let start = Instant::now();
        loop {
            let left = deadline.saturating_sub(start.elapsed());
            let r = unsafe { MsgWaitForMultipleObjectsEx(Some(&[ev]), left.as_millis() as u32, QS_SENDMESSAGE, MWMO_INPUTAVAILABLE) };
            if r == WAIT_OBJECT_0 {
                return true;
            }
            if r.0 == WAIT_OBJECT_0.0 + 1 && !left.is_zero() {
                let mut msg = MSG::default();
                // Dispatches the sent messages only; posted ones stay queued.
                unsafe {
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE | PM_QS_SENDMESSAGE);
                }
                continue;
            }
            return false;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn waits_for_answers_but_not_for_a_stuck_explorer() {
            assert_eq!(call("quick", Duration::from_secs(5), || 7), Some(7));
            // A call made while this thread waits is queued, not run here.
            let me = std::thread::current().id();
            let inner_ran_on = Arc::new(Mutex::new(None));
            let inner2 = inner_ran_on.clone();
            assert_eq!(
                call("outer", Duration::from_secs(5), move || {
                    // (On the helper: a nested call runs inline there.)
                    call("nested on helper", Duration::from_secs(5), || 1)
                }),
                Some(Some(1))
            );
            WAITING.with(|w| w.set(true));
            assert_eq!(call("reentered", Duration::from_secs(5), move || *inner2.lock().unwrap() = Some(std::thread::current().id())), None);
            WAITING.with(|w| w.set(false));
            assert!(wait_idle(Duration::from_secs(5)));
            assert_ne!(*inner_ran_on.lock().unwrap(), Some(me), "never on the caller's thread");
            assert!(inner_ran_on.lock().unwrap().is_some());
            // Too slow: the caller goes on after the deadline...
            let t = Instant::now();
            let release = Arc::new(AtomicBool::new(false));
            let r2 = release.clone();
            assert_eq!(
                call("slow", Duration::from_millis(100), move || {
                    while !r2.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    1
                }),
                None
            );
            assert!(t.elapsed() < Duration::from_secs(2));
            assert!(!idle());
            // ...and while it is stuck, later calls do not wait at all but
            // still run, in order.
            let ran = Arc::new(AtomicBool::new(false));
            let ran2 = ran.clone();
            let t = Instant::now();
            assert_eq!(call("queued", Duration::from_secs(5), move || ran2.store(true, Ordering::SeqCst)), None);
            assert!(t.elapsed() < Duration::from_millis(50));
            assert!(!wait_idle(Duration::from_millis(50)), "still stuck");
            release.store(true, Ordering::SeqCst);
            assert!(wait_idle(Duration::from_secs(5)));
            assert!(ran.load(Ordering::SeqCst));
            // Unstuck: answers are waited for again.
            assert_eq!(call("again", Duration::from_secs(5), || "ok"), Some("ok"));
            // An optional call explorer does not answer (the tray icon's
            // rect, posted right after NIM_ADD) never holds a waited one
            // (NIM_SETVERSION) behind it, nor marks explorer as stuck.
            let held = Arc::new(AtomicBool::new(false));
            let h2 = held.clone();
            assert!(post_latest("stuck rect", move || {
                let t = Instant::now();
                while !h2.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(10) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }));
            let t = Instant::now();
            assert_eq!(call("version after rect", Duration::from_secs(3), || 4), Some(4));
            assert!(t.elapsed() < Duration::from_secs(1), "not behind the optional call");
            assert!(!STUCK.load(Ordering::SeqCst));
            held.store(true, Ordering::SeqCst);
        }

        #[test]
        fn the_optional_lane_keeps_only_the_newest_waiting_call() {
            let lane = Lane::new("t-optional");
            let ran = Arc::new(Mutex::new(Vec::new()));
            let go = Arc::new(AtomicBool::new(false));
            let started = Arc::new(AtomicBool::new(false));
            let (r, g, s) = (ran.clone(), go.clone(), started.clone());
            assert!(lane.post(Box::new(move || {
                s.store(true, Ordering::SeqCst);
                while !g.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                r.lock().unwrap().push(1);
            })));
            let t = Instant::now();
            while !started.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(2));
            }
            // While the first is running: two more, only the newest stays.
            for n in [2, 3] {
                let r = ran.clone();
                assert!(lane.post(Box::new(move || r.lock().unwrap().push(n))));
            }
            go.store(true, Ordering::SeqCst);
            let t = Instant::now();
            while ran.lock().unwrap().len() < 2 && t.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(2));
            }
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(*ran.lock().unwrap(), vec![1, 3]);
        }
    }
}

#[cfg(test)]
mod reserve_tests {
    use super::{own_px_on, strip_excluded, Edge, Own, RECT};
    use crate::config::Position;
    use crate::placement::{dock_reserve, hide_verdict, BarSeen, BarSpace, HideCheck};
    use windows::Win32::UI::Shell::{ABE_BOTTOM, ABE_TOP};

    /// What the top bar and the dock note for the hide check (note_own's
    /// entries, through Own::new as reserve() makes them), for a bar `h`
    /// px at the bottom reserving or not and a dock reserving `d` px.
    fn noted(mon: RECT, h: i32, reserves: bool, d: i32) -> (Vec<Own>, i32) {
        let bar_t = if reserves { h } else { 0 };
        let space = BarSpace { top: 0, bottom: h, bottom_free: if reserves { 0 } else { h } };
        let r = dock_reserve(Position::Bottom, d, space);
        let mut own: Vec<Own> = Vec::new();
        own.extend(Own::new(1, mon, ABE_BOTTOM, bar_t, bar_t, bar_t));
        own.extend(Own::new(2, mon, ABE_BOTTOM, d, r.reach, r.own));
        // What the shell keeps out: the bar's strip and the dock's, or the
        // dock's reach over the bar.
        let inset = (bar_t + d).max(r.reach);
        (own, inset)
    }

    #[test]
    fn what_the_dock_reserves_for_the_bar_is_ours() {
        let m1 = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let m2 = RECT { left: 1920, top: -120, right: 4480, bottom: 1320 };
        // A hidden taskbar slid away, 2 px left on its monitor.
        let slid = |m: RECT| RECT { top: m.bottom - 2, bottom: m.bottom + 46, ..m };
        for reserves in [true, false] {
            for d in [0, 70] {
                for m in [m1, m2] {
                    let (mut own, inset) = noted(m, 32, reserves, d);
                    // A bar on top of another display adds nothing here.
                    own.extend(Own::new(3, m1, ABE_TOP, 32, 32, 32));
                    let what = format!("reserves={reserves} dock={d} {m:?}");
                    let own_px = own_px_on(&own, m, Edge::Bottom);
                    assert_eq!(own_px, inset, "{what}: ours, counted once");
                    let seen = BarSeen { mon: m, work: RECT { bottom: m.bottom - inset, ..m }, bar: slid(m), own_px };
                    assert_eq!(hide_verdict(&[seen]), HideCheck::Freed, "{what}");
                    // The taskbar still keeps its 48 px: seen, not hidden.
                    let kept = BarSeen { work: RECT { bottom: m.bottom - inset - 48, ..m }, bar: RECT { top: m.bottom - 48, ..m }, ..seen };
                    assert_eq!(hide_verdict(&[kept]), HideCheck::Waiting, "{what}");
                }
            }
        }
        // Nothing reserved: nothing noted.
        assert!(Own::new(4, m1, ABE_BOTTOM, 0, 102, 102).is_none());
    }

    const MON: RECT = RECT { left: 0, top: 0, right: 2560, bottom: 1600 };

    #[test]
    fn a_kept_strip_is_out_of_the_work_area() {
        // Measured: top bar 48 px, taskbar hidden.
        let work = RECT { top: 48, ..MON };
        assert!(strip_excluded(MON, work, Edge::Top, 48));
        // After the explorer restart the shell forgot it: work area = monitor.
        assert!(!strip_excluded(MON, MON, Edge::Top, 48));
        // Below a taskbar on the top edge: the strip reaches further.
        assert!(!strip_excluded(MON, work, Edge::Top, 96));
        assert!(strip_excluded(MON, RECT { top: 96, ..MON }, Edge::Top, 96));
    }

    #[test]
    fn every_edge() {
        let px = 80;
        assert!(strip_excluded(MON, RECT { bottom: 1600 - 64 - px, ..MON }, Edge::Bottom, px));
        assert!(!strip_excluded(MON, RECT { bottom: 1600 - 10, ..MON }, Edge::Bottom, px));
        assert!(strip_excluded(MON, RECT { left: px, ..MON }, Edge::Left, px));
        assert!(!strip_excluded(MON, MON, Edge::Left, px));
        assert!(strip_excluded(MON, RECT { right: 2560 - px, ..MON }, Edge::Right, px));
        assert!(!strip_excluded(MON, MON, Edge::Right, px));
        // A monitor not at the origin.
        let m2 = RECT { left: 2560, top: -200, right: 4480, bottom: 880 };
        assert!(strip_excluded(m2, RECT { top: -200 + 40, ..m2 }, Edge::Top, 40));
        assert!(!strip_excluded(m2, m2, Edge::Top, 40));
    }
}
