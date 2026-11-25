//! The Win32 side of the desktop layer (see the module notes in
//! desklayer/mod.rs): the window hosting the desktop icons, putting a
//! widget on the desktop or in front, the WM_WINDOWPOSCHANGING pin and the
//! foreground hook. Everything here runs on the widgets' own thread.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetClassNameW, GetForegroundWindow, GetWindow, PostMessageW, RegisterWindowMessageW, SetWindowPos,
    EVENT_SYSTEM_FOREGROUND, GW_HWNDPREV, HWND_BOTTOM, HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, SWP_HIDEWINDOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, WINDOWPOS, WINEVENT_OUTOFCONTEXT,
};

use super::layer::Layer;

thread_local! {
    /// Our own placement is under way: the pin lets it through.
    static PLACING: Cell<bool> = const { Cell::new(false) };
    /// The desktop window the widgets sit above (shell_host(), kept).
    static HOST: Cell<isize> = const { Cell::new(0) };
}

/// Runs `f` with the pin open (our own z-order changes).
pub fn placing<R>(f: impl FnOnce() -> R) -> R {
    let before = PLACING.with(|p| p.replace(true));
    let r = f();
    PLACING.with(|p| p.set(before));
    r
}

pub fn is_placing() -> bool {
    PLACING.with(|p| p.get())
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// Pure: is a window of class `class` part of the desktop shell (the
/// desktop itself, or a WorkerW of it)?
pub fn is_shell_class(class: &str) -> bool {
    class == "Progman" || class == "WorkerW"
}

/// Is `hwnd` the desktop (what is in front during Show desktop)?
pub fn is_shell(hwnd: HWND) -> bool {
    !hwnd.is_invalid() && is_shell_class(&class_of(hwnd))
}

/// A candidate host: (window, is Progman, holds SHELLDLL_DefView).
pub type Candidate = (isize, bool, bool);

/// Pure: the window hosting the desktop icons among `cands`: the first
/// one holding SHELLDLL_DefView (Progman on 24H2 and with a still
/// wallpaper; a WorkerW on older builds with a slideshow), else Progman.
pub fn pick_host(cands: &[Candidate]) -> Option<isize> {
    cands.iter().find(|c| c.2).or_else(|| cands.iter().find(|c| c.1)).map(|c| c.0)
}

fn has_defview(h: HWND) -> bool {
    unsafe { FindWindowExW(Some(h), None, w!("SHELLDLL_DefView"), PCWSTR::null()).is_ok() }
}

/// The window hosting the desktop icons now (None: no Explorer shell).
pub fn shell_host() -> Option<HWND> {
    unsafe {
        let mut cands: Vec<Candidate> = Vec::new();
        if let Ok(p) = FindWindowW(w!("Progman"), PCWSTR::null()) {
            cands.push((p.0 as isize, true, has_defview(p)));
        }
        if !cands.iter().any(|c| c.2) {
            let mut after: Option<HWND> = None;
            // At most a handful: guard against a list changing under us.
            for _ in 0..64 {
                let Ok(wk) = FindWindowExW(None, after, w!("WorkerW"), PCWSTR::null()) else { break };
                cands.push((wk.0 as isize, false, has_defview(wk)));
                after = Some(wk);
            }
        }
        let host = pick_host(&cands).map(|h| HWND(h as *mut _));
        HOST.with(|c| c.set(host.map(|h| h.0 as isize).unwrap_or(0)));
        host
    }
}

/// The desktop window found last (no lookup).
fn cached_host() -> Option<HWND> {
    let h = HOST.with(|c| c.get());
    (h != 0).then_some(HWND(h as *mut _))
}

/// Where `me` goes in the z-order to sit just above the desktop `host`:
/// after the window right above the host (so between it and the host).
/// None: already there. With no host: the bottom.
fn desk_slot(me: HWND, host: Option<HWND>) -> Option<HWND> {
    let Some(host) = host else { return Some(HWND_BOTTOM) };
    match unsafe { GetWindow(host, GW_HWNDPREV) } {
        Ok(prev) if prev == me => None,
        Ok(prev) if !prev.is_invalid() => Some(prev),
        // The desktop is the top window (Show desktop): right above it.
        _ => Some(HWND_TOP),
    }
}

/// Puts `hwnd` on the desktop layer: just above `host` (shell_host()) in
/// the z-order, not owned by it.
pub fn to_desktop(hwnd: HWND, host: Option<HWND>) {
    super::privacy::seen(hwnd);
    placing(|| unsafe {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
        if let Some(slot) = desk_slot(hwnd, host) {
            let _ = SetWindowPos(hwnd, Some(slot), 0, 0, 0, 0, flags);
        }
    });
}

/// Brings `hwnd` in front of everything (topmost). The caller activates
/// the one to type in.
pub fn to_front(hwnd: HWND) {
    super::privacy::seen(hwnd);
    placing(|| unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    });
}

