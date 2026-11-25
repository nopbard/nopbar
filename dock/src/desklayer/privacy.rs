//! Widgets kept out of screen sharing and captures (Zoom, Teams, Snipping
//! Tool): SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) on every widget
//! window (Windows 10 2004 and later; refused elsewhere, which is ignored).
//! The user sees them as always. One switch (Settings > widgets, on by
//! default) turns it off for every widget; the snip overlay stays out of
//! captures whatever it says.
//!
//! Windows register themselves when they are made (or, for windows another
//! module makes, the first time desklayer::win places them), so a change
//! of the switch reaches every one at once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::HWND;

/// Kept out of captures (the switch). On by default.
static HIDE: AtomicBool = AtomicBool::new(true);
/// Every widget window seen: (window, always hidden whatever the switch).
static WINDOWS: Mutex<Vec<(isize, bool)>> = Mutex::new(Vec::new());

/// What to ask of a window: kept out of captures, or shown in them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity {
    Exclude,
    Normal,
}

/// Pure: the affinity of a widget (`always`: the snip overlay) with the
/// switch `hide`.
pub fn affinity(hide: bool, always: bool) -> Affinity {
    if hide || always {
        Affinity::Exclude
    } else {
        Affinity::Normal
    }
}

/// Sets a window's affinity (the real one calls Windows; tests inject).
pub type Setter = fn(isize, Affinity) -> bool;

fn real_set(hwnd: isize, a: Affinity) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
    let v = match a {
        Affinity::Exclude => WDA_EXCLUDEFROMCAPTURE,
        Affinity::Normal => WDA_NONE,
    };
    // Older Windows refuse WDA_EXCLUDEFROMCAPTURE: nothing more to do.
    unsafe { SetWindowDisplayAffinity(HWND(hwnd as *mut _), v) }.is_ok()
}

fn alive(hwnd: isize) -> bool {
    unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(hwnd as *mut _))) }.as_bool()
}

fn list() -> std::sync::MutexGuard<'static, Vec<(isize, bool)>> {
    WINDOWS.lock().unwrap_or_else(|e| e.into_inner())
}

/// A widget window was made (`always`: kept out whatever the switch).
pub fn register(hwnd: HWND, always: bool) {
    register_with(hwnd.0 as isize, always, real_set);
}

/// register() with the setter given (tests).
pub fn register_with(hwnd: isize, always: bool, set: Setter) {
    {
        let mut l = list();
        match l.iter_mut().find(|(h, _)| *h == hwnd) {
            Some(e) => e.1 |= always,
            None => l.push((hwnd, always)),
        }
    }
    let always = list().iter().find(|(h, _)| *h == hwnd).is_some_and(|e| e.1);
    let a = affinity(HIDE.load(Ordering::SeqCst), always);
    if !set(hwnd, a) && a == Affinity::Exclude {
        crate::log!("widgets: Windows did not keep a widget out of captures (older Windows?)");
    }
}

/// A window desklayer places (notes made elsewhere): registered once.
pub fn seen(hwnd: HWND) {
    let h = hwnd.0 as isize;
    if !list().iter().any(|(w, _)| *w == h) {
        register(hwnd, false);
    }
}

/// Window `hwnd` is kept out of captures with the widgets (tests).
#[cfg(test)]
pub fn registered(hwnd: HWND) -> bool {
    list().iter().any(|(w, _)| *w == hwnd.0 as isize)
}

/// The window is going away.
pub fn forget(hwnd: HWND) {
    let h = hwnd.0 as isize;
    list().retain(|(w, _)| *w != h);
}

/// The switch (config widgets.hide_from_capture) changed: every widget
/// window takes it now.
pub fn set_hidden(hide: bool) {
    set_hidden_with(hide, real_set, alive);
}

/// set_hidden() with the setter and the "still a window" check given.
pub fn set_hidden_with(hide: bool, set: Setter, alive: fn(isize) -> bool) {
    HIDE.store(hide, Ordering::SeqCst);
    let now: Vec<(isize, bool)> = {
        let mut l = list();
        l.retain(|(h, _)| alive(*h));
        l.clone()
    };
    let refused = now.iter().filter(|(h, always)| !set(*h, affinity(hide, *always))).count();
    if refused > 0 {
        crate::log!("widgets: {refused} widget window(s) did not take the capture setting");
    }
}

pub fn hidden() -> bool {
    HIDE.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as M;

    static CALLS: M<Vec<(isize, Affinity)>> = M::new(Vec::new());
    /// The tests share the switch and the list: one at a time.
    static ONE: M<()> = M::new(());

    fn spy(h: isize, a: Affinity) -> bool {
        CALLS.lock().unwrap().push((h, a));
        true
    }

    fn calls_for(range: std::ops::Range<isize>) -> Vec<(isize, Affinity)> {
        CALLS.lock().unwrap().iter().copied().filter(|(h, _)| range.contains(h)).collect()
    }

    #[test]
    fn always_ones_stay_out_of_captures() {
        assert_eq!(affinity(true, false), Affinity::Exclude);
        assert_eq!(affinity(false, false), Affinity::Normal);
        assert_eq!(affinity(false, true), Affinity::Exclude);
        assert_eq!(affinity(true, true), Affinity::Exclude);
    }

    #[test]
    fn made_windows_take_the_switch_and_a_change_reaches_all() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        set_hidden_with(true, spy, |_| true);
        // Made: kept out at once.
        register_with(9001, false, spy);
        register_with(9002, true, spy);
        assert_eq!(calls_for(9001..9003), vec![(9001, Affinity::Exclude), (9002, Affinity::Exclude)]);
        CALLS.lock().unwrap().clear();
        // Switched off: every widget shows in captures but the always ones.
        set_hidden_with(false, spy, |h| h != 9003);
        let mut got = calls_for(9001..9003);
        got.sort_by_key(|c| c.0);
        assert_eq!(got, vec![(9001, Affinity::Normal), (9002, Affinity::Exclude)]);
        // Made while off: shown in captures.
        CALLS.lock().unwrap().clear();
        register_with(9003, false, spy);
        assert_eq!(calls_for(9003..9004), vec![(9003, Affinity::Normal)]);
        // A window gone is dropped from the list on the next change.
        CALLS.lock().unwrap().clear();
        set_hidden_with(true, spy, |h| h != 9003);
        assert!(calls_for(9003..9004).is_empty());
        assert_eq!(calls_for(9001..9002), vec![(9001, Affinity::Exclude)]);
        forget(HWND(9001 as *mut _));
        forget(HWND(9002 as *mut _));
        set_hidden_with(true, |_, _| true, |_| true);
    }
}
