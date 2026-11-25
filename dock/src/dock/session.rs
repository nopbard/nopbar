//! Sleep, lock and session switches (audit A09). After resume, unlock or a
//! remote/console reconnect, ordinary change events may never arrive (the
//! displays, explorer, audio devices and the network changed while we were
//! not looking), so one coalesced resync re-reads everything: monitors and
//! work areas, both appbars, the taskbar's hidden state, the tray spy's
//! place in front, the audio/network listener, the window list, the
//! wallpaper and the Recycle Bin. A second, later pass catches what settles
//! slowly (explorer, a docking station's monitors). While the session is
//! locked, asleep or disconnected, or the display is off, background
//! polling stops: the hung-window check, speaker levels, the tray spy's
//! watchdog and the bar clock's seconds (they start again once the user is
//! back).

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, KillTimer, SetTimer};

use super::*;

/// WM_WTSSESSION_CHANGE.
pub(super) const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
pub(super) const RESYNC_TIMER: usize = 46;
pub(super) const RESYNC_LATE_TIMER: usize = 47;
/// Resync this long after the event (bursts: resume brings several).
const RESYNC_MS: u32 = 1500;
/// The second pass.
const RESYNC_LATE_MS: u32 = 6000;

// WM_POWERBROADCAST wParam values.
const PBT_POWERSETTINGCHANGE: usize = 0x8013;
const PBT_APMSUSPEND: usize = 0x4;
const PBT_APMRESUMESUSPEND: usize = 0x7;
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;
// WM_WTSSESSION_CHANGE wParam values.
const WTS_CONSOLE_CONNECT: usize = 0x1;
const WTS_CONSOLE_DISCONNECT: usize = 0x2;
const WTS_REMOTE_CONNECT: usize = 0x3;
const WTS_REMOTE_DISCONNECT: usize = 0x4;
const WTS_SESSION_LOGON: usize = 0x5;
const WTS_SESSION_LOCK: usize = 0x7;
const WTS_SESSION_UNLOCK: usize = 0x8;
const WTS_SESSION_REMOTE_CONTROL: usize = 0x9;

/// Why the user is away. Sleep, lock and disconnect come and go on their
/// own (a locked session can sleep and wake up still locked; a remote
/// session can be disconnected while locked), so each is tracked apart and
/// the user counts as back only when none is left (stability review).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Presence {
    pub asleep: bool,
    pub locked: bool,
    pub disconnected: bool,
    /// This session's display is off (GUID_SESSION_DISPLAY_STATUS 0;
    /// dimmed counts as on). The session's, not the console's: a remote
    /// session shows while the local screen stays dark. Not "away" for the
    /// resync: only polling stops for it.
    pub display_off: bool,
    /// Something happened while away that needs a resync once back (a
    /// resume, a reconnect, an unlock).
    pub resync_due: bool,
}

impl Presence {
    pub fn away(&self) -> bool {
        self.asleep || self.locked || self.disconnected
    }

    /// Nobody can see the dock or the bar: away, or the display is off.
    /// Background polling waits for the user.
    pub fn idle(&self) -> bool {
        self.away() || self.display_off
    }

    /// The display went off or came back (no resync for it).
    pub fn display(&mut self, on: bool) {
        self.display_off = !on;
    }

    /// Takes a power broadcast; true if a resync should be scheduled now.
    pub fn power(&mut self, wp: usize) -> bool {
        match wp {
            PBT_APMSUSPEND => self.asleep = true,
            PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => {
                self.asleep = false;
                self.resync_due = true;
            }
            _ => return false,
        }
        self.ready()
    }

    /// Takes a session change; true if a resync should be scheduled now.
    pub fn session(&mut self, wp: usize) -> bool {
        match wp {
            WTS_SESSION_LOCK => self.locked = true,
            WTS_SESSION_UNLOCK => self.locked = false,
            WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => self.disconnected = true,
            WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => self.disconnected = false,
            // Signed in again (fast user switching back): neither.
            WTS_SESSION_LOGON => {
                self.locked = false;
                self.disconnected = false;
            }
            WTS_SESSION_REMOTE_CONTROL => {}
            _ => return false,
        }
        // Every one of these may have changed displays, explorer or audio
        // while we were away.
        self.resync_due = true;
        self.ready()
    }

    /// Back and a resync is owed: true once (it is then no longer due).
    fn ready(&mut self) -> bool {
        if self.away() || !self.resync_due {
            return false;
        }
        self.resync_due = false;
        true
    }

    /// A scheduled resync came due: run it only if the user is still here;
    /// if they left again it waits until they are back.
    pub fn run_now(&mut self) -> bool {
        if self.away() {
            self.resync_due = true;
            return false;
        }
        true
    }
}

static PRESENCE: std::sync::Mutex<Presence> =
    std::sync::Mutex::new(Presence { asleep: false, locked: false, disconnected: false, display_off: false, resync_due: false });
