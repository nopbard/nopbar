//! "Send to tray" for any window (the dock's window menu, a preview card's
//! menu, a configurable shortcut): the window is MINIMIZED (a posted
//! SC_MINIMIZE, so a hung app never holds us) and tucked:
//! the top bar's tray cell lists it (topbar/hidden.rs) with its app's icon
//! and title, and the dock leaves it out of the app's windows (previews,
//! the window menu, cycling) while the app keeps its running dot and gets
//! a small tray badge. A click on the entry restores and focuses it.
//!
//! It is never hidden. The tuck list lives in memory only (no file, no
//! restore paths): if NopBar quits or crashes, the windows are plain
//! minimized windows, and Alt+Tab, Task View and the taskbar show them as
//! always. Nothing can be lost.
//!
//! A window leaves the list when it closes, or when it is not minimized
//! any more at a window-list refresh (restored by Alt+Tab, Task View, the
//! app itself, or our entry). Identity is the handle and what owned it when
//! it was sent (process, thread, window class), checked with IsWindow on
//! every refresh. A minimize not seen yet, and a window shown while its
//! minimize was still on its way, get one one-shot recheck each
//! (RECHECK_TIMER at the nearest due time; no timer while none is pending):
//! Show behind a pending minimize posts SC_RESTORE after it.
//!
//! A window in the tray is never closed from NopBar: no Close on its entry,
//! and the dock's Close / Close all / middle-click act on its other
//! windows only. To close one, show it first (then it is an ordinary
//! window). No message can reach a window that took over a stale handle.
//!
//! The list and its rules are pure (`Tuck`) and tested below; the Win32
//! reads that feed them are cheap and never wait on the window's app.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::UI::Composition::CompositionBrush;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::apps::AppWindow;

/// Posted to the dock window when the list changed: the dock syncs its
/// windows, the top bar redraws its cell.
pub const WM_TRAYWIN: u32 = WM_APP + 133;
/// At most this many windows in the tray at once.
pub const MAX_HIDDEN: usize = 64;
/// A window just sent may take this long to be minimized (its app handles
/// the posted request); after that, one that never was minimized leaves
/// the list.
const GRACE: Duration = Duration::from_secs(5);

/// Why a window is not sent to the tray (or not pinned on top:
/// dock/ontop.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// One of NopBar's own windows.
    Own,
    /// The desktop or a taskbar.
    Shell,
    /// Runs with higher rights than NopBar (UIPI drops our requests).
    Elevated,
    /// Not responding.
    Hung,
    /// Gone, invisible or cloaked (nothing to minimize).
    Gone,
    /// Already in the tray.
    Already,
    /// MAX_HIDDEN reached.
    Full,
}

impl Refusal {
    /// Text key of the reason "Send to tray" was refused.
    pub fn why_key(self) -> &'static str {
        match self {
            Refusal::Own | Refusal::Shell | Refusal::Gone | Refusal::Already => "traywin.refused.why",
            Refusal::Elevated => "traywin.refused.elevated",
            Refusal::Hung => "traywin.refused.hung",
            Refusal::Full => "traywin.refused.full",
        }
    }

    /// Text key of the reason "Always on top" was refused.
    pub fn ontop_key(self) -> &'static str {
        match self {
            Refusal::Elevated => "ontop.refused.elevated",
            Refusal::Hung => "ontop.refused.hung",
            _ => "ontop.refused.why",
        }
    }
}

/// What is known about a window, read without waiting on its app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub alive: bool,
    pub ours: bool,
    pub shell: bool,
    pub cloaked: bool,
    pub visible: bool,
    pub locked: bool,
    pub hung: bool,
}

/// Window classes of the shell, never sent nor pinned.
pub const SHELL_CLASSES: [&str; 6] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd", "NotifyIconOverflowWindow", "TopLevelWindowForOverflowXamlIsland"];

