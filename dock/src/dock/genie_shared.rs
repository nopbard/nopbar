//! What the genie minimize (genie_thumb.rs) is built from: the strip
//! geometry (pure, tested) and the crash-safe handling of the target
//! window's DWM transitions.
//!
//! Crash safety: the target's DWMWA_TRANSITIONS_FORCEDISABLED (original
//! value) is written to `genie-pending` before it is changed and removed
//! when it is put back; the guardian and the next start restore leftovers.

use windows::core::{Result, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DwmSetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS, DWMWA_TRANSITIONS_FORCEDISABLED};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow, PostMessageW, SC_MINIMIZE, WM_SYSCOMMAND};

use super::Rect;
use crate::config::Position;

/// Product timing (S4): ease-in-out (0.42, 0, 0.58, 1).
pub(super) const EASE: (f32, f32, f32, f32) = (0.42, 0.0, 0.58, 1.0);

// ---- geometry (pure, tested) -------------------------------------------------

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// CSS-style cubic-bezier timing: progress at time `t` (0..1).
pub(super) fn bezier(t: f32, (x1, y1, x2, y2): (f32, f32, f32, f32)) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let at = |s: f32, a: f32, b: f32| {
        let m = 1.0 - s;
        3.0 * m * m * s * a + 3.0 * m * s * s * b + s * s * s
    };
    // x(s) is monotonic for x1, x2 in 0..1: bisect.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if at(mid, x1, x2) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at((lo + hi) / 2.0, y1, y2)
}

/// Strip at `v` (0 far side .. 1 dock side) at progress `p`, in the
/// canonical frame where the dock is below: (x, y, scale_x, scale_y).
/// Phase 1 (squeeze) pinches the dock side toward the icon first; phase 2
/// (slide) pulls every strip into the icon. (S4 spike formula.)
fn strip_at(p: f32, v: f32, win: (f32, f32, f32, f32), icon: (f32, f32, f32)) -> (f32, f32, f32, f32) {
    let (wx, wy, ww, wh) = win;
    let (tx, ty, isz) = icon;
    let squeeze = ease(p * 1.6 - (1.0 - v) * 0.6);
    let slide = ease((p - 0.25) / 0.75);
    let width = ww + (isz - ww) * squeeze;
    let cx = (wx + ww / 2.0) + (tx - (wx + ww / 2.0)) * squeeze;
    let y0 = wy + v * wh;
    let y1 = (ty - isz / 2.0) + v * isz;
    let y = y0 + (y1 - y0) * slide;
    let sy = 1.0 + ((isz / wh) - 1.0) * slide;
    (cx - width / 2.0, y, width / ww, sy)
}

/// Screen rect -> canonical frame (dock below). Right: transpose; Left:
/// transpose with the x axis flipped, so "toward the dock" is always +y.
fn to_canon(pos: Position, r: Rect) -> Rect {
    match pos {
        Position::Bottom => r,
        Position::Right => Rect { x: r.y, y: r.x, w: r.h, h: r.w },
        Position::Left => Rect { x: r.y, y: -(r.x + r.w), w: r.h, h: r.w },
    }
}

fn from_canon(pos: Position, c: Rect) -> Rect {
    match pos {
        Position::Bottom => c,
        Position::Right => Rect { x: c.y, y: c.x, w: c.h, h: c.w },
        Position::Left => Rect { x: -(c.y + c.h), y: c.x, w: c.h, h: c.w },
    }
}

/// Where strip `i` of `n` is in the capture (`w` x `h` px): rows for a
/// bottom dock, columns for side docks (strip n-1 is next to the dock).
pub(super) fn strip_source(pos: Position, i: usize, n: usize, w: f32, h: f32) -> Rect {
    let k = i as f32;
    let n = n as f32;
    match pos {
        Position::Bottom => Rect { x: 0.0, y: k * h / n, w, h: h / n },
        Position::Right => Rect { x: k * w / n, y: 0.0, w: w / n, h },
        Position::Left => Rect { x: w - (k + 1.0) * w / n, y: 0.0, w: w / n, h },
    }
}