/// Pure: what the pin does to a z-order change on the desktop layer:
/// `to_top` (it asks for the top / topmost), `show_desktop` (the desktop
/// is in front). Some(true): let it be; Some(false): no z change; None:
/// put it just above the desktop instead.
pub fn pin_rule(to_top: bool, show_desktop: bool) -> Option<bool> {
    if !to_top || show_desktop {
        Some(true)
    } else {
        None
    }
}

/// WM_WINDOWPOSCHANGING of a widget on `layer`: on the desktop layer an
/// activation's "to the top" becomes "just above the desktop" and a hide
/// (Show desktop) is refused. Our own placement passes.
pub fn pin(layer: Layer, hwnd: HWND, wp: &mut WINDOWPOS) {
    if layer != Layer::Desktop || is_placing() {
        return;
    }
    wp.flags &= !SWP_HIDEWINDOW;
    if wp.flags.contains(SWP_NOZORDER) {
        return;
    }
    let after = wp.hwndInsertAfter;
    let to_top = after == HWND_TOP || after == HWND_TOPMOST;
    let show_desktop = is_shell(unsafe { GetForegroundWindow() });
    match pin_rule(to_top, show_desktop) {
        Some(true) => {}
        Some(false) => wp.flags |= SWP_NOZORDER,
        None => match desk_slot(hwnd, cached_host()) {
            Some(slot) if slot != HWND_TOP => wp.hwndInsertAfter = slot,
            _ => wp.flags |= SWP_NOZORDER,
        },
    }
}

/// Explorer's "I (re)started" broadcast.
pub fn taskbar_created() -> u32 {
    static MSG: AtomicU32 = AtomicU32::new(0);
    let m = MSG.load(Ordering::Relaxed);
    if m != 0 {
        return m;
    }
    let m = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    MSG.store(m, Ordering::Relaxed);
    m
}

thread_local! {
    /// This thread's foreground watcher: (window, message). A WinEvent
    /// hook made out of context calls back on the thread that set it, so
    /// each widget thread (the notes', the dock's) has its own.
    static FG: Cell<(isize, u32)> = const { Cell::new((0, 0)) };
}

unsafe extern "system" fn on_foreground(_h: HWINEVENTHOOK, _ev: u32, hwnd: HWND, _obj: i32, _child: i32, _thread: u32, _time: u32) {
    let (target, msg) = FG.with(|f| f.get());
    if target != 0 {
        let _ = PostMessageW(Some(HWND(target as *mut _)), msg, WPARAM(hwnd.0 as usize), LPARAM(0));
    }
}

/// Posts `msg` (wParam: the new foreground window) to `target` whenever
/// another process's window comes to the front: a WinEvent, nothing runs
/// in between. One watcher per thread; returns the hook (0: failed).
pub fn watch_foreground(target: HWND, msg: u32) -> isize {
    FG.with(|f| f.set((target.0 as isize, msg)));
    let h = unsafe {
        SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(on_foreground), 0, 0, WINEVENT_OUTOFCONTEXT)
    };
    h.0 as isize
}

pub fn unwatch_foreground(hook: isize) {
    if hook != 0 {
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(hook as *mut _));
        }
    }
    FG.with(|f| f.set((0, 0)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icons_host_is_found_on_24h2_and_before() {
        // 24H2: Progman holds the icons.
        assert_eq!(pick_host(&[(1, true, true), (2, false, false)]), Some(1));
        // Older builds with a slideshow: a WorkerW holds them.
        assert_eq!(pick_host(&[(1, true, false), (2, false, false), (3, false, true)]), Some(3));
        // Icons hidden or not found: Progman still is the desktop.
        assert_eq!(pick_host(&[(1, true, false), (2, false, false)]), Some(1));
        assert_eq!(pick_host(&[]), None);
    }

    #[test]
    fn shell_classes() {
        assert!(is_shell_class("Progman") && is_shell_class("WorkerW"));
        assert!(!is_shell_class("Shell_TrayWnd") && !is_shell_class("progman "));
    }

    #[test]
    fn the_pin_keeps_activation_on_the_desktop_but_follows_show_desktop() {
        // A click asks for the top: stays just above the desktop.
        assert_eq!(pin_rule(true, false), None);
        // Show desktop raises the desktop: the widget may come with it.
        assert_eq!(pin_rule(true, true), Some(true));
        // Anything else (a move to a given place) goes through.
        assert_eq!(pin_rule(false, false), Some(true));
    }
}
