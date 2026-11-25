//! Refit titled windows stranded in a newly reserved top strip.

use windows::core::BOOL;
use windows::Win32::{
    Foundation::{HWND, LPARAM, RECT},
    Graphics::{
        Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
        Gdi::{GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONULL},
    },
    UI::WindowsAndMessaging::*,
};

fn has_caption(style: u32, ex: u32, owned: bool) -> bool {
    !owned && style & WS_CAPTION.0 == WS_CAPTION.0 && style & WS_POPUP.0 == 0 && ex & WS_EX_TOOLWINDOW.0 == 0
}

struct Placement {
    monitor: HMONITOR,
    screen: RECT,
    work: RECT,
    strip_bottom: i32,
}

impl Placement {
    fn target(&self, window: RECT, frame: RECT, maximized: bool, style: u32) -> Option<RECT> {
        if frame.top < self.screen.top || frame.top >= self.strip_bottom
            || crate::dock::covers_as_fullscreen(frame, maximized, style, self.screen, Some((self.screen, self.work)))
        {
            return None;
        }
        if maximized {
            // Keep the window maximized, including any invisible resize borders.
            Some(RECT {
                left: self.work.left + window.left - frame.left,
                top: self.work.top + window.top - frame.top,
                right: self.work.right + window.right - frame.right,
                bottom: self.work.bottom + window.bottom - frame.bottom,
            })
        } else {
            let dy = self.work.top - frame.top;
            let bottom = (frame.bottom + dy).min(self.work.bottom) + window.bottom - frame.bottom;
            Some(RECT { top: window.top + dy, bottom, ..window })
        }
    }
}

unsafe extern "system" fn refit(hwnd: HWND, arg: LPARAM) -> BOOL {
    let open = BOOL(1);
    let plan = &*(arg.0 as *const Placement);
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() || IsWindowArranged(hwnd).as_bool()
        || MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) != plan.monitor
        || !has_caption(style, GetWindowLongW(hwnd, GWL_EXSTYLE) as u32, GetWindow(hwnd, GW_OWNER).is_ok())
    {
        return open;
    }
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == 0 || pid == std::process::id() || crate::apps::is_cloaked(hwnd) {
        return open;
    }
    let mut window = RECT::default();
    if GetWindowRect(hwnd, &mut window).is_err() {
        return open;
    }
    let mut frame = RECT::default();
    if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, (&mut frame as *mut RECT).cast(), size_of::<RECT>() as u32).is_err() {
        frame = window;
    }
    let maximized = IsZoomed(hwnd).as_bool();
    if let Some(to) = plan.target(window, frame, maximized, style).filter(|to| *to != window) {
        let mut flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
        if !maximized && to.bottom - to.top == window.bottom - window.top {
            flags |= SWP_NOSIZE;
        }
        if SetWindowPos(hwnd, None, to.left, to.top, to.right - to.left, to.bottom - to.top, flags).is_ok() {
            crate::log!("appbar: caption refit pid={pid} hwnd={:#x} y={} -> {} maximized={maximized}", hwnd.0 as usize, window.top, to.top);
        }
    }
    open
}

/// Called by the shell worker after ABM_SETPOS has reserved the top strip.
pub(crate) fn refit_after_reserve(screen: RECT, strip_bottom: i32) {
    if cfg!(test) || strip_bottom <= screen.top {
        return;
    }
    unsafe {
        let monitor = MonitorFromRect(&screen, MONITOR_DEFAULTTONULL);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if monitor.is_invalid() || !GetMonitorInfoW(monitor, &mut info).as_bool() || info.rcMonitor != screen {
            return;
        }
        let work = RECT { top: info.rcWork.top.max(strip_bottom), ..info.rcWork };
        if work.top >= work.bottom {
            return;
        }
        let plan = Placement { monitor, screen, work, strip_bottom };
        let _ = EnumWindows(Some(refit), LPARAM((&plan as *const Placement) as isize));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titlebar_repair_moves_a_normal_frame_and_refits_a_maximized_frame() {
        let screen = RECT { left: -2_560, top: 0, right: 0, bottom: 1_600 };
        let work = RECT { top: 39, bottom: 1_550, ..screen };
        let plan = Placement { monitor: HMONITOR::default(), screen, work, strip_bottom: 39 };
        let frame = RECT { left: -2_000, top: 8, right: -600, bottom: 900 };
        let window = RECT { left: -2_008, top: 0, right: -592, bottom: 908 };
        let style = WS_CAPTION.0;
        assert_eq!(plan.target(window, frame, false, style), Some(RECT { top: 31, bottom: 939, ..window }));
        assert_eq!(plan.target(window, frame, true, style), Some(RECT { left: -2_568, top: 31, right: 8, bottom: 1_558 }));
        let tall = RECT { bottom: work.bottom, ..frame };
        let tall_window = RECT { bottom: tall.bottom + 8, ..window };
        assert_eq!(plan.target(tall_window, tall, false, style), Some(RECT { top: 31, ..tall_window }));
        assert_eq!(plan.target(screen, screen, false, style), None);
        assert_eq!(plan.target(screen, screen, true, WS_POPUP.0), None);
        assert_eq!(plan.target(window, RECT { top: 39, ..frame }, false, style), None);
        assert_eq!(plan.target(window, RECT { top: -40, ..frame }, false, style), None);
    }

    #[test]
    fn popups_tool_windows_and_owned_windows_do_not_move() {
        assert!(has_caption(WS_CAPTION.0, 0, false));
        assert!(!has_caption(WS_POPUP.0 | WS_CAPTION.0, 0, false));
        assert!(!has_caption(WS_CAPTION.0, WS_EX_TOOLWINDOW.0, false));
        assert!(!has_caption(WS_CAPTION.0, 0, true));
        assert!(!has_caption(WS_THICKFRAME.0, 0, false));
    }
}