/// Pure: may a window with `f` be sent to the tray (None) or why not.
/// `already`: it is in the list; `count`: how many are. UWP frames
/// (ApplicationFrameWindow) minimize like any window: allowed.
pub fn tuck_refusal(f: &Facts, already: bool, count: usize) -> Option<Refusal> {
    if !f.alive {
        return Some(Refusal::Gone);
    }
    if f.ours {
        return Some(Refusal::Own);
    }
    if f.shell {
        return Some(Refusal::Shell);
    }
    if already {
        return Some(Refusal::Already);
    }
    if f.cloaked || !f.visible {
        return Some(Refusal::Gone);
    }
    if f.locked {
        return Some(Refusal::Elevated);
    }
    if f.hung {
        return Some(Refusal::Hung);
    }
    if count >= MAX_HIDDEN {
        return Some(Refusal::Full);
    }
    None
}

/// The window's class name ("" if unreadable; never sends it a message).
pub fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// Reads `h`'s facts (cheap: nothing here waits on its app).
pub fn facts(h: HWND) -> Facts {
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    unsafe {
        if h.is_invalid() || !IsWindow(Some(h)).as_bool() {
            return Facts::default();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let class = class_of(h);
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
        let ours = pid == std::process::id();
        Facts {
            alive: true,
            ours,
            shell: SHELL_CLASSES.iter().any(|c| c.eq_ignore_ascii_case(&class)) || h == GetDesktopWindow() || h == GetShellWindow(),
            cloaked: cloaked != 0,
            visible: IsWindowVisible(h).as_bool(),
            // UIPI drops a posted SC_MINIMIZE to a window above our rights.
            locked: !ours && crate::apps::is_locked(pid),
            hung: IsHungAppWindow(h).as_bool(),
        }
    }
}

// ---- the tuck list (pure) --------------------------------------------------------

/// After Show: one more look this soon (a minimize still on its way must
/// not leave the window minimized).
const RESTORE_CHECK: Duration = Duration::from_millis(600);

/// Which window an entry is: the handle, and what owned it when it was
/// sent (process, thread, window class). Any of those changed: another
/// window that got the same handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    pub hwnd: isize,
    pub pid: u32,
    pub tid: u32,
    pub class: String,
}

/// One window in the tray.
#[derive(Clone)]
pub struct Entry {
    pub id: Ident,
    pub hwnd: isize,
    pub title: String,
    /// The app's icon (the dock icon's brush; None: a generic glyph).
    pub icon: Option<CompositionBrush>,
    /// Seen minimized since it was sent.
    pub seen_min: bool,
    pub since: Instant,
}

impl Entry {
    pub fn hwnd(&self) -> HWND {
        HWND(self.hwnd as *mut _)
    }
}

/// What a window looks like at a refresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    /// IsWindow, with the same process, thread and class.
    pub alive: bool,
    pub minimized: bool,
}

/// Pure: does an entry stay at a refresh? Gone or another window: no.
/// Minimized: yes. Not minimized after having been: restored some other
/// way, no. Never minimized yet: yes while its minimize may still be on
/// its way (`age` < GRACE).
pub fn stays(now: Now, seen_min: bool, age: Duration) -> bool {
    match (now.alive, now.minimized, seen_min) {
        (false, ..) => false,
        (true, true, _) => true,
        (true, false, true) => false,
        (true, false, false) => age < GRACE,
    }
}

/// A window shown from the tray while its minimize was still on its way,
/// looked at once more at `due`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watch {
    pub id: Ident,
    pub due: Instant,
}

/// The list itself (and the windows just shown, watched once).
#[derive(Default)]
pub struct Tuck {
    pub list: Vec<Entry>,
    pub watch: Vec<Watch>,
}

impl Tuck {
    pub fn contains(&self, h: isize) -> bool {
        self.list.iter().any(|e| e.hwnd == h)
    }

    /// Adds `e` unless it is there already or the list is full. A watch
    /// left from an earlier Show of that window goes: the newer wish (in
    /// the tray, minimized) wins.
    pub fn add(&mut self, e: Entry) -> bool {
        if self.contains(e.hwnd) || self.list.len() >= MAX_HIDDEN {
            return false;
        }
        self.watch.retain(|w| w.id.hwnd != e.hwnd);
        self.list.push(e);
        true
    }

    /// Takes `h` off the list.
    pub fn take(&mut self, h: isize) -> Option<Entry> {
        let i = self.list.iter().position(|e| e.hwnd == h)?;
        Some(self.list.remove(i))
    }

