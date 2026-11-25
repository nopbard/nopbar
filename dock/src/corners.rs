//! Hot corners (like macOS): resting the pointer in a screen corner for a
//! moment runs that corner's action.
//!
//! Nothing sits in the corners, so no click is ever taken from the Start
//! button or a maximized window's close button. Instead a background
//! thread's message-only window receives raw mouse input (RIDEV_INPUTSINK):
//! every pointer move, anywhere, wakes it to compare the cursor with the
//! corners. No polling, no mouse hook, no timer while the pointer rests
//! elsewhere; raw input is only registered while some corner is set and
//! not every cornered monitor has a fullscreen app in front.
//!
//! The action itself runs on the dock's UI thread (the Control Center
//! lives there), through a small message window of its own.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VIRTUAL_KEY, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON, VK_XBUTTON1, VK_XBUTTON2};
use windows::Win32::UI::Input::{RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_INPUTSINK, RIDEV_REMOVE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::{CornerAction, HotCornersCfg};
use crate::keys::Sent;

/// The pointer must rest this long in the corner.
const DWELL_TIMER: usize = 1;
const DWELL_MS: u32 = 150;
/// Physical px: the square in each corner that counts. A pointer pushed
/// into a corner stops on its very pixel.
const SIZE_PX: i32 = 2;

/// To the sensing thread: the zones in SHARED changed.
const WM_RECONFIG: u32 = WM_APP + 1;
/// To the sensing thread: the action did not run (wParam = generation,
/// lParam = zone), so this visit is not used up.
const WM_NOT_RUN: u32 = WM_APP + 2;
/// To the UI window: run CornerAction::ALL[wParam]; lParam = (generation
/// << 8) | zone, echoed back with WM_NOT_RUN.
const WM_HOT_CORNER: u32 = WM_APP + 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// In the order of HotCornersCfg::all().
pub const CORNERS: [Corner; 4] = [Corner::TopLeft, Corner::TopRight, Corner::BottomLeft, Corner::BottomRight];

/// The `size` x `size` px square in corner `c` of monitor rect `m`.
pub fn corner_rect(c: Corner, m: RECT, size: i32) -> RECT {
    let (left, top) = match c {
        Corner::TopLeft => (m.left, m.top),
        Corner::TopRight => (m.right - size, m.top),
        Corner::BottomLeft => (m.left, m.bottom - size),
        Corner::BottomRight => (m.right - size, m.bottom - size),
    };
    RECT { left, top, right: left + size, bottom: top + size }
}

fn inside(r: RECT, pt: POINT) -> bool {
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Zone {
    /// Its monitor's rect (for the fullscreen check).
    monitor: RECT,
    rect: RECT,
    action: CornerAction,
}

/// What the sensing thread should watch: (generation, zones, paused).
static SHARED: Mutex<(u32, Vec<Zone>, bool)> = Mutex::new((0, Vec::new(), false));
static GENERATION: AtomicU32 = AtomicU32::new(0);
static SENSE_HWND: AtomicIsize = AtomicIsize::new(0);
static UI_HWND: AtomicIsize = AtomicIsize::new(0);
/// An action that pumps messages (the Control Center) is running.
static RUNNING: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// UI thread: the setting the zones were made for.
    static CLICK_WINDOWS: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
    static CFG: Cell<HotCornersCfg> = Cell::new(HotCornersCfg::default());
}

// ---- UI thread ---------------------------------------------------------------

/// The setting changed (or may have).
pub fn apply(cfg: &HotCornersCfg) {
    if UI_HWND.load(Ordering::SeqCst) != 0 && CFG.with(|c| c.get()) == *cfg {
        return;
    }
    CFG.with(|c| c.set(*cfg));
    rebuild();
}

/// Monitors came, went or moved: new corners.
pub fn displays_changed() {
    rebuild();
}

fn rebuild() {
    let cfg = CFG.with(|c| c.get());
    if cfg.desktop_click { ensure_ui_window(); }
    rebuild_click_windows();
    let mut zones = Vec::new();
    if cfg.active() {
        for m in crate::monitors::all() {
            for (corner, action) in CORNERS.into_iter().zip(cfg.all()) {
                if action != CornerAction::None {
                    zones.push(Zone { monitor: m.rect, rect: corner_rect(corner, m.rect, SIZE_PX), action });
                }
            }
        }
        start();
    }
    crate::log!("hot corners: {} zone(s)", zones.len());
    let paused = all_fullscreen(&zones);
    publish(zones, paused);
}

/// Every monitor with a corner has a fullscreen app in front.
fn all_fullscreen(zones: &[Zone]) -> bool {
    !zones.is_empty() && zones.iter().all(|z| crate::dock::fullscreen_on(z.monitor))
}

fn publish(zones: Vec<Zone>, paused: bool) {
    let gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    if let Ok(mut s) = SHARED.lock() {
        *s = (gen, zones, paused);
    }
    let h = SENSE_HWND.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_RECONFIG, WPARAM(0), LPARAM(0));
        }
    }
}