static REGISTERED: AtomicBool = AtomicBool::new(false);
/// The display on / off notification (HPOWERNOTIFY), 0 = none.
static DISPLAY_NOTIFY: AtomicIsize = AtomicIsize::new(0);

fn presence<R>(f: impl FnOnce(&mut Presence) -> R) -> Option<R> {
    PRESENCE.lock().ok().map(|mut p| f(&mut p))
}

/// Away, or the display is off: no background polling (see Presence::idle).
pub(super) fn idle() -> bool {
    presence(|p| p.idle()).unwrap_or(false)
}

/// Session notifications for the dock window (unregistered in `stop`).
pub(super) unsafe fn start(hwnd: HWND) {
    if WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION).is_ok() {
        REGISTERED.store(true, Ordering::SeqCst);
    } else {
        crate::log!("session: no lock/unlock notifications; resume still resyncs");
    }
    // Display on / off (sent once now with the current state, then on
    // every change).
    use windows::Win32::System::Power::RegisterPowerSettingNotification;
    use windows::Win32::System::SystemServices::GUID_SESSION_DISPLAY_STATUS;
    use windows::Win32::UI::WindowsAndMessaging::REGISTER_NOTIFICATION_FLAGS;
    match RegisterPowerSettingNotification(windows::Win32::Foundation::HANDLE(hwnd.0), &GUID_SESSION_DISPLAY_STATUS, REGISTER_NOTIFICATION_FLAGS(0)) {
        Ok(h) => DISPLAY_NOTIFY.store(h.0 as isize, Ordering::SeqCst),
        Err(e) => crate::log!("session: no display on/off notifications ({e}); polling pauses only when locked or asleep"),
    }
}

pub(super) unsafe fn stop(hwnd: HWND) {
    if REGISTERED.swap(false, Ordering::SeqCst) {
        let _ = WTSUnRegisterSessionNotification(hwnd);
    }
    let h = DISPLAY_NOTIFY.swap(0, Ordering::SeqCst);
    if h != 0 {
        let _ = windows::Win32::System::Power::UnregisterPowerSettingNotification(windows::Win32::System::Power::HPOWERNOTIFY(h as _));
    }
}

/// The display state in a PBT_POWERSETTINGCHANGE (`lp`): Some(on).
unsafe fn display_state(lp: isize) -> Option<bool> {
    use windows::Win32::System::Power::POWERBROADCAST_SETTING;
    use windows::Win32::System::SystemServices::GUID_SESSION_DISPLAY_STATUS;
    if lp == 0 {
        return None;
    }
    let s = &*(lp as *const POWERBROADCAST_SETTING);
    if s.PowerSetting != GUID_SESSION_DISPLAY_STATUS || s.DataLength < 4 {
        return None;
    }
    // 0 off, 1 on, 2 dimmed (still on).
    Some(u32::from_le_bytes(std::ptr::read_unaligned(s.Data.as_ptr() as *const [u8; 4])) != 0)
}

/// WM_POWERBROADCAST (`power`, with its lParam `lp`) or WM_WTSSESSION_CHANGE.
pub(super) unsafe fn on_message(hwnd: HWND, power: bool, wp: usize, lp: isize) {
    let display = if power && wp == PBT_POWERSETTINGCHANGE { display_state(lp) } else { None };
    if power && wp == PBT_POWERSETTINGCHANGE && display.is_none() {
        return;
    }
    let Some((was_away, was_idle, now)) = presence(|p| {
        let (was, was_idle) = (p.away(), p.idle());
        let go = match display {
            Some(on) => {
                p.display(on);
                false
            }
            None if power => p.power(wp),
            None => p.session(wp),
        };
        (was, was_idle, (go, *p))
    }) else {
        return;
    };
    let (go, p) = now;
    if p.away() != was_away { crate::companion::session_away(p.away()); }
    if p.idle() != was_idle {
        idle_changed(p.idle());
        if display.is_some() {
            crate::log!("session: display {}; background polling {}", if p.display_off { "off" } else { "on" }, if p.idle() { "paused" } else { "back" });
        }
    }
    if p.away() && !was_away {
        crate::log!("session: away (asleep {}, locked {}, disconnected {}); background polling paused", p.asleep, p.locked, p.disconnected);
        // A resync scheduled a moment ago waits until the user is back.
        let _ = KillTimer(Some(hwnd), RESYNC_TIMER);
        let _ = KillTimer(Some(hwnd), RESYNC_LATE_TIMER);
    }
    if go {
        crate::log!("session: back; resync scheduled");
        // Coalesced: a burst (resume + unlock) gives one resync.
        SetTimer(Some(hwnd), RESYNC_TIMER, RESYNC_MS, None);
        SetTimer(Some(hwnd), RESYNC_LATE_TIMER, RESYNC_LATE_MS, None);
    }
}

