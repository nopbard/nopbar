//! Shell hook / WinEvent plumbing, explorer tracking, refresh scheduling.

use super::*;

pub(super) unsafe extern "system" fn win_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object == OBJID_WINDOW.0 && id_child == 0 {
        schedule_refresh();
        if event == EVENT_SYSTEM_FOREGROUND {
            follow_foreground(hwnd);
            schedule_fullscreen_check();
        }
    }
}

/// The foreground window moved or resized (e.g. F11).
unsafe extern "system" fn fg_location_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object == OBJID_WINDOW.0 && id_child == 0 && hwnd == GetForegroundWindow() {
        schedule_fullscreen_check();
    }
}

/// Move the location hook to the thread of the new foreground window
/// (nothing for our own windows).
pub(super) unsafe fn follow_foreground(fg: HWND) {
    let old = FG_LOCATION_HOOK.swap(0, Ordering::SeqCst);
    if old != 0 {
        let _ = UnhookWinEvent(HWINEVENTHOOK(old as *mut _));
    }
    FG_LOCATION_TID.store(0, Ordering::SeqCst);
    if fg.is_invalid() {
        return;
    }
    let mut pid = 0u32;
    let tid = GetWindowThreadProcessId(fg, Some(&mut pid));
    if tid == 0 || pid == std::process::id() {
        return;
    }
    let h = SetWinEventHook(
        EVENT_OBJECT_LOCATIONCHANGE,
        EVENT_OBJECT_LOCATIONCHANGE,
        None,
        Some(fg_location_event),
        pid,
        tid,
        WINEVENT_OUTOFCONTEXT,
    );
    if h.is_invalid() {
        // Not watched: the next check tries again (refollow_foreground).
        crate::log!("fullscreen: cannot watch the window in front (thread {tid})");
        return;
    }
    FG_LOCATION_HOOK.store(h.0 as isize, Ordering::SeqCst);
    FG_LOCATION_TID.store(tid, Ordering::SeqCst);
}

/// The location hook watches the thread of the window in front now. It
/// can miss a change of window without a foreground event: a hung app's
/// ghost (owned by dwm) is in front while it hangs and the app's own window
/// takes its place again silently.
pub(super) unsafe fn refollow_foreground() {
    let fg = GetForegroundWindow();
    let mut pid = 0u32;
    let tid = if fg.is_invalid() { 0 } else { GetWindowThreadProcessId(fg, Some(&mut pid)) };
    let ours = tid == 0 || pid == std::process::id();
    if must_refollow(ours, tid, FG_LOCATION_TID.load(Ordering::SeqCst), FG_LOCATION_HOOK.load(Ordering::SeqCst) != 0) {
        follow_foreground(fg);
    }
}

/// The location hook has to move: it watches a thread that is not the
/// front window's (or anything while one of ours is in front), or a hook
/// for the right thread could not be set.
fn must_refollow(ours: bool, front: u32, watching: u32, hooked: bool) -> bool {
    if ours {
        watching != 0 || hooked
    } else {
        front != watching || !hooked
    }
}

/// Coalesced fullscreen re-check (one-shot timer).
pub(super) fn schedule_fullscreen_check() {
    if FULLSCREEN_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let h = HWND(DOCK_HWND.load(Ordering::SeqCst) as *mut _);
    unsafe {
        SetTimer(Some(h), FULLSCREEN_TIMER, FULLSCREEN_DEBOUNCE_MS, None);
    }
}

/// Coalesces bursts of window events into one refresh 80 ms later
/// (one-shot timer; nothing runs while the desktop is idle).
pub(super) fn schedule_refresh() {
    if REFRESH_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let h = HWND(DOCK_HWND.load(Ordering::SeqCst) as *mut _);
    unsafe {
        SetTimer(Some(h), REFRESH_TIMER, REFRESH_DEBOUNCE_MS, None);
    }
}