    /// Show: takes `h` off the list. Some(true): its minimize may still be
    /// on its way (never seen minimized), so a restore must follow it, and
    /// it is watched once (at `at` + RESTORE_CHECK). A window seen
    /// minimized gets no watch: nothing is on its way.
    pub fn restore(&mut self, h: isize, at: Instant) -> Option<(Ident, bool)> {
        let e = self.take(h)?;
        self.watch.retain(|w| w.id.hwnd != h);
        let pending = !e.seen_min;
        if pending {
            self.watch.push(Watch { id: e.id.clone(), due: at + RESTORE_CHECK });
        }
        Some((e.id, pending))
    }

    /// A refresh: `now` says how each window looks. Entries go when their
    /// window went (stays). True if the list changed.
    pub fn refresh(&mut self, now: impl Fn(&Ident) -> Now, at: Instant) -> bool {
        let before = self.list.len();
        self.list.retain_mut(|e| {
            let n = now(&e.id);
            let keep = stays(n, e.seen_min, at.saturating_duration_since(e.since));
            e.seen_min |= keep && n.minimized;
            keep
        });
        before != self.list.len()
    }

    /// The watched windows due at `at` (taken off the watch).
    pub fn due(&mut self, at: Instant) -> Vec<Ident> {
        let (due, rest): (Vec<Watch>, Vec<Watch>) = self.watch.drain(..).partition(|w| w.due <= at);
        self.watch = rest;
        due.into_iter().map(|w| w.id).collect()
    }

    /// When the next look is needed (None: nothing pending, no timer):
    /// a minimize not seen yet (its grace ends), a window shown behind a
    /// pending minimize (RESTORE_CHECK).
    pub fn next_due(&self) -> Option<Instant> {
        let grace = self.list.iter().filter(|e| !e.seen_min).map(|e| e.since + GRACE);
        let watch = self.watch.iter().map(|w| w.due);
        grace.chain(watch).min()
    }
}

/// Pure: the dock's rule for an app's windows. Tucked windows are not the
/// app's windows (previews, the window menu, cycling) but they keep the
/// app running (its dot stays, with the tray badge). Returns (the app's
/// windows, its tucked ones).
pub fn split(wins: &[HWND], tucked: &dyn Fn(HWND) -> bool) -> (Vec<HWND>, Vec<HWND>) {
    wins.iter().partition(|&&h| !tucked(h))
}

// ---- the list (dock UI thread) ------------------------------------------------------

thread_local! {
    static TUCK: RefCell<Tuck> = RefCell::new(Tuck::default());
}

/// The dock window (gets WM_TRAYWIN and the recheck timer).
static DOCK: AtomicIsize = AtomicIsize::new(0);

/// One-shot timer on the dock window: the next look (Tuck::next_due).
pub const RECHECK_TIMER: usize = 72;

/// The dock window that hears about changes (dock start).
pub fn attach(dock: HWND) {
    DOCK.store(dock.0 as isize, Ordering::SeqCst);
}

fn dock() -> Option<HWND> {
    let d = DOCK.load(Ordering::SeqCst);
    (d != 0).then(|| HWND(d as *mut _))
}

/// Tells the dock (posted: callers may be inside a dock or bar borrow),
/// and arms the one-shot for the next look (or ends it).
fn changed() {
    if let Some(d) = dock() {
        unsafe {
            let _ = PostMessageW(Some(d), WM_TRAYWIN, WPARAM(0), LPARAM(0));
        }
    }
    arm();
}

/// The one-shot at the next due time; none while nothing is pending.
fn arm() {
    let Some(d) = dock() else { return };
    let next = TUCK.with(|t| t.borrow().next_due());
    unsafe {
        match next {
            Some(at) => {
                let ms = at.saturating_duration_since(Instant::now()).as_millis().clamp(10, u32::MAX as u128) as u32;
                SetTimer(Some(d), RECHECK_TIMER, ms, None);
            }
            None => {
                let _ = KillTimer(Some(d), RECHECK_TIMER);
            }
        }
    }
}

/// The windows in the tray now.
pub fn entries() -> Vec<Entry> {
    TUCK.with(|t| t.borrow().list.clone())
}

pub fn count() -> usize {
    TUCK.with(|t| t.borrow().list.len())
}

