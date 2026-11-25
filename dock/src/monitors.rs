//! Monitors: list, primary, the one under the cursor, and each one's DPI.

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    /// HMONITOR as isize (stable while the display configuration is).
    pub handle: isize,
    pub rect: RECT,
    pub work: RECT,
    pub primary: bool,
    pub dpi: u32,
}

impl Monitor {
    pub fn scale(&self) -> f32 {
        self.dpi.max(96) as f32 / 96.0
    }
}

/// Details of one monitor; None if it is gone.
pub fn info(handle: isize) -> Option<Monitor> {
    unsafe {
        let h = HMONITOR(handle as *mut _);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(h, &mut mi).as_bool() {
            return None;
        }
        let (mut x, mut y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        Some(Monitor { handle, rect: mi.rcMonitor, work: mi.rcWork, primary: mi.dwFlags & 1 != 0, dpi: x })
    }
}

/// All monitors, primary first.
pub fn all() -> Vec<Monitor> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<isize>);
        out.push(h.0 as isize);
        BOOL(1)
    }
    let mut handles: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut handles as *mut _ as isize));
    }
    let mut list: Vec<Monitor> = handles.into_iter().filter_map(info).collect();
    list.sort_by_key(|m| !m.primary);
    list
}

/// All monitors, primary first, read surely: the listing worked, every
/// monitor's details and DPI were read, and exactly one is the primary.
/// None at the first doubt (desklayer::icons moves icons only with this).
pub fn all_sure() -> Option<Vec<Monitor>> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut Vec<isize>);
        out.push(h.0 as isize);
        BOOL(1)
    }
    let mut handles: Vec<isize> = Vec::new();
    if !unsafe { EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut handles as *mut _ as isize)) }.as_bool() || handles.is_empty() {
        return None;
    }
    let mut list = Vec::with_capacity(handles.len());
    for handle in handles {
        let h = HMONITOR(handle as *mut _);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !unsafe { GetMonitorInfoW(h, &mut mi) }.as_bool() {
            return None;
        }
        let (mut x, mut y) = (0u32, 0u32);
        unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.ok()?;
        if x == 0 {
            return None;
        }
        list.push(Monitor { handle, rect: mi.rcMonitor, work: mi.rcWork, primary: mi.dwFlags & 1 != 0, dpi: x });
    }
    if list.iter().filter(|m| m.primary).count() != 1 {
        return None;
    }
    list.sort_by_key(|m| !m.primary);
    Some(list)
}

pub fn primary() -> Option<Monitor> {
    let h = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    info(h.0 as isize)
}

/// The monitor under the cursor (nearest, if between screens).
pub fn under_cursor() -> Option<Monitor> {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
        info(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST).0 as isize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_comes_first() {
        let list = all();
        assert!(!list.is_empty());
        assert!(list[0].primary);
        assert_eq!(primary().map(|m| m.handle), Some(list[0].handle));
        assert!(list.iter().all(|m| m.dpi >= 96 && m.rect.right > m.rect.left));
        // Read surely: the same displays (only reads, nothing changed).
        assert_eq!(all_sure(), Some(list));
    }
}