/// Screen rect of strip `i` of `n` at progress `p`, flying from window rect
/// `win` into icon rect `icon` (both screen px) on a dock at `pos`.
pub(super) fn strip_rect(pos: Position, p: f32, i: usize, n: usize, win: Rect, icon: Rect) -> Rect {
    let cw = to_canon(pos, win);
    let ci = to_canon(pos, icon);
    let isz = ci.w.min(ci.h);
    let v = i as f32 / n as f32;
    let (x, y, sx, sy) = strip_at(p, v, (cw.x, cw.y, cw.w, cw.h), (ci.x + ci.w / 2.0, ci.y + ci.h / 2.0, isz));
    from_canon(pos, Rect { x, y, w: cw.w * sx, h: cw.h / n as f32 * sy })
}

// ---- crash safety: the target's DWM transitions ------------------------------------

/// A window whose transitions we turned off, and what they were before.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Saved {
    hwnd: isize,
    pid: u32,
    /// DWMWA_TRANSITIONS_FORCEDISABLED before we touched it.
    original: bool,
}

fn pending_file() -> std::path::PathBuf {
    crate::config::app_dir().join("genie-pending")
}

fn format_saved(s: &Saved) -> String {
    format!("{} {} {}\n", s.hwnd, s.pid, s.original as u8)
}

fn parse_saved(text: &str) -> Vec<Saved> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let hwnd = it.next()?.parse().ok()?;
            let pid = it.next()?.parse().ok()?;
            let original = match it.next()? {
                "0" => false,
                "1" => true,
                _ => return None,
            };
            Some(Saved { hwnd, pid, original })
        })
        .collect()
}

/// Windows whose transitions are off right now (several at once for
/// "None" minimizes), mirrored to genie-pending.
static PENDING: std::sync::Mutex<Vec<Saved>> = std::sync::Mutex::new(Vec::new());

/// `list` with `s` added (replacing an older record of the same window).
fn with_record(list: &[Saved], s: Saved) -> Vec<Saved> {
    let mut out: Vec<Saved> = list.iter().filter(|o| o.hwnd != s.hwnd).copied().collect();
    out.push(s);
    out
}

/// `list` without the record of `s`'s window.
fn without_record(list: &[Saved], s: &Saved) -> Vec<Saved> {
    list.iter().filter(|o| o.hwnd != s.hwnd).copied().collect()
}

/// Writes the records (atomically), or removes the file when none are left.
fn write_pending(list: &[Saved]) -> std::io::Result<()> {
    let path = pending_file();
    if list.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::create_dir_all(crate::config::app_dir());
    let text: String = list.iter().map(format_saved).collect();
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path))
}

/// Records `s` on disk before the attribute changes, so the guardian (or
/// the next start) can put it back if the dock dies mid-genie.
fn persist(s: &Saved) -> bool {
    let mut list = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let next = with_record(&list, *s);
    match write_pending(&next) {
        Ok(()) => {
            *list = next;
            true
        }
        Err(e) => {
            crate::log!("genie: cannot record the window's transitions ({e}): OS animation");
            false
        }
    }
}

/// Drops `s`'s record (its transitions are back or its window is gone).
fn unpersist(s: &Saved) {
    let mut list = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let next = without_record(&list, s);
    if let Err(e) = write_pending(&next) {
        crate::log!("genie: cannot update genie-pending ({e})");
    }
    *list = next;
}

fn pid_of(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

fn read_transitions_off(h: HWND) -> Option<bool> {
    let mut v = BOOL(0);
    unsafe {
        DwmGetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED, &mut v as *mut _ as *mut _, 4).ok()?;
    }
    Some(v.as_bool())
}

fn write_transitions_off(h: HWND, off: bool) -> Result<()> {
    let v = BOOL(off as i32);
    unsafe { DwmSetWindowAttribute(h, DWMWA_TRANSITIONS_FORCEDISABLED, &v as *const _ as *const _, 4) }
}