/// A fullscreen app came or went: stop listening to the mouse while every
/// cornered monitor is fullscreen (a game gets no wake-ups from us).
pub fn refresh_fullscreen() {
    rebuild_click_windows();
    let (zones, paused) = match SHARED.lock() {
        Ok(s) => (s.1.clone(), s.2),
        Err(_) => return,
    };
    let now = all_fullscreen(&zones);
    if now != paused {
        publish(zones, now);
    }
}

/// Only the dock UI thread owns the small click targets.
pub fn taskbar_changed() {
    let ui = UI_HWND.load(Ordering::SeqCst);
    if ui != 0 {
        unsafe { let _ = PostMessageW(Some(HWND(ui as *mut _)), WM_RECONFIG, WPARAM(0), LPARAM(0)); }
    }
}

fn exposed_corner(m: RECT, monitors: &[RECT]) -> bool {
    !monitors.iter().any(|&other| {
        [(m.right, m.bottom - 1), (m.right - 1, m.bottom), (m.right, m.bottom)]
            .into_iter().any(|(x, y)| inside(other, POINT { x, y }))
    })
}

fn rebuild_click_windows() {
    let cfg = CFG.with(Cell::get);
    let mut wanted = Vec::new();
    if cfg.desktop_click && !(cfg.enabled && cfg.bottom_right != CornerAction::None) && crate::taskbar::hidden_by_us() {
        let monitors: Vec<_> = crate::monitors::all().into_iter().map(|m| m.rect).collect();
        for &m in &monitors {
            if exposed_corner(m, &monitors) && !crate::dock::fullscreen_on(m) {
                wanted.push(corner_rect(Corner::BottomRight, m, SIZE_PX));
            }
        }
    }
    CLICK_WINDOWS.with(|windows| unsafe {
        let mut windows = windows.borrow_mut();
        windows.retain(|&h| {
            let mut r = RECT::default();
            if GetWindowRect(h, &mut r).is_ok() {
                if let Some(i) = wanted.iter().position(|v| *v == r) {
                    wanted.swap_remove(i);
                    return true;
                }
            }
            let _ = DestroyWindow(h);
            false
        });
        if wanted.is_empty() { return; }
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let name = w!("NopBarShowDesktopCorner");
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(click_proc),
            hInstance: inst.into(), lpszClassName: name, ..Default::default() };
        RegisterClassExW(&wc);
        for r in wanted {
            if let Ok(h) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                name, w!(""), WS_POPUP, r.left, r.top, r.right-r.left, r.bottom-r.top, None, None, Some(inst.into()), None) {
                // A nonzero alpha keeps the corner hit-testable.
                let _ = SetLayeredWindowAttributes(h, Default::default(), 1, LWA_ALPHA);
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                windows.push(h);
            }
        }
    });
}

