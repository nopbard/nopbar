//! "Dock follows the mouse" across monitors without polling: a 1 px
//! invisible sensor window sits where the dock would be on every other
//! monitor. Resting the pointer there for 120 ms moves the dock over
//! (polish plan §4: deliberate edge entry only).
//!
//! The sensor never takes a click meant for the window under it. The
//! pointer's first move onto it is all it needs: from then on it is
//! click-through (WS_EX_TRANSPARENT on a layered window takes it out of
//! Windows' hit-testing for every process, so presses and moves go to
//! whatever is below; HTTRANSPARENT would only pass them to windows of
//! our own thread). The dwell timer then reads the pointer itself: still
//! there with no button held, the dock moves; a button held (someone is
//! using that edge), it stays click-through until the pointer has left;
//! gone, it becomes a sensor again for the next arrival.

use super::*;

/// Posted to the dock: wParam = HMONITOR to move to.
pub(super) const WM_MOVE_TO_MONITOR: u32 = WM_APP + 60;
const DWELL_TIMER: usize = 1;
const DWELL_MS: u32 = 120;
/// 1 px: as little of the other monitor's edge as possible.
const THICK_PX: i32 = 1;
/// Click-through after the dwell (a button was held, or the dock was
/// asked over): look again this often whether the pointer has left; a
/// click that still landed on a sensor hides it this long.
const YIELD_TIMER: usize = 2;
const YIELD_MS: u32 = 1500;
const RECHECK_MS: u32 = 500;

/// What a sensor does when its dwell is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dwell {
    /// The pointer rests on it: the dock comes over.
    Move,
    /// A mouse button is held there: someone is using that edge. Stay
    /// click-through until the pointer leaves.
    Yield,
    /// The pointer moved on: a sensor again.
    Rearm,
}

/// Pure: the dwell is over, the pointer is `inside` the strip (or not),
/// a mouse button is held (or not).
pub(super) fn after_dwell(inside: bool, button_down: bool) -> Dwell {
    match (inside, button_down) {
        (false, _) => Dwell::Rearm,
        (true, true) => Dwell::Yield,
        (true, false) => Dwell::Move,
    }
}

/// Clicks (and moves) go through the sensor to the window below (`on`),
/// or reach it again.
unsafe fn pass_through(h: HWND, on: bool) {
    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
    let bit = WS_EX_TRANSPARENT.0 as isize;
    let want = if on { ex | bit } else { ex & !bit };
    if want != ex {
        SetWindowLongPtrW(h, GWL_EXSTYLE, want);
    }
}

/// The pointer is on sensor `hwnd`'s strip.
fn pointer_on(hwnd: HWND) -> bool {
    let mut pt = POINT::default();
    let _ = unsafe { GetCursorPos(&mut pt) };
    SENSORS.with(|s| s.borrow().iter().any(|(h, _, r)| *h == hwnd && pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom))
}

pub(super) fn button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
    [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON].into_iter().any(|k| unsafe { GetAsyncKeyState(k.0 as i32) } < 0)
}

thread_local! {
    /// (sensor window, its monitor, its screen rect)
    static SENSORS: RefCell<Vec<(HWND, isize, RECT)>> = const { RefCell::new(Vec::new()) };
    /// Sensors whose dwell is running.
    static ARMED: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
    /// Sensors hidden because a fullscreen app covers their monitor.
    static COVERED: RefCell<Vec<isize>> = const { RefCell::new(Vec::new()) };
}