/// Turns the target's OS animations off for the genie. Some(None): they
/// were off already (nothing to put back). None: cannot do it safely.
pub(super) fn disable_transitions(h: HWND) -> Option<Option<Saved>> {
    let original = match read_transitions_off(h) {
        Some(v) => v,
        None => {
            // Not readable on this build: the documented default is "on".
            crate::log!("genie: cannot read the window's transitions; assuming on");
            false
        }
    };
    if original {
        return Some(None);
    }
    let s = Saved { hwnd: h.0 as isize, pid: pid_of(h), original };
    if s.pid == 0 || !persist(&s) {
        return None;
    }
    if let Err(e) = write_transitions_off(h, true) {
        crate::log!("genie: cannot turn the window's transitions off ({e:?}): OS animation");
        unpersist(&s);
        return None;
    }
    Some(Some(s))
}

/// Puts the saved value back. The record stays on disk if that failed for
/// a window that still exists (the guardian / next start retry).
pub(super) fn restore_transitions(s: &Saved) {
    let h = HWND(s.hwnd as *mut _);
    let alive = unsafe { IsWindow(Some(h)).as_bool() } && pid_of(h) == s.pid;
    if alive {
        if let Err(e) = write_transitions_off(h, s.original) {
            crate::log!("genie: cannot restore the window's transitions ({e:?}); kept for later");
            return;
        }
    }
    unpersist(s);
}

/// After a crash: restore what a dead dock left changed. Called by the
/// guardian when the dock exits and by the dock when it starts.
pub(super) fn recover_pending() {
    let Some(text) = crate::config::read_capped(&pending_file(), 64 << 10).ok().and_then(|b| String::from_utf8(b).ok()) else { return };
    for s in parse_saved(&text) {
        let h = HWND(s.hwnd as *mut _);
        if unsafe { IsWindow(Some(h)).as_bool() } && pid_of(h) == s.pid {
            let r = write_transitions_off(h, s.original);
            crate::log!("genie: restored transitions of {:#x} after an unclean exit: {:?}", s.hwnd, r.map(|_| "ok"));
        }
    }
    let _ = std::fs::remove_file(pending_file());
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The window's visible bounds (no invisible resize borders), physical px:
/// what Windows.Graphics.Capture captures.
pub(super) fn frame_bounds(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32).ok()?;
    }
    (r.right > r.left && r.bottom > r.top).then_some(r)
}