unsafe extern "system" fn click_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, SetCapture, ReleaseCapture};
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => { SetCapture(h); LRESULT(0) }
        WM_LBUTTONUP if GetCapture() == h => {
            let _ = ReleaseCapture();
            let mut r = RECT::default();
            if crate::taskbar::hidden_by_us() && GetWindowRect(h, &mut r).is_ok() && inside(r, cursor()) {
                run(CornerAction::Desktop);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

/// Click targets need only the UI window.
fn ensure_ui_window() {
    if UI_HWND.load(Ordering::SeqCst) == 0 {
        unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(ui_wndproc),
                hInstance: hinst.into(),
                lpszClassName: w!("NopBarHotCornerUi"),
                ..Default::default()
            };
            RegisterClassExW(&wc);
            if let Ok(h) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("NopBarHotCornerUi"), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None) {
                UI_HWND.store(h.0 as isize, Ordering::SeqCst);
            }
        }
    }
}

/// Raw input is needed only for configured hover actions.
fn start() {
    ensure_ui_window();
    static STARTED: AtomicBool = AtomicBool::new(false);
    if !STARTED.swap(true, Ordering::SeqCst) {
        let ok = std::thread::Builder::new().name("hot-corners".into()).spawn(sense_thread).is_ok();
        if !ok {
            STARTED.store(false, Ordering::SeqCst);
            crate::log!("hot corners: cannot start the sensing thread");
        }
    }
}

/// Runs a corner's action. True if it ran (or is already running).
fn run(action: CornerAction) -> bool {
    let sent = |what: &str, s: Sent| -> bool {
        match s {
            Sent::Ok => true,
            Sent::ModifiersHeld => false,
            Sent::Failed => {
                crate::log!("hot corner: {what}: Windows did not take the shortcut");
                false
            }
        }
    };
    crate::log!("hot corner: {}", action.name());
    match action {
        CornerAction::None => true,
        CornerAction::TaskView => sent("task view", crate::keys::unmixed(&crate::keys::TASK_VIEW)),
        CornerAction::Desktop => sent("desktop", crate::keys::unmixed(&crate::keys::SHOW_DESKTOP)),
        CornerAction::Notifications => sent("notifications", crate::keys::unmixed(&crate::keys::NOTIFICATIONS)),
        CornerAction::NextDesktop => sent("next desktop", crate::vdesk::step(true)),
        CornerAction::PreviousDesktop => sent("previous desktop", crate::vdesk::step(false)),
        CornerAction::ControlCenter => {
            // It pumps messages until closed: never two at once.
            if RUNNING.swap(true, Ordering::SeqCst) {
                return true;
            }
            let ok = crate::topbar::open_control_center()
                // No top bar (turned off): Windows' own quick settings.
                || sent("quick settings", crate::keys::unmixed(&crate::keys::QUICK_SETTINGS));
            RUNNING.store(false, Ordering::SeqCst);
            ok
        }
        CornerAction::Lock => match unsafe { windows::Win32::System::Shutdown::LockWorkStation() } {
            Ok(()) => true,
            Err(e) => {
                crate::log!("hot corner: lock failed: {e}");
                false
            }
        },
        CornerAction::ScreenSaver => {
            // Starts the screen saver set in Windows (nothing if none is).
            let r = unsafe {
                SendMessageTimeoutW(
                    GetDesktopWindow(),
                    WM_SYSCOMMAND,
                    WPARAM(windows::Win32::Graphics::Gdi::SC_SCREENSAVE as usize),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG,
                    1000,
                    None,
                )
            };
            if r.0 == 0 {
                crate::log!("hot corner: screen saver: no answer");
            }
            r.0 != 0
        }
    }
}

