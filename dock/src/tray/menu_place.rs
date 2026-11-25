//! Watch the clicked icon's process for the popup it opens.

use std::cell::Cell;
use std::time::{Duration, Instant};
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONULL},
    UI::{
        Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
        WindowsAndMessaging::*,
    },
};

const WATCH_MS: u32 = 1500;
const MENU_EVENT_GRACE_MS: u32 = 75;
const MOVE_CHECK_MS: u32 = 50;

#[derive(Clone, Copy)]
struct Watch {
    pid: u32,
    at: POINT,
    until: Instant,
    hooks: [HWINEVENTHOOK; 2],
    timer: Option<usize>,
    fallback: Option<HWND>,
    fallback_timer: Option<usize>,
    moved: Option<HWND>,
}

thread_local! {
    static ACTIVE: Cell<Option<Watch>> = const { Cell::new(None) };
}

pub(super) fn stop() {
    if let Some(watch) = ACTIVE.take() {
        if let Some(id) = watch.timer {
            crate::motion::cancel(id);
        }
        if let Some(id) = watch.fallback_timer {
            crate::motion::cancel(id);
        }
        for hook in watch.hooks.into_iter().filter(|h| !h.is_invalid()) {
            unsafe { let _ = UnhookWinEvent(hook); }
        }
    }
}

pub(super) unsafe fn watch(pid: u32, at: POINT) {
    stop();
    if cfg!(test) || pid == 0 || pid == std::process::id() {
        return;
    }
    let hooks = [EVENT_SYSTEM_MENUPOPUPSTART, EVENT_OBJECT_SHOW].map(|event| {
        SetWinEventHook(event, event, None, Some(shown), pid, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)
    });
    if hooks.iter().all(|h| h.is_invalid()) {
        return;
    }
    let until = Instant::now() + Duration::from_millis(u64::from(WATCH_MS));
    ACTIVE.set(Some(Watch { pid, at, until, hooks, timer: None, fallback: None, fallback_timer: None, moved: None }));
    if let Some(timer) = crate::motion::after(WATCH_MS, Box::new(stop)) {
        if let Some(watch) = ACTIVE.get() {
            ACTIVE.set(Some(Watch { timer: Some(timer), ..watch }));
        }
    }
}

fn popup_style(style: u32) -> bool {
    style & WS_POPUP.0 != 0 && style & WS_CAPTION.0 != WS_CAPTION.0 && style & WS_CHILD.0 == 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Candidate {
    Menu,
    Popup,
}

fn candidate(event: u32, class: &str, style: u32, ex: u32) -> Option<Candidate> {
    if !popup_style(style) || class.eq_ignore_ascii_case("tooltips_class32") || class == "SysShadow" {
        return None;
    }
    if event == EVENT_SYSTEM_MENUPOPUPSTART || class == "#32768" {
        return Some(Candidate::Menu);
    }
    (ex & (WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) != 0
        && ex & WS_EX_APPWINDOW.0 == 0 && style & WS_THICKFRAME.0 == 0)
        .then_some(Candidate::Popup)
}

fn destination(at: POINT, popup: RECT, work: RECT, same_monitor: bool) -> Option<POINT> {
    if same_monitor && popup.left >= work.left && popup.right <= work.right
        && popup.top >= work.top && popup.bottom <= work.bottom
    {
        return None;
    }
    let (w, h) = (popup.right - popup.left, popup.bottom - popup.top);
    if w <= 0 || h <= 0 || w > work.right - work.left || h > work.bottom - work.top {
        return None;
    }
    Some(POINT {
        x: (at.x - w / 2).clamp(work.left, work.right - w),
        y: at.y.clamp(work.top, work.bottom - h),
    })
}

unsafe extern "system" fn shown(hook: HWINEVENTHOOK, event: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    let Some(watch) = ACTIVE.get().filter(|watch| watch.hooks.contains(&hook) && Instant::now() < watch.until) else { return };
    if hwnd.is_invalid() || child != 0 || (event == EVENT_OBJECT_SHOW && object != OBJID_WINDOW.0)
        || !IsWindowVisible(hwnd).as_bool() || GetAncestor(hwnd, GA_ROOT) != hwnd
    {
        return;
    }
    if !belongs_to(hwnd, watch.pid) {
        return;
    }
    let mut class = [0u16; 64];
    let len = GetClassNameW(hwnd, &mut class) as usize;
    match candidate(
        event, &String::from_utf16_lossy(&class[..len]),
        GetWindowLongW(hwnd, GWL_STYLE) as u32, GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
    ) {
        Some(Candidate::Menu) => {
            if watch.moved == Some(hwnd) || reposition(hwnd, watch).is_some() {
                stop();
            }
        }
        Some(Candidate::Popup) if watch.moved.is_none() => {
            // Native menu events often follow SHOW. Give those first pick.
            ACTIVE.set(Some(Watch { fallback: Some(hwnd), ..watch }));
            if watch.fallback_timer.is_none() {
                if let Some(timer) = crate::motion::after(MENU_EVENT_GRACE_MS, Box::new(use_fallback)) {
                    ACTIVE.set(Some(Watch { fallback: Some(hwnd), fallback_timer: Some(timer), ..watch }));
                }
            }
        }
        _ => {}
    }
}

unsafe fn belongs_to(hwnd: HWND, pid: u32) -> bool {
    let mut owner = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut owner));
    owner == pid && IsWindowVisible(hwnd).as_bool() && !crate::apps::is_cloaked(hwnd)
}