/// The existing dock path: ask the window to minimize itself.
pub(super) fn os_minimize(h: HWND) {
    let _ = unsafe { PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIPS: usize = 40;

    const WIN: (f32, f32, f32, f32) = (100.0, 100.0, 900.0, 600.0);
    const ICON: (f32, f32, f32) = (1280.0, 1490.0, 100.0);

    #[test]
    fn starts_at_window() {
        for i in 0..STRIPS {
            let v = i as f32 / STRIPS as f32;
            let (x, y, sx, sy) = strip_at(0.0, v, WIN, ICON);
            assert!((x - 100.0).abs() < 0.01 && (sx - 1.0).abs() < 0.01 && (sy - 1.0).abs() < 0.01);
            assert!((y - (100.0 + v * 600.0)).abs() < 0.01);
        }
    }

    #[test]
    fn ends_inside_icon() {
        for i in 0..STRIPS {
            let v = i as f32 / STRIPS as f32;
            let (x, y, sx, _) = strip_at(1.0, v, WIN, ICON);
            let width = sx * 900.0;
            assert!((width - 100.0).abs() < 0.5, "width {width}");
            assert!((x + width / 2.0 - 1280.0).abs() < 0.5);
            assert!(y >= 1490.0 - 50.0 - 0.5 && y <= 1490.0 + 50.0 + 0.5);
        }
    }

    #[test]
    fn bottom_pinches_before_top() {
        let (_, _, top_sx, _) = strip_at(0.3, 0.0, WIN, ICON);
        let (_, _, bot_sx, _) = strip_at(0.3, 0.97, WIN, ICON);
        assert!(bot_sx < top_sx);
    }

    fn near(a: Rect, b: Rect, eps: f32) -> bool {
        (a.x - b.x).abs() < eps && (a.y - b.y).abs() < eps && (a.w - b.w).abs() < eps && (a.h - b.h).abs() < eps
    }

    const SCREEN_WIN: Rect = Rect { x: 300.0, y: 200.0, w: 900.0, h: 600.0 };

    fn icon_for(pos: Position) -> Rect {
        match pos {
            Position::Bottom => Rect { x: 1230.0, y: 1440.0, w: 100.0, h: 100.0 },
            Position::Left => Rect { x: 10.0, y: 700.0, w: 100.0, h: 100.0 },
            Position::Right => Rect { x: 2450.0, y: 700.0, w: 100.0, h: 100.0 },
        }
    }

    #[test]
    fn canon_round_trips() {
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            assert!(near(from_canon(pos, to_canon(pos, SCREEN_WIN)), SCREEN_WIN, 1e-3), "{pos:?}");
        }
    }

    #[test]
    fn every_edge_starts_on_its_source_strip() {
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            for i in 0..STRIPS {
                let r = strip_rect(pos, 0.0, i, STRIPS, SCREEN_WIN, icon_for(pos));
                let s = strip_source(pos, i, STRIPS, SCREEN_WIN.w, SCREEN_WIN.h);
                let want = Rect { x: SCREEN_WIN.x + s.x, y: SCREEN_WIN.y + s.y, w: s.w, h: s.h };
                assert!(near(r, want, 0.01), "{pos:?} strip {i}: {r:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn every_edge_ends_inside_the_icon() {
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            let ic = icon_for(pos);
            for i in 0..STRIPS {
                let r = strip_rect(pos, 1.0, i, STRIPS, SCREEN_WIN, ic);
                assert!(r.x >= ic.x - 0.5 && r.x + r.w <= ic.x + ic.w + 0.5, "{pos:?} {i}: {r:?}");
                assert!(r.y >= ic.y - 0.5 && r.y + r.h <= ic.y + ic.h + 0.5, "{pos:?} {i}: {r:?}");
            }
        }
    }

    #[test]
    fn side_docks_pinch_the_dock_side_first() {
        // Across the flow (screen height for side docks) the strip next to
        // the dock is narrower early on.
        for pos in [Position::Left, Position::Right] {
            let ic = icon_for(pos);
            let far = strip_rect(pos, 0.3, 0, STRIPS, SCREEN_WIN, ic);
            let near_dock = strip_rect(pos, 0.3, STRIPS - 1, STRIPS, SCREEN_WIN, ic);
            assert!(near_dock.h < far.h, "{pos:?}");
        }
        // And the strip next to the dock is the column on the dock's side.
        let l = strip_source(Position::Left, STRIPS - 1, STRIPS, 900.0, 600.0);
        let r = strip_source(Position::Right, STRIPS - 1, STRIPS, 900.0, 600.0);
        assert!(l.x < 1.0 && r.x + r.w > 899.0);
    }

    #[test]
    fn pending_record_round_trips() {
        let a = Saved { hwnd: 0x1234_5678, pid: 4242, original: false };
        let b = Saved { hwnd: -5, pid: 1, original: true };
        let text = format_saved(&a) + &format_saved(&b) + "garbage line
1 2 7
";
        assert_eq!(parse_saved(&text), vec![a, b]);
        assert!(parse_saved("").is_empty());
    }

    #[test]
    fn pending_records_several_windows() {
        let a = Saved { hwnd: 1, pid: 10, original: false };
        let b = Saved { hwnd: 2, pid: 20, original: false };
        let a2 = Saved { hwnd: 1, pid: 11, original: true };
        let l = with_record(&with_record(&[], a), b);
        assert_eq!(l, vec![a, b]);
        assert_eq!(with_record(&l, a2), vec![b, a2], "one record per window");
        assert_eq!(without_record(&l, &a), vec![b]);
        assert!(without_record(&without_record(&l, &a), &b).is_empty());
        let text: String = l.iter().map(format_saved).collect();
        assert_eq!(parse_saved(&text), l, "all of them survive a crash");
    }

    #[test]
    fn ease_in_out_curve() {
        assert!(bezier(0.0, EASE).abs() < 1e-4);
        assert!((bezier(1.0, EASE) - 1.0).abs() < 1e-4);
        assert!((bezier(0.5, EASE) - 0.5).abs() < 1e-3);
        // Slow start, slow end, monotonic.
        assert!(bezier(0.1, EASE) < 0.1 && bezier(0.9, EASE) > 0.9);
        let mut last = 0.0;
        for k in 0..=100 {
            let y = bezier(k as f32 / 100.0, EASE);
            assert!(y >= last - 1e-5);
            last = y;
        }
    }
}