/// explorer showed one of its taskbar windows: hide it again.
pub(super) unsafe extern "system" fn tray_show_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW.0 || !HIDE_TASKBAR.load(Ordering::SeqCst) {
        return;
    }
    let mut buf = [0u16; 32];
    let n = GetClassNameW(hwnd, &mut buf).max(0) as usize;
    let class = String::from_utf16_lossy(&buf[..n]);
    if class == "Shell_TrayWnd" || class == "Shell_SecondaryTrayWnd" {
        taskbar::rehide();
    }
}

/// (Re)attach the EVENT_OBJECT_SHOW hook to the current explorer process.
pub(super) unsafe fn hook_explorer() {
    let old = TRAY_SHOW_HOOK.swap(0, Ordering::SeqCst);
    if old != 0 {
        let _ = UnhookWinEvent(HWINEVENTHOOK(old as *mut _));
    }
    let pid = taskbar::explorer_pid();
    if pid == 0 {
        return;
    }
    let h = SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_SHOW, None, Some(tray_show_event), pid, 0, WINEVENT_OUTOFCONTEXT);
    TRAY_SHOW_HOOK.store(h.0 as isize, Ordering::SeqCst);
}

/// Whether a TaskbarCreated means explorer (re)started and the restart
/// path must run: explorer's own broadcast always does (running it twice
/// for one explorer is harmless); our tray's own one (`own`) only while
/// explorer `pid` is one the path has not run for (`handled`), since
/// explorer's own may come after ours, or not at all.
pub(super) fn restart_due(own: bool, pid: u32, handled: u32) -> bool {
    !own || (pid != 0 && pid != handled)
}

/// The shell forgot our AppBars (explorer restarted): register the top
/// bar's (first: side docks keep below it) and every dock's again.
pub(super) unsafe fn reregister_appbars() {
    crate::topbar::reregister();
    views::for_each(|d| {
        d.appbar = false;
        d.apply_taskbar_mode();
        d.layout();
    });
}

/// Checks a few times over the next seconds that the shell keeps what our
/// AppBars reserve (RESERVE_CHECK_TIMER); restarts the series if running.
pub(super) unsafe fn start_reserve_checks(hwnd: HWND) {
    RESERVE_CHECK_STEP.store(0, Ordering::SeqCst);
    SetTimer(Some(hwnd), RESERVE_CHECK_TIMER, RESERVE_CHECK_STEPS_MS[0], None);
}

/// The watched explorer exited: start looking for its replacement.
pub(super) unsafe fn on_explorer_gone(hwnd: HWND, pid: u32) {
    crate::log!("explorer gone (pid {pid})");
    // (The tray spy was told by the watcher thread itself: a late notice
    // from here could close a spy already rebuilt for the new explorer.)
    // Its successor's taskbar is not re-hidden before the strip is seen
    // free. (A late notice about an older explorer changes nothing.)
    if !taskbar::explorer_gone(pid) && taskbar::hiding_now() {
        crate::log!("explorer gone: pid {pid} is not the explorer whose taskbar we hid; ignored");
    }
    EXPLORER_WAIT_LEFT.store(EXPLORER_WAIT_TRIES, Ordering::SeqCst);
    SetTimer(Some(hwnd), EXPLORER_WAIT_TIMER, 100, None);
}

/// The process-wide WinEvent hooks (audit A18): kept so they are set once
/// and removed at exit, not left implicit.
static HOOKS: std::sync::Mutex<Vec<isize>> = std::sync::Mutex::new(Vec::new());

pub(super) unsafe fn install_hooks() {
    let Ok(mut hooks) = HOOKS.lock() else { return };
    if !hooks.is_empty() {
        return; // never a second set
    }
    // Window create/destroy/activate come from the shell hook (taskbar-
    // worthy windows only); these narrow ranges cover what it does not.
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ];
    for (a, b) in ranges {
        let h = SetWinEventHook(a, b, None, Some(win_event), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS);
        if h.is_invalid() {
            crate::log!("hooks: WinEvent hook {a:#x}..{b:#x} not set");
        } else {
            hooks.push(h.0 as isize);
        }
    }
}