pub fn contains(h: HWND) -> bool {
    TUCK.with(|t| t.borrow().contains(h.0 as isize))
}

/// The handle's owner now: (pid, tid).
fn owner(h: HWND) -> (u32, u32) {
    let mut pid = 0u32;
    let tid = unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (pid, tid)
}

/// Is `id`'s handle still that window (process, thread, class)?
fn is(id: &Ident) -> bool {
    let h = HWND(id.hwnd as *mut _);
    if !unsafe { IsWindow(Some(h)).as_bool() } {
        return false;
    }
    owner(h) == (id.pid, id.tid) && class_of(h) == id.class
}

/// Tucks window `w` (as the window list knows it), with the app's `icon`,
/// and minimizes it (a posted SC_MINIMIZE: a hung app never holds us).
/// Refused windows are left as they are; a minimize Windows will not post
/// (UIPI, a window above our rights) takes it off the list again.
pub fn tuck(w: &AppWindow, icon: Option<CompositionBrush>) -> Result<(), Refusal> {
    let f = facts(w.hwnd);
    if let Some(r) = tuck_refusal(&f, contains(w.hwnd), count()) {
        return Err(r);
    }
    let (pid, tid) = owner(w.hwnd);
    let class = class_of(w.hwnd);
    if pid == 0 || pid != w.pid || tid == 0 || class.is_empty() {
        return Err(Refusal::Gone);
    }
    let title = {
        let mut buf = [0u16; 256];
        let n = unsafe { GetWindowTextW(w.hwnd, &mut buf) }.max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    };
    let minimized = unsafe { IsIconic(w.hwnd).as_bool() };
    let h = w.hwnd.0 as isize;
    let e = Entry { id: Ident { hwnd: h, pid, tid, class }, hwnd: h, title, icon, seen_min: minimized, since: Instant::now() };
    if !TUCK.with(|t| t.borrow_mut().add(e)) {
        return Err(Refusal::Full);
    }
    if !minimized {
        let posted = unsafe { PostMessageW(Some(w.hwnd), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0)) };
        if let Err(err) = posted {
            TUCK.with(|t| t.borrow_mut().take(h));
            crate::log!("traywin: {h:#x} not minimized ({err})");
            return Err(if crate::apps::is_locked(pid) { Refusal::Elevated } else { Refusal::Gone });
        }
    }
    crate::log!("traywin: sent {h:#x} (pid {pid}) to the tray");
    changed();
    Ok(())
}

/// Takes `h` off the list and brings it back in front. A minimize still on
/// its way gets a posted SC_RESTORE behind it (the window's queue keeps
/// the order), and only then is the window looked at once more shortly
/// (on_timer).
/// False if it was not in the tray or is not its window any more.
pub fn restore(h: HWND) -> bool {
    let Some((id, pending)) = TUCK.with(|t| t.borrow_mut().restore(h.0 as isize, Instant::now())) else { return false };
    changed();
    if !is(&id) {
        return false;
    }
    if pending {
        unsafe {
            let _ = PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_RESTORE as usize), LPARAM(0));
        }
    }
    crate::dock::bring_to_front(h);
    true
}

/// Every window of these back (a dock icon's click, Show all): the first
/// one ends in front.
pub fn restore_many(hwnds: &[HWND]) {
    for &h in hwnds.iter().rev() {
        restore(h);
    }
}

/// The window list changed: windows that closed, changed hands or are not
/// minimized any more leave the tray. True if what is offered changed.
/// Cheap (IsWindow, IsIconic, the owner, the class): run on every refresh
/// and on the one-shot, never on a repeating timer.
pub fn prune() -> bool {
    let c = TUCK.with(|t| t.borrow_mut().refresh(|id| Now { alive: is(id), minimized: unsafe { IsIconic(HWND(id.hwnd as *mut _)).as_bool() } }, Instant::now()));
    if c {
        changed();
    } else {
        arm();
    }
    c
}

