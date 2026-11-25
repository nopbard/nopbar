//! The dock thread's hidden window for what the desktop widgets there
//! share: the foreground hook (a piece just cut goes to the desktop, Show
//! desktop brings the pieces back over it), displays changing and Explorer
//! restarting (crate::pieces), the desktop's Ctrl+V's posted work
//! (deskpaste), the desktop folders' watch that keeps the icon grid the
//! notes snap to fresh (icons), and the last try at stray widget files
//! at quit (disk). Never shown; a top-level window (not
//! message-only) so the broadcasts (WM_DISPLAYCHANGE, TaskbarCreated)
//! reach it.

use std::cell::{Cell, RefCell};

use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::icons;
use super::load;
use super::win as lw;

const CLASS: windows::core::PCWSTR = w!("NopBar.DeskHost");
/// Our foreground WinEvent (wParam: the new foreground window).
const WM_FOREGROUND: u32 = WM_APP + 1;
/// The desktop folders changed (icons::watch).
const WM_DESK_NOTICE: u32 = WM_APP + 2;
/// The grid is read again once a burst of changes settles (a copy of
/// many files).
const GRID_TIMER: usize = 1;
const GRID_MS: u32 = 700;
const BATTERY_TIMER: usize = 2;
const BACKDROP_TIMER: usize = 3;
const DISPLAY_TIMER: usize = 5;
const DISPLAY_MS: u32 = 1600;
#[derive(Default)]
struct DisplayWatch {
    monitors: Vec<crate::monitors::Monitor>,
    since: Option<std::time::Instant>,
}
impl DisplayWatch {
    fn observe(&mut self, monitors: Vec<crate::monitors::Monitor>, now: std::time::Instant) -> bool {
        if self.monitors == monitors { return false; }
        self.monitors = monitors;
        self.since = Some(now);
        true
    }
    fn settled(&mut self, now: std::time::Instant) -> bool {
        if self.since.is_some_and(|at| now.duration_since(at).as_millis() >= u128::from(DISPLAY_MS)) {
            self.since = None;
            true
        } else { false }
    }
}
unsafe fn display_change(hwnd: HWND) {
    DISPLAYS.with(|d| {
        let mut d = d.borrow_mut();
        d.monitors = crate::monitors::all();
        d.since = Some(std::time::Instant::now());
    });
    super::grid_state::begin_display_change();
    super::edit::finish();
    super::edit::displays_changed();
    SetTimer(Some(hwnd), DISPLAY_TIMER, DISPLAY_MS, None);
}
static EDIT_HOST:std::sync::atomic::AtomicIsize=std::sync::atomic::AtomicIsize::new(0);
pub fn wake_edit(){let h=EDIT_HOST.load(std::sync::atomic::Ordering::Relaxed);if h!=0{unsafe{let _=PostMessageW(Some(HWND(h as *mut _)),super::edit::REFRESH,WPARAM(0),LPARAM(0));}}}

thread_local! {
    /// (the window, the foreground hook).
    static HOST: Cell<Option<(isize, isize)>> = const { Cell::new(None) };
    /// The desktop folders' watch.
    static NOTICE: RefCell<icons::Watch> = RefCell::new(icons::Watch::default());
    /// An empty secondary display can change DPI without changing any icon.
    static DISPLAYS: RefCell<DisplayWatch> = RefCell::new(DisplayWatch::default());
}

fn hwnd() -> Option<HWND> {
    HOST.with(|h| h.get()).map(|(h, _)| HWND(h as *mut _))
}

/// Makes the window (once, on the dock's thread) and starts what it hosts.
pub fn init(cfg: &crate::config::Config) {
    if hwnd().is_some() {
        return;
    }
    match unsafe { create_window() } {
        Ok(h) => {
            let hook = lw::watch_foreground(h, WM_FOREGROUND);
            HOST.with(|c| c.set(Some((h.0 as isize, hook))));
            EDIT_HOST.store(h.0 as isize,std::sync::atomic::Ordering::Relaxed);
            super::live::refresh();
            unsafe { SetTimer(Some(h), BACKDROP_TIMER, 60_000, None); }
            DISPLAYS.with(|d| d.borrow_mut().monitors = crate::monitors::all());
            apply_config(cfg);
            super::deskpaste::set_widgets(cfg.widgets.notes, cfg.widgets.pieces);
            super::deskpaste::set_owner(Some(h));
            NOTICE.with(|n| *n.borrow_mut() = icons::watch(h, WM_DESK_NOTICE));
        }
        Err(e) => crate::log!("desklayer: no host window ({e})"),
    }
}

/// Settings changed: which widgets the desktop's Ctrl+V makes.
pub fn apply_config(cfg: &crate::config::Config) {
    if let Some(h) = hwnd() { unsafe { SetTimer(Some(h), GRID_TIMER, GRID_MS, None); } }
    super::deskpaste::set_widgets(cfg.widgets.notes, cfg.widgets.pieces);
    if let Some(h) = hwnd() { unsafe {
        if cfg.widgets.device_battery { SetTimer(Some(h), BATTERY_TIMER, 1000, None); }
        else { let _ = KillTimer(Some(h), BATTERY_TIMER); }
    }}
}

/// NopBar is quitting: widget files this thread made that would not go
/// are tried once more (disk::retry_strays; what is left is swept at the
/// next start).
pub fn shutdown() {
    super::edit::finish();super::edit::process();
    EDIT_HOST.store(0,std::sync::atomic::Ordering::Relaxed);
    crate::device_battery::runtime::shutdown();
    let _ = super::disk::retry_strays();
    let Some((h, hook)) = HOST.with(|c| c.take()) else { return };
    lw::unwatch_foreground(hook);
    icons::unwatch(NOTICE.with(|n| std::mem::take(&mut *n.borrow_mut())));
    super::deskpaste::set_owner(None);
    unsafe {
        let _ = DestroyWindow(HWND(h as *mut _));
    }
}