/// Removes the hooks install_hooks set (dock window going away).
pub(super) unsafe fn remove_hooks() {
    let Ok(mut hooks) = HOOKS.lock() else { return };
    for h in hooks.drain(..) {
        let _ = UnhookWinEvent(HWINEVENTHOOK(h as *mut _));
    }
    drop(hooks);
    // The foreground location hook too (same UI thread).
    follow_foreground(HWND::default());
}

#[cfg(test)]
mod follow_tests {
    use super::must_refollow;

    #[test]
    fn the_hook_moves_only_when_it_watches_the_wrong_thread() {
        // Watching the front window's thread: stays.
        assert!(!must_refollow(false, 10, 10, true));
        // Another thread in front (a hung app's ghost went away): moves.
        assert!(must_refollow(false, 10, 20, true));
        // The hook for the right thread could not be set: tried again.
        assert!(must_refollow(false, 10, 0, false));
        assert!(must_refollow(false, 10, 10, false));
        // One of our windows in front: an old hook is dropped, none is set.
        assert!(must_refollow(true, 0, 20, true));
        assert!(!must_refollow(true, 0, 0, false));
    }
}

#[cfg(test)]
mod restart_tests {
    use super::restart_due;

    /// The TaskbarCreated handler's state: the explorer it last ran for,
    /// and how often the restart path ran.
    struct Gate {
        handled: u32,
        runs: Vec<u32>,
    }

    impl Gate {
        fn new(startup_pid: u32) -> Self {
            Gate { handled: startup_pid, runs: Vec::new() }
        }
        /// One TaskbarCreated while explorer `pid` runs.
        fn taskbar_created(&mut self, own: bool, pid: u32) {
            if restart_due(own, pid, self.handled) {
                if pid != 0 {
                    self.handled = pid;
                }
                self.runs.push(pid);
            }
        }
    }

    #[test]
    fn our_own_broadcast_is_not_a_restart() {
        let mut g = Gate::new(100);
        g.taskbar_created(true, 100);
        g.taskbar_created(true, 100);
        assert!(g.runs.is_empty());
    }

    #[test]
    fn explorers_broadcast_after_ours_still_reregisters() {
        // The measured case: explorer back, our tray takes the front back
        // and asks apps (its broadcast first), explorer's own comes after.
        let mut g = Gate::new(100);
        g.taskbar_created(true, 200);
        g.taskbar_created(false, 200);
        // The last run is for explorer's own broadcast: the AppBars are
        // registered once the new shell is ready for them.
        assert_eq!(g.runs, vec![200, 200]);
        // Later asks of ours change nothing.
        g.taskbar_created(true, 200);
        assert_eq!(g.runs.len(), 2);
    }

    #[test]
    fn explorers_broadcast_before_ours_runs_once() {
        let mut g = Gate::new(100);
        g.taskbar_created(false, 200);
        g.taskbar_created(true, 200);
        assert_eq!(g.runs, vec![200]);
    }

    #[test]
    fn a_lost_explorer_broadcast_is_covered_by_ours() {
        let mut g = Gate::new(100);
        g.taskbar_created(true, 200);
        assert_eq!(g.runs, vec![200]);
        // And the next restart again.
        g.taskbar_created(true, 300);
        assert_eq!(g.runs, vec![200, 300]);
    }

    #[test]
    fn our_broadcast_without_an_explorer_waits() {
        // Explorer not found (between restarts, or none at startup): ours
        // does not run the path; explorer's own does when it comes.
        let mut g = Gate::new(0);
        g.taskbar_created(true, 0);
        assert!(g.runs.is_empty());
        g.taskbar_created(false, 0);
        assert_eq!(g.runs, vec![0]);
        // Nothing handled yet: our next ask with explorer up runs it.
        g.taskbar_created(true, 400);
        assert_eq!(g.runs, vec![0, 400]);
    }
}