/// A fullscreen app came to the front of a sensor's monitor (or left it):
/// the sensor goes (comes back). Nothing of ours stays over a game, not
/// even a 1 px strip; a sensor made later skips such a monitor (rebuild).
pub(super) unsafe fn refresh_fullscreen() {
    let list: Vec<(HWND, isize, RECT)> = SENSORS.with(|s| s.borrow().clone());
    for (h, mon, _) in list {
        let rect = crate::monitors::info(mon).map(|m| m.rect).unwrap_or_default();
        let full = crate::dock::fullscreen_on_monitor(mon, rect);
        let me = h.0 as isize;
        let was = COVERED.with(|c| c.borrow().contains(&me));
        if full == was {
            continue;
        }
        if full {
            COVERED.with(|c| c.borrow_mut().push(me));
            let _ = KillTimer(Some(h), DWELL_TIMER);
            let _ = KillTimer(Some(h), YIELD_TIMER);
            ARMED.with(|a| a.borrow_mut().retain(|&x| x != me));
            pass_through(h, false);
            let _ = ShowWindow(h, SW_HIDE);
        } else {
            COVERED.with(|c| c.borrow_mut().retain(|&x| x != me));
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            // Back on top (it was left alone while hidden).
            let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}

fn covered(h: HWND) -> bool {
    COVERED.with(|c| c.borrow().contains(&(h.0 as isize)))
}

unsafe fn register_class() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarSensor"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassExW(&wc);
}

pub(super) fn clear() {
    COVERED.with(|c| c.borrow_mut().clear());
    ARMED.with(|a| a.borrow_mut().clear());
    let old: Vec<(HWND, isize, RECT)> = SENSORS.with(|s| s.borrow_mut().drain(..).collect());
    for (h, ..) in old {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

/// Puts sensors on every monitor but `current`, along `pos`, `len_dip`
/// long (scaled to each monitor). Nothing when following is off or there
/// is only one monitor.
pub(super) unsafe fn rebuild(follow: bool, pos: Position, hide_taskbar: bool, current: isize, len_dip: f32) {
    clear();
    let monitors = crate::monitors::all();
    if !follow || monitors.len() < 2 {
        return;
    }
    register_class();
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    for m in monitors.iter().filter(|m| m.handle != current) {
        // Nothing over a fullscreen app: made hidden, shown once it goes
        // (refresh_fullscreen).
        let full = crate::dock::fullscreen_on_monitor(m.handle, m.rect);
        // Where the dock would sit there: clear of a kept taskbar, and of
        // a top bar at the bottom (the sensor goes right above it, where
        // the dock's edge would be; never on the bar's own cells).
        let bars = if hide_taskbar { Vec::new() } else { crate::taskbar::bars_on(m.handle) };
        let bar = crate::topbar::space_for(m.rect);
        let area = crate::placement::dock_area(m.rect, m.work, &bars, hide_taskbar, crate::placement::BarSpace { bottom: bar.bottom, ..Default::default() });
        let r = edge::sensor_rect(pos, area, (len_dip * m.scale()).round() as i32, THICK_PX);
        let Ok(h) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("NopBarSensor"),
            w!(""),
            WS_POPUP,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            continue;
        };
        // Nearly transparent: invisible, but still gets the mouse.
        let _ = SetLayeredWindowAttributes(h, COLORREF(0), 1, LWA_ALPHA);
        if full {
            COVERED.with(|c| c.borrow_mut().push(h.0 as isize));
        } else {
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
        }
        SENSORS.with(|s| s.borrow_mut().push((h, m.handle, r)));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let me = hwnd.0 as isize;
    match msg {
        WM_MOUSEMOVE => {
            // The pointer arrived: click-through at once (the next press
            // goes to the window below), the dwell decides the rest.
            if !ARMED.with(|a| a.borrow().contains(&me)) {
                ARMED.with(|a| a.borrow_mut().push(me));
                pass_through(hwnd, true);
                let _ = KillTimer(Some(hwnd), YIELD_TIMER);
                SetTimer(Some(hwnd), DWELL_TIMER, DWELL_MS, None);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            // A press that still landed here (before click-through took):
            // get out of the way for a moment.
            let _ = KillTimer(Some(hwnd), DWELL_TIMER);
            ARMED.with(|a| a.borrow_mut().retain(|&h| h != me));
            let _ = ShowWindow(hwnd, SW_HIDE);
            SetTimer(Some(hwnd), YIELD_TIMER, YIELD_MS, None);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == YIELD_TIMER => {
            if covered(hwnd) {
                // A fullscreen app came meanwhile: stays hidden.
                let _ = KillTimer(Some(hwnd), YIELD_TIMER);
            } else if pointer_on(hwnd) && IsWindowVisible(hwnd).as_bool() {
                // Still there: stay out of the way until it leaves.
                SetTimer(Some(hwnd), YIELD_TIMER, RECHECK_MS, None);
            } else {
                let _ = KillTimer(Some(hwnd), YIELD_TIMER);
                pass_through(hwnd, false);
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == DWELL_TIMER => {
            let _ = KillTimer(Some(hwnd), DWELL_TIMER);
            ARMED.with(|a| a.borrow_mut().retain(|&h| h != me));
            match after_dwell(pointer_on(hwnd), button_down()) {
                Dwell::Move => {
                    let mon = SENSORS.with(|s| s.borrow().iter().find(|(h, ..)| *h == hwnd).map(|(_, m, _)| *m));
                    if let Some(mon) = mon {
                        let dock = HWND(DOCK_HWND.load(Ordering::SeqCst) as *mut _);
                        let _ = PostMessageW(Some(dock), WM_MOVE_TO_MONITOR, WPARAM(mon as usize), LPARAM(0));
                    }
                    // The move rebuilds the sensors; should the dock stay,
                    // this one waits for the pointer to leave.
                    SetTimer(Some(hwnd), YIELD_TIMER, RECHECK_MS, None);
                }
                Dwell::Yield => {
                    SetTimer(Some(hwnd), YIELD_TIMER, RECHECK_MS, None);
                }
                Dwell::Rearm => pass_through(hwnd, false),
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sensor_moves_the_dock_only_for_a_resting_pointer() {
        assert_eq!(after_dwell(true, false), Dwell::Move);
        assert_eq!(after_dwell(true, true), Dwell::Yield, "a button held there: someone is using that edge");
        assert_eq!(after_dwell(false, false), Dwell::Rearm);
        assert_eq!(after_dwell(false, true), Dwell::Rearm, "gone (dragging elsewhere)");
    }
}