/// RECHECK_TIMER (one-shot): windows shown behind a pending minimize that
/// it minimized after all are restored again (unless they were sent to
/// the tray again meanwhile: add() dropped their watch), then the list is
/// checked.
pub fn on_timer() {
    if let Some(d) = dock() {
        unsafe {
            let _ = KillTimer(Some(d), RECHECK_TIMER);
        }
    }
    let due = TUCK.with(|t| t.borrow_mut().due(Instant::now()));
    for id in due {
        let h = HWND(id.hwnd as *mut _);
        if is(&id) && unsafe { IsIconic(h).as_bool() } {
            crate::log!("traywin: {:#x} was minimized after Show; restoring it again", id.hwnd);
            crate::dock::bring_to_front(h);
        }
    }
    prune();
}

// On quit (or a crash) nothing is done on purpose: tucked windows are
// ordinary minimized windows, which Alt+Tab, Task View and the taskbar
// already show. Restoring them all at once would throw the user's windows
// over the screen while NopBar closes.

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Facts {
        Facts { alive: true, visible: true, ..Default::default() }
    }

    fn ident(h: isize) -> Ident {
        Ident { hwnd: h, pid: 10, tid: 20, class: "Notepad".into() }
    }

    fn entry(h: isize) -> Entry {
        Entry { id: ident(h), hwnd: h, title: format!("w{h}"), icon: None, seen_min: false, since: Instant::now() }
    }

    #[test]
    fn only_ordinary_windows_are_sent() {
        assert_eq!(tuck_refusal(&ok(), false, 0), None);
        assert_eq!(tuck_refusal(&Facts { ours: true, ..ok() }, false, 0), Some(Refusal::Own));
        assert_eq!(tuck_refusal(&Facts { shell: true, ..ok() }, false, 0), Some(Refusal::Shell));
        assert_eq!(tuck_refusal(&Facts { locked: true, ..ok() }, false, 0), Some(Refusal::Elevated), "UIPI would drop the minimize");
        assert_eq!(tuck_refusal(&Facts { hung: true, ..ok() }, false, 0), Some(Refusal::Hung));
        assert_eq!(tuck_refusal(&Facts { cloaked: true, ..ok() }, false, 0), Some(Refusal::Gone));
        assert_eq!(tuck_refusal(&Facts { visible: false, ..ok() }, false, 0), Some(Refusal::Gone));
        assert_eq!(tuck_refusal(&Facts::default(), false, 0), Some(Refusal::Gone));
        assert_eq!(tuck_refusal(&ok(), true, 0), Some(Refusal::Already));
        assert_eq!(tuck_refusal(&ok(), false, MAX_HIDDEN), Some(Refusal::Full));
        assert_eq!(tuck_refusal(&Facts { ours: true, locked: true, hung: true, ..ok() }, true, MAX_HIDDEN), Some(Refusal::Own));
    }

    #[test]
    fn each_action_has_its_own_reasons() {
        for r in [Refusal::Own, Refusal::Shell, Refusal::Elevated, Refusal::Hung, Refusal::Gone, Refusal::Already, Refusal::Full] {
            assert!(r.why_key().starts_with("traywin.") && r.ontop_key().starts_with("ontop."), "{r:?}");
            assert!(crate::i18n::get(r.why_key()).is_some() && crate::i18n::get(r.ontop_key()).is_some());
        }
        assert_ne!(Refusal::Own.why_key(), Refusal::Own.ontop_key());
    }

    #[test]
    fn the_list_adds_once_and_takes_by_click() {
        let mut t = Tuck::default();
        assert!(t.add(entry(1)));
        assert!(!t.add(entry(1)), "already there");
        assert!(t.add(entry(2)));
        assert!(t.contains(1));
        let at = Instant::now();
        assert_eq!(t.restore(1, at).map(|r| r.0.hwnd), Some(1), "a click on its entry");
        assert!(!t.contains(1));
        assert!(t.restore(1, at).is_none());
        for h in 3..=MAX_HIDDEN as isize + 5 {
            t.add(entry(h));
        }
        assert_eq!(t.list.len(), MAX_HIDDEN, "never more than the cap");
    }

    #[test]
    fn windows_leave_when_closed_or_not_minimized_any_more() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut t = Tuck::default();
        for h in 1..=4 {
            t.add(Entry { since: t0, ..entry(h) });
        }
        // 1 minimized; 2 closed; 3 still on its way (not minimized yet); 4 minimized.
        let changed = t.refresh(
            |id| match id.hwnd {
                1 | 4 => Now { alive: true, minimized: true },
                2 => Now { alive: false, minimized: false },
                _ => Now { alive: true, minimized: false },
            },
            at(1),
        );
        assert!(changed);
        assert_eq!(t.list.iter().map(|e| e.hwnd).collect::<Vec<_>>(), vec![1, 3, 4]);
        assert!(t.list[0].seen_min && !t.list[1].seen_min);
        // 1 restored by Alt+Tab / the app; 3's minimize never came.
        t.refresh(|id| Now { alive: true, minimized: id.hwnd == 4 }, at(10));
        assert_eq!(t.list.iter().map(|e| e.hwnd).collect::<Vec<_>>(), vec![4]);
        // Another window got the handle (process, thread or class changed).
        t.refresh(|_| Now { alive: false, minimized: true }, at(11));
        assert!(t.list.is_empty());
    }

    #[test]
    fn a_minimize_on_its_way_gets_a_grace_period_and_a_recheck_when_it_ends() {
        let n = Now { alive: true, minimized: false };
        assert!(stays(n, false, Duration::from_secs(1)));
        assert!(!stays(n, false, GRACE));
        assert!(!stays(n, true, Duration::ZERO), "minimized before, not now: restored elsewhere");
        // The one-shot is due exactly when the grace ends, and none is
        // needed once the window was seen minimized.
        let t0 = Instant::now();
        let mut t = Tuck::default();
        assert_eq!(t.next_due(), None, "nothing pending: no timer");
        t.add(Entry { since: t0, ..entry(1) });
        assert_eq!(t.next_due(), Some(t0 + GRACE));
        // At that recheck the minimize never came: the window is back.
        assert!(t.refresh(|_| n, t0 + GRACE));
        assert!(t.list.is_empty());
        assert_eq!(t.next_due(), None);
        t.add(Entry { since: t0, seen_min: true, ..entry(2) });
        assert_eq!(t.next_due(), None, "minimized already: only window events matter");
    }

    #[test]
    fn show_during_a_pending_minimize_restores_after_it() {
        let t0 = Instant::now();
        let mut t = Tuck::default();
        t.add(Entry { since: t0, ..entry(1) });
        t.add(Entry { since: t0, seen_min: true, ..entry(2) });
        // Not seen minimized yet: a restore must follow the minimize.
        assert_eq!(t.restore(1, t0), Some((ident(1), true)));
        // Minimized already: focusing restores it, and no watch at all.
        assert_eq!(t.restore(2, t0), Some((ident(2), false)));
        // Only the pending one is looked at once more shortly.
        assert_eq!(t.next_due(), Some(t0 + RESTORE_CHECK));
        assert!(t.due(t0).is_empty(), "not yet");
        assert_eq!(t.due(t0 + RESTORE_CHECK), vec![ident(1)]);
        assert!(t.watch.is_empty() && t.next_due().is_none());
    }

    #[test]
    fn sending_it_again_cancels_the_watch_of_an_earlier_show() {
        let t0 = Instant::now();
        let mut t = Tuck::default();
        t.add(Entry { since: t0, ..entry(1) });
        assert_eq!(t.restore(1, t0), Some((ident(1), true)));
        assert_eq!(t.watch.len(), 1);
        // Sent to the tray again before the watch was due: the newer wish
        // wins; the old watch must not bring it back.
        assert!(t.add(Entry { since: t0, seen_min: true, ..entry(1) }));
        assert!(t.watch.is_empty());
        assert!(t.due(t0 + RESTORE_CHECK).is_empty());
        assert_eq!(t.next_due(), None);
    }

    #[test]
    fn tucked_windows_are_not_the_apps_windows() {
        let w = |k: usize| HWND(k as *mut _);
        let tucked = |h: HWND| h == w(2) || h == w(3);
        assert_eq!(split(&[w(1), w(2), w(3)], &tucked), (vec![w(1)], vec![w(2), w(3)]));
        // Only tucked windows: none of its own, still running (the tucked list).
        assert_eq!(split(&[w(2), w(3)], &tucked), (vec![], vec![w(2), w(3)]));
        assert_eq!(split(&[w(1)], &tucked), (vec![w(1)], vec![]));
    }
}
