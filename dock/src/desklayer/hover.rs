//! Pointer/focus chrome, 120 ms in both directions; no polling when settled.
use std::{sync::Mutex, time::Instant};
use windows::Win32::{
    Foundation::*,
    Graphics::Gdi::InvalidateRect,
    UI::{
        Controls::WM_MOUSELEAVE,
        Input::KeyboardAndMouse::*,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};
const TIMER: usize = 0x5748;
pub const REDRAW: u32 = WM_APP + 0x2c0;
#[derive(Clone, Copy)]
struct Fade {
    value: f32,
    target: f32,
    at: Instant,
    focused: bool,
    hovered: bool,
}
impl Fade {
    fn alpha(self) -> f32 {
        let d = (self.at.elapsed().as_secs_f32() / 0.120).clamp(0., 1.);
        self.value + (self.target - self.value) * d
    }
}
static STATES: Mutex<Vec<(isize, Fade)>> = Mutex::new(Vec::new());
pub fn alpha(h: HWND) -> f32 {
    STATES
        .lock()
        .ok()
        .and_then(|v| {
            v.iter()
                .find(|(id, _)| *id == h.0 as isize)
                .map(|(_, f)| f.alpha())
        })
        .unwrap_or(0.)
}
pub fn attach(h: HWND) {
    unsafe {
        let _ = SetWindowSubclass(h, Some(proc), TIMER, 0);
    }
}
unsafe extern "system" fn proc(
    h: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    let still_inside = if m == WM_MOUSELEAVE {
        // The card, not the window: its shadow margin is not "inside".
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        super::icons::card(h).is_some_and(|r| p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom)
    } else {
        false
    };
    if matches!(
        m,
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_SETFOCUS | WM_KILLFOCUS | WM_ACTIVATE
    ) {
        if m == WM_MOUSEMOVE {
            let mut t = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: h,
                ..Default::default()
            };
            let _ = TrackMouseEvent(&mut t);
        }
        if let Ok(mut states) = STATES.lock() {
            if !states.iter().any(|(id, _)| *id == h.0 as isize) {
                states.push((
                    h.0 as isize,
                    Fade {
                        value: 0.,
                        target: 0.,
                        at: Instant::now(),
                        focused: false,
                        hovered: false,
                    },
                ));
            }
            if let Some((_, f)) = states.iter_mut().find(|(id, _)| *id == h.0 as isize) {
                match m {
                    WM_MOUSEMOVE => f.hovered = true,
                    WM_MOUSELEAVE => f.hovered = still_inside,
                    WM_SETFOCUS => f.focused = true,
                    WM_KILLFOCUS => f.focused = GetAncestor(GetFocus(), GA_ROOT) == h,
                    WM_ACTIVATE => f.focused = w.0 & 0xffff != WA_INACTIVE as usize,
                    _ => {}
                }
                let target = if f.focused || f.hovered { 1. } else { 0. };
                if target != f.target {
                    f.value = f.alpha();
                    f.target = target;
                    f.at = Instant::now();
                    SetTimer(Some(h), TIMER, 16, None);
                }
            }
        }
    }
    if m == WM_TIMER && w.0 == TIMER {
        let _ = InvalidateRect(Some(h), None, false);
        let _ = PostMessageW(Some(h), REDRAW, WPARAM(0), LPARAM(0));
        let settled = STATES.lock().ok().is_none_or(|v| {
            v.iter()
                .find(|(id, _)| *id == h.0 as isize)
                .is_none_or(|(_, f)| f.at.elapsed().as_millis() >= 120)
        });
        if settled {
            let _ = KillTimer(Some(h), TIMER);
        }
        return LRESULT(0);
    }
    if m == WM_NCDESTROY {
        if let Ok(mut v) = STATES.lock() {
            v.retain(|(id, _)| *id != h.0 as isize);
        }
        let _ = RemoveWindowSubclass(h, Some(proc), TIMER);
    }
    DefSubclassProc(h, m, w, l)
}