unsafe fn create_window() -> Result<HWND> {
    let hinst = GetModuleHandleW(None)?;
    let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: CLASS, ..Default::default() };
    RegisterClassExW(&wc);
    CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, CLASS, None, WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst.into()), None)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let _span = load::span(load::Part::Widget(load::Kind::Pieces));
    match msg {
        WM_FOREGROUND => {
            let fg = HWND(wp.0 as *mut _);
            crate::launchpad::wintap::foreground(fg);
            crate::companion::foreground_changed();
            super::edit::foreground(fg);
            crate::pieces::on_foreground(fg, lw::is_shell(fg));
            crate::device_battery::runtime::reattach();
            LRESULT(0)
        }
        m if m==super::edit::REFRESH=>{super::edit::process();LRESULT(0)},
        WM_SETTINGCHANGE | WM_POWERBROADCAST => {
            super::live::refresh();
            if msg == WM_SETTINGCHANGE {
                crate::notification_state::changed();
                let changed = DISPLAYS.with(|d| d.borrow_mut().observe(crate::monitors::all(), std::time::Instant::now()));
                if changed || wp.0 as u32 == SPI_SETWORKAREA.0 { display_change(hwnd); }
            }
            let _ = SetTimer(Some(hwnd), GRID_TIMER, GRID_MS, None);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_TIMER if wp.0 == BACKDROP_TIMER => {
            super::live::refresh();
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            super::live::refresh();
            display_change(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == DISPLAY_TIMER => {
            let now = std::time::Instant::now();
            let changed = DISPLAYS.with(|d| d.borrow_mut().observe(crate::monitors::all(), now));
            if changed { display_change(hwnd); }
            else if DISPLAYS.with(|d| d.borrow_mut().settled(now)) {
                let _ = KillTimer(Some(hwnd), DISPLAY_TIMER);
                super::grid_state::end_display_change();
                super::edit::displays_changed();
                crate::pieces::displays_changed();
                crate::device_battery::runtime::reattach();
                icons::refresh();
            }
            LRESULT(0)
        }
        WM_DESK_NOTICE => {
            icons::on_notice(wp, lp);
            let _ = SetTimer(Some(hwnd), GRID_TIMER, GRID_MS, None);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == GRID_TIMER => {
            if super::grid_state::display_settling() { return LRESULT(0); }
            let _ = KillTimer(Some(hwnd), GRID_TIMER);
            icons::refresh();
            LRESULT(0)
        }
        WM_TIMER if wp.0 == BATTERY_TIMER => {
            crate::device_battery::runtime::tick();
            LRESULT(0)
        }
        m if m == super::deskpaste::WM_DESKPASTE => {
            super::deskpaste::on_message(wp, lp);
            LRESULT(0)
        }
        m if m == lw::taskbar_created() => {
            // Explorer restarted: its folders' watch anew.
            NOTICE.with(|n| {
                let mut n = n.borrow_mut();
                icons::unwatch(std::mem::take(&mut *n));
                *n = icons::watch(hwnd, WM_DESK_NOTICE);
            });
            icons::refresh();
            crate::pieces::reattach();
            crate::device_battery::runtime::reattach();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secondary_dpi_change_without_geometry_change_triggers_reflow() {
        use std::time::{Duration, Instant};
        let t = Instant::now();
        let rect = windows::Win32::Foundation::RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let primary = crate::monitors::Monitor { handle: 1, rect, work: rect, primary: true, dpi: 96 };
        let mut secondary = crate::monitors::Monitor { handle: 2, primary: false, ..primary };
        let mut watch = DisplayWatch { monitors: vec![primary, secondary], since: None };
        secondary.dpi = 144;
        assert!(watch.observe(vec![primary, secondary], t));
        assert!(!watch.observe(vec![primary, secondary], t + Duration::from_millis(100)));
        assert!(watch.settled(t + Duration::from_millis(DISPLAY_MS as u64)));
        assert!(watch.since.is_none());
    }
    #[test]
    fn review_display_burst_reflows_only_after_last_stable_sample() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let rect = windows::Win32::Foundation::RECT { left:0, top:0, right:1920, bottom:1080 };
        let mut m = crate::monitors::Monitor { handle:1, rect, work:rect, primary:true, dpi:96 };
        let mut state = DisplayWatch::default();
        assert!(state.observe(vec![m], start));
        assert!(!state.settled(start + Duration::from_millis(1500)));
        m.work.bottom -= 40;
        assert!(state.observe(vec![m], start + Duration::from_millis(1500)));
        assert!(!state.settled(start + Duration::from_millis(3000)));
        assert!(state.settled(start + Duration::from_millis(3100)));
        assert!(!state.settled(start + Duration::from_millis(5000)));
        assert!(!state.observe(vec![m], start + Duration::from_millis(6000)));
    }

    #[test]
    fn its_messages_do_not_collide() {
        let ids = [WM_FOREGROUND, WM_DESK_NOTICE, super::super::deskpaste::WM_DESKPASTE];
        for (i, a) in ids.iter().enumerate() {
            assert!(!ids[i + 1..].contains(a), "duplicate host message {a}");
        }
    }

    #[test]
    fn monitor_changes_use_broadcasts_without_periodic_polling() {
        let code = include_str!("host.rs");
        let code = code.split("#[cfg(test)]").next().unwrap();
        assert!(!code.contains("GRID_POLL_TIMER"));
        assert!(code.contains("WM_DISPLAYCHANGE | WM_DPICHANGED"));
        assert!(code.contains("SPI_SETWORKAREA"));
    }
}