unsafe extern "system" fn ui_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_RECONFIG {
        rebuild_click_windows();
        return LRESULT(0);
    }
    if msg == WM_HOT_CORNER {
        let action = CornerAction::ALL.get(wp.0).copied().unwrap_or_default();
        if !run(action) {
            // Not used up: the next move in the corner tries again.
            let gen = (lp.0 as usize >> 8) as u32;
            let zone = lp.0 as usize & 0xFF;
            let h = SENSE_HWND.load(Ordering::SeqCst);
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_NOT_RUN, WPARAM(gen as usize), LPARAM(zone as isize));
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

// ---- Sensing thread ----------------------------------------------------------

/// The sensing thread's view of the pointer.
#[derive(Default)]
struct Sense {
    gen: u32,
    zones: Vec<Zone>,
    registered: bool,
    /// The zone the pointer is in.
    current: Option<usize>,
    /// The action was handed over during this visit: not again until the
    /// pointer leaves the zone.
    fired: bool,
    dwelling: bool,
}

thread_local! {
    static SENSE: RefCell<Sense> = RefCell::new(Sense::default());
}

fn sense_thread() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(sense_wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("NopBarHotCornerSense"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(h) = CreateWindowExW(WINDOW_EX_STYLE(0), w!("NopBarHotCornerSense"), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None) else {
            crate::log!("hot corners: cannot create the sensing window");
            return;
        };
        SENSE_HWND.store(h.0 as isize, Ordering::SeqCst);
        // Zones published before the window existed.
        let _ = PostMessageW(Some(h), WM_RECONFIG, WPARAM(0), LPARAM(0));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}

fn button_held() -> bool {
    let down = |k: VIRTUAL_KEY| unsafe { GetAsyncKeyState(k.0 as i32) } < 0;
    [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2].into_iter().any(down)
}

fn cursor() -> POINT {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    pt
}

unsafe fn listen(hwnd: HWND, on: bool) -> bool {
    let rid = RAWINPUTDEVICE {
        usUsagePage: 0x01, // generic desktop
        usUsage: 0x02,     // mouse
        dwFlags: if on { RIDEV_INPUTSINK } else { RIDEV_REMOVE },
        hwndTarget: if on { hwnd } else { HWND::default() },
    };
    match RegisterRawInputDevices(&[rid], std::mem::size_of::<RAWINPUTDEVICE>() as u32) {
        Ok(()) => true,
        Err(e) => {
            crate::log!("hot corners: raw input {}: {e}", if on { "on" } else { "off" });
            false
        }
    }
}

/// What a pointer event means for the visit state (pure, for the tests):
/// returns (new current zone, new fired, start the dwell timer).
fn step(current: Option<usize>, fired: bool, dwelling: bool, now: Option<usize>) -> (Option<usize>, bool, bool) {
    if now != current {
        // Left (or moved to another corner): the visit is over.
        return (now, false, now.is_some());
    }
    // Still in the same corner: a visit that did not fire yet (modifiers
    // were held, or the action failed) gets another rest.
    (now, fired, now.is_some() && !fired && !dwelling)
}

/// Any raw mouse event: where is the pointer now?
unsafe fn on_pointer(hwnd: HWND) {
    let pt = cursor();
    SENSE.with(|s| {
        let mut s = s.borrow_mut();
        let now = s.zones.iter().position(|z| inside(z.rect, pt));
        if now != s.current && s.dwelling {
            let _ = KillTimer(Some(hwnd), DWELL_TIMER);
            s.dwelling = false;
        }
        let (cur, fired, dwell) = step(s.current, s.fired, s.dwelling, now);
        s.current = cur;
        s.fired = fired;
        if dwell {
            SetTimer(Some(hwnd), DWELL_TIMER, DWELL_MS, None);
            s.dwelling = true;
        }
    });
}

unsafe fn on_dwell(hwnd: HWND) {
    let _ = KillTimer(Some(hwnd), DWELL_TIMER);
    let pt = cursor();
    let hand_over = SENSE.with(|s| {
        let mut s = s.borrow_mut();
        s.dwelling = false;
        let i = s.current?;
        let z = *s.zones.get(i)?;
        if s.fired || !inside(z.rect, pt) {
            return None;
        }
        if button_held() || crate::dock::fullscreen_on(z.monitor) {
            // A drag that ends in the corner, or a fullscreen app: this
            // visit is used up.
            s.fired = true;
            return None;
        }
        if crate::keys::modifiers_held() {
            // Not used up: the next move in the corner tries again.
            return None;
        }
        s.fired = true;
        Some((z.action, s.gen, i))
    });
    if let Some((action, gen, i)) = hand_over {
        let idx = CornerAction::ALL.iter().position(|a| *a == action).unwrap_or(0);
        let lp = ((gen as usize) << 8 | (i & 0xFF)) as isize;
        let ui = UI_HWND.load(Ordering::SeqCst);
        let _ = PostMessageW(Some(HWND(ui as *mut _)), WM_HOT_CORNER, WPARAM(idx), LPARAM(lp));
    }
}

unsafe extern "system" fn sense_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            on_pointer(hwnd);
            // Lets Windows free the raw input data.
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER if wp.0 == DWELL_TIMER => {
            on_dwell(hwnd);
            LRESULT(0)
        }
        WM_RECONFIG => {
            let (gen, zones, paused) = SHARED.lock().map(|s| s.clone()).unwrap_or_default();
            let _ = KillTimer(Some(hwnd), DWELL_TIMER);
            let want = !zones.is_empty() && !paused;
            SENSE.with(|s| {
                let mut s = s.borrow_mut();
                let registered = s.registered;
                *s = Sense { gen, zones, registered, ..Default::default() };
                if want != registered && listen(hwnd, want) {
                    s.registered = want;
                }
            });
            LRESULT(0)
        }
        WM_NOT_RUN => {
            SENSE.with(|s| {
                let mut s = s.borrow_mut();
                if s.gen == wp.0 as u32 && s.current == Some(lp.0 as usize) {
                    s.fired = false;
                }
            });
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_click_skips_touching_edges_and_diagonals() {
        let m = RECT { left: -100, top: -100, right: 0, bottom: 0 };
        assert!(exposed_corner(m, &[m]));
        for (left, top) in [(0, -100), (-100, 0), (0, 0)] {
            assert!(!exposed_corner(m, &[m, RECT { left, top, right: left+100, bottom: top+100 }]));
        }
    }

    #[test]
    fn corner_rects_sit_inside_the_monitor() {
        let m = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        assert_eq!(corner_rect(Corner::TopLeft, m, 1), RECT { left: -1920, top: 0, right: -1919, bottom: 1 });
        assert_eq!(corner_rect(Corner::TopRight, m, 1), RECT { left: -1, top: 0, right: 0, bottom: 1 });
        assert_eq!(corner_rect(Corner::BottomLeft, m, 2), RECT { left: -1920, top: 1078, right: -1918, bottom: 1080 });
        assert_eq!(corner_rect(Corner::BottomRight, m, 2), RECT { left: -2, top: 1078, right: 0, bottom: 1080 });
        // The pixel a pointer pushed into the corner stops on is inside.
        assert!(inside(corner_rect(Corner::BottomRight, m, 1), POINT { x: -1, y: 1079 }));
        assert!(!inside(corner_rect(Corner::BottomRight, m, 1), POINT { x: 0, y: 1079 }));
    }

    #[test]
    fn visits_fire_once_and_rearm_on_any_exit() {
        // Enter: dwell starts.
        assert_eq!(step(None, false, false, Some(0)), (Some(0), false, true));
        // Moving inside while dwelling: nothing new.
        assert_eq!(step(Some(0), false, true, Some(0)), (Some(0), false, false));
        // Fired: moving inside does not start another.
        assert_eq!(step(Some(0), true, false, Some(0)), (Some(0), true, false));
        // Any event outside, however short the trip, ends the visit...
        assert_eq!(step(Some(0), true, false, None), (None, false, false));
        // ...so coming back starts a new one.
        assert_eq!(step(None, false, false, Some(0)), (Some(0), false, true));
        // Straight into another corner (shared monitor edge): new visit.
        assert_eq!(step(Some(0), true, false, Some(5)), (Some(5), false, true));
        // Not fired (modifiers held, or the action failed): next move retries.
        assert_eq!(step(Some(0), false, false, Some(0)), (Some(0), false, true));
    }
}