/// Nobody can see us (`idle`), or someone can again: the background
/// polling stops or starts (hung windows, speaker levels, the tray spy's
/// watchdog).
fn idle_changed(idle: bool) {
    crate::tray::set_quiet(idle);
    super::views::for_each(|d| {
        // Speaker polling stops while idle and comes back if the pointer
        // still rests on the dock.
        d.sound_resume();
        // Stops the hung-window timer while idle, starts it again after.
        d.hang_sync();
        d.sync_edge_cursor();
    });
    // The bar clock's seconds rest, or tick again from the next second; so
    // does an open calendar's large clock.
    crate::topbar::session_idle_changed();
    crate::popup::session_idle_changed();
    // Calendar fetches rest too, and catch up once someone is back.
    crate::agenda::session_idle_changed();
}

/// RESYNC_TIMER / RESYNC_LATE_TIMER.
pub(super) unsafe fn on_timer(hwnd: HWND, id: usize) {
    let _ = KillTimer(Some(hwnd), id);
    if SHUTTING_DOWN.load(Ordering::SeqCst) {
        return;
    }
    // Gone again (locked or slept right after coming back): kept due.
    if !presence(|p| p.run_now()).unwrap_or(true) {
        return;
    }
    if !resync(id == RESYNC_TIMER) {
        // The dock or the bars are busy (a menu is open): shortly.
        SetTimer(Some(hwnd), id, 300, None);
    }
}

/// Re-reads and re-asserts everything. `full`: the first pass (also the
/// wallpaper and the Recycle Bin). False if the dock was busy.
unsafe fn resync(full: bool) -> bool {
    gpu::check();
    let done = super::views::with_primary(|d| {
        // Monitors and work areas: one top bar each, the dock on its own.
        crate::topbar::apply_config(&d.gfx, &d.cfg, &d.theme);
        crate::corners::displays_changed();
        trayicon::displays_changed();
        // The appbar and the taskbar's hidden state, as configured.
        d.apply_taskbar_mode();
        d.replace_on_monitor();
        if full {
            // The wallpaper may have changed while the session was away.
            crate::wallpaper::invalidate();
            d.refresh_mica();
            d.refresh_trash();
        }
        d.report_active(GetForegroundWindow());
    });
    if done.is_none() || !crate::topbar::resync() {
        return false;
    }
    // The other displays' docks: there, re-placed, their AppBars back.
    super::views::post_reconcile();
    if full {
        super::views::for_views(|d| {
            d.refresh_mica();
            d.refresh_trash();
        });
    }
    if HIDE_TASKBAR.load(Ordering::SeqCst) {
        taskbar::rehide();
    }
    // Explorer may have restarted while we were away.
    hook_explorer();
    worker::watch_explorer(taskbar::explorer_pid());
    schedule_refresh();
    schedule_fullscreen_check();
    crate::log!("session: resync {} done", if full { "pass 1" } else { "pass 2" });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waking_up_locked_is_still_away() {
        let mut p = Presence::default();
        assert!(!p.session(WTS_SESSION_LOCK));
        assert!(!p.power(PBT_APMSUSPEND));
        assert!(!p.power(PBT_APMRESUMEAUTOMATIC), "awake but still locked: no resync yet");
        assert!(p.away() && p.locked && !p.asleep);
        assert!(p.session(WTS_SESSION_UNLOCK), "unlocked: now the owed resync");
        assert!(!p.away());
        assert!(!p.session(0x6), "logoff: WM_ENDSESSION handles it");
    }

    #[test]
    fn a_disconnected_locked_session_needs_both_back() {
        let mut p = Presence::default();
        p.session(WTS_SESSION_LOCK);
        p.session(WTS_REMOTE_DISCONNECT);
        assert!(!p.session(WTS_REMOTE_CONNECT), "connected, still locked");
        assert!(p.session(WTS_SESSION_UNLOCK));
        // Plain resume while present: resync at once.
        assert!(p.power(PBT_APMRESUMESUSPEND));
        assert!(!p.power(0xA), "power status change: the bar reads the battery");
    }

    #[test]
    fn a_dark_display_pauses_polling_but_owes_no_resync() {
        let mut p = Presence::default();
        p.display(false);
        assert!(p.idle() && !p.away(), "display off: idle, not away");
        assert!(!p.resync_due);
        p.display(true);
        assert!(!p.idle());
        // Locked with the display on is idle too; unlocked with it off still is.
        p.session(WTS_SESSION_LOCK);
        assert!(p.idle());
        p.display(false);
        assert!(p.session(WTS_SESSION_UNLOCK), "the unlock's resync runs (the display is not away)");
        assert!(p.idle(), "still dark");
        p.display(true);
        assert!(!p.idle());
    }

    #[test]
    fn a_resync_due_while_away_again_waits() {
        let mut p = Presence::default();
        assert!(p.session(WTS_SESSION_UNLOCK));
        p.session(WTS_SESSION_LOCK); // locked again before the timer fired
        assert!(!p.run_now());
        assert!(p.resync_due);
        assert!(p.session(WTS_SESSION_UNLOCK), "runs once back");
        assert!(p.run_now());
    }
}