fn use_fallback() {
    let Some(mut watch) = ACTIVE.get() else { return };
    let popup = watch.fallback.take();
    watch.fallback_timer = None;
    if let Some(hwnd) = popup.filter(|_| Instant::now() < watch.until) {
        if unsafe { belongs_to(hwnd, watch.pid) && reposition(hwnd, watch) == Some(true) } {
            watch.moved = Some(hwnd);
        }
    }
    ACTIVE.set(Some(watch));
}

// None means the popup could not be handled. Keep watching in that case.
unsafe fn reposition(hwnd: HWND, watch: Watch) -> Option<bool> {
    let mut popup = RECT::default();
    if GetWindowRect(hwnd, &mut popup).is_err() || popup.right <= popup.left || popup.bottom <= popup.top {
        return None;
    }
    let target = MonitorFromPoint(watch.at, MONITOR_DEFAULTTONULL);
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if target.is_invalid() || !GetMonitorInfoW(target, &mut info).as_bool() {
        return None;
    }
    let same_monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) == target;
    let Some(to) = destination(watch.at, popup, info.rcWork, same_monitor) else { return Some(false) };
    if !move_popup(hwnd, to) {
        return None;
    }
    crate::log!("tray: menu reposition pid={} hwnd={:#x} ({},{}) -> ({},{})", watch.pid, hwnd.0 as usize, popup.left, popup.top, to.x, to.y);
    check_move(hwnd, watch.pid, watch.at, info.rcWork, popup, Instant::now() + Duration::from_millis(u64::from(WATCH_MS)));
    Some(true)
}

unsafe fn move_popup(hwnd: HWND, to: POINT) -> bool {
    let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
    SetWindowPos(hwnd, None, to.x, to.y, 0, 0, flags).is_ok()
}

fn check_move(hwnd: HWND, pid: u32, at: POINT, work: RECT, before: RECT, until: Instant) {
    // The owner's message loop applies the move and any DPI resize.
    crate::motion::after(MOVE_CHECK_MS, Box::new(move || unsafe {
        let mut actual = RECT::default();
        if !belongs_to(hwnd, pid) || GetWindowRect(hwnd, &mut actual).is_err() {
            return;
        }
        if actual == before {
            if Instant::now() < until {
                check_move(hwnd, pid, at, work, before, until);
            }
        } else if let Some(to) = destination(at, actual, work, true) {
            move_popup(hwnd, to);
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn misplaced_popups_move_below_the_clicked_cell_in_physical_coordinates() {
        let at = POINT { x: -40, y: 55 };
        let work = RECT { left: -2_560, top: 39, right: 0, bottom: 1_600 };
        let misplaced = RECT { left: 30, top: -250, right: 230, bottom: 50 };
        assert_eq!(destination(at, misplaced, work, false), Some(POINT { x: -200, y: 55 }));
        assert_eq!(destination(at, misplaced, work, true), Some(POINT { x: -200, y: 55 }));
        let bottom = POINT { y: 1_550, ..at };
        assert_eq!(destination(bottom, misplaced, work, false), Some(POINT { x: -200, y: 1_300 }));
        let placed = RECT { left: -200, top: 55, right: 0, bottom: 355 };
        assert_eq!(destination(at, placed, work, true), None);
    }

    #[test]
    fn only_captionless_top_level_popups_qualify() {
        assert!(popup_style(WS_POPUP.0 | WS_BORDER.0));
        assert!(!popup_style(WS_POPUP.0 | WS_CAPTION.0));
        assert!(!popup_style(WS_POPUP.0 | WS_CHILD.0));
        assert!(!popup_style(WS_OVERLAPPEDWINDOW.0));
    }

    #[test]
    fn menu_events_and_native_menus_take_priority_over_show_popups() {
        let show = EVENT_OBJECT_SHOW;
        let popup = WS_POPUP.0;
        let topmost = WS_EX_TOPMOST.0;
        assert_eq!(candidate(EVENT_SYSTEM_MENUPOPUPSTART, "Custom", popup, 0), Some(Candidate::Menu));
        assert_eq!(candidate(show, "#32768", popup, 0), Some(Candidate::Menu));
        assert_eq!(candidate(show, "Custom", popup, topmost), Some(Candidate::Popup));
        assert_eq!(candidate(show, "Custom", popup, WS_EX_TOOLWINDOW.0), Some(Candidate::Popup));
        assert_eq!(candidate(show, "Custom", popup, 0), None);
        assert_eq!(candidate(show, "Custom", popup | WS_THICKFRAME.0, topmost), None);
        assert_eq!(candidate(show, "Custom", popup, topmost | WS_EX_APPWINDOW.0), None);
        assert_eq!(candidate(show, "tooltips_class32", popup, topmost), None);
    }
}
