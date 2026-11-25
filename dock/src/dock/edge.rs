//! Screen-edge geometry. Layout is computed once in a canonical frame and
//! mapped onto the window for the dock's edge:
//! - `u` runs along the dock (left->right on the bottom, top->bottom on the
//!   sides),
//! - `v` runs across it, with `v = depth` at the screen edge and `v = 0` at
//!   the far side of the window (where labels go).
//!
//! The bottom dock is the identity mapping; the side docks swap axes.

use super::Rect;
use crate::config::Position;
use windows::Win32::Foundation::RECT;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Frame {
    pub pos: Position,
    /// Window extent across the dock (DIP).
    pub depth: f32,
}

impl Frame {
    pub fn vertical(&self) -> bool {
        self.pos != Position::Bottom
    }

    /// Canonical rect -> window DIP rect.
    pub fn to_win(&self, r: Rect) -> Rect {
        match self.pos {
            Position::Bottom => r,
            Position::Left => Rect { x: self.depth - r.y - r.h, y: r.x, w: r.h, h: r.w },
            Position::Right => Rect { x: r.y, y: r.x, w: r.h, h: r.w },
        }
    }

    /// Window DIP point -> canonical (u, v).
    pub fn from_win(&self, x: f32, y: f32) -> (f32, f32) {
        match self.pos {
            Position::Bottom => (x, y),
            Position::Left => (y, self.depth - x),
            Position::Right => (y, x),
        }
    }

    /// Window DIP offset that moves the dock `d` toward (and past) the
    /// screen edge, for auto-hide.
    pub fn away(&self, d: f32) -> (f32, f32) {
        match self.pos {
            Position::Bottom => (0.0, d),
            Position::Left => (-d, 0.0),
            Position::Right => (d, 0.0),
        }
    }

    /// Top-left (window DIP) of an upright `w` x `h` label that sits `gap`
    /// beyond the canonical point (`u`, `v`) on the far side of the dock.
    pub fn label_at(&self, u: f32, v: f32, w: f32, h: f32, gap: f32) -> (f32, f32) {
        match self.pos {
            Position::Bottom => (u - w / 2.0, v - gap - h),
            Position::Left => (self.depth - v + gap, u - h / 2.0),
            Position::Right => (v - gap - w, u - h / 2.0),
        }
    }

    /// The window's px rect on a work area for a window `depth_px` deep.
    pub fn window_rect(&self, area: RECT, depth_px: i32) -> RECT {
        // One definition, also used by placement.rs's tests.
        crate::placement::dock_rect(self.pos, area, depth_px)
    }
}

/// The thin strip on `area`'s `pos` edge, `len` px long and centered,
/// `thick` px deep: where a dock on that monitor would be touched.
pub(super) fn sensor_rect(pos: Position, area: RECT, len: i32, thick: i32) -> RECT {
    let cx = (area.left + area.right) / 2;
    let cy = (area.top + area.bottom) / 2;
    let along = |c: i32, lo: i32, hi: i32| {
        let l = len.min(hi - lo).max(1);
        let a = (c - l / 2).max(lo);
        (a, a + l)
    };
    match pos {
        Position::Bottom => {
            let (x0, x1) = along(cx, area.left, area.right);
            RECT { left: x0, top: area.bottom - thick, right: x1, bottom: area.bottom }
        }
        Position::Left => {
            let (y0, y1) = along(cy, area.top, area.bottom);
            RECT { left: area.left, top: y0, right: area.left + thick, bottom: y1 }
        }
        Position::Right => {
            let (y0, y1) = along(cy, area.top, area.bottom);
            RECT { left: area.right - thick, top: y0, right: area.right, bottom: y1 }
        }
    }
}

/// True if a window rect covers the whole monitor (games, video, F11).
/// Maximized windows are excluded by the caller (they can also cover the
/// monitor once the taskbar is gone).
pub(super) fn covers(win: RECT, mon: RECT) -> bool {
    mon.right > mon.left && win.left <= mon.left && win.top <= mon.top && win.right >= mon.right && win.bottom >= mon.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    fn eq(a: Rect, b: Rect) -> bool {
        (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4 && (a.w - b.w).abs() < 1e-4 && (a.h - b.h).abs() < 1e-4
    }

    #[test]
    fn bottom_is_identity() {
        let f = Frame { pos: Position::Bottom, depth: 150.0 };
        assert!(eq(f.to_win(r(10.0, 20.0, 30.0, 40.0)), r(10.0, 20.0, 30.0, 40.0)));
        assert_eq!(f.from_win(5.0, 6.0), (5.0, 6.0));
    }

    #[test]
    fn left_puts_the_edge_at_x0() {
        let f = Frame { pos: Position::Left, depth: 150.0 };
        // A bar touching the edge (v from 80 to 150) sits at x 0..70.
        let w = f.to_win(r(100.0, 80.0, 400.0, 70.0));
        assert!(eq(w, r(0.0, 100.0, 70.0, 400.0)), "{w:?}");
        // Round trip of a point.
        let (u, v) = f.from_win(10.0, 300.0);
        assert_eq!((u, v), (300.0, 140.0));
    }

    #[test]
    fn right_puts_the_edge_at_depth() {
        let f = Frame { pos: Position::Right, depth: 150.0 };
        let w = f.to_win(r(100.0, 80.0, 400.0, 70.0));
        assert!(eq(w, r(80.0, 100.0, 70.0, 400.0)), "{w:?}");
        assert_eq!(f.from_win(140.0, 300.0), (300.0, 140.0));
    }

    #[test]
    fn mapping_and_inverse_agree() {
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            let f = Frame { pos, depth: 150.0 };
            // A 1x1 rect's window corner maps back to its canonical corner
            // (up to the rect's own extent on the flipped axis).
            let w = f.to_win(r(37.0, 90.0, 0.0, 0.0));
            assert_eq!(f.from_win(w.x, w.y), (37.0, 90.0), "{pos:?}");
        }
    }

    #[test]
    fn labels_go_on_the_far_side() {
        let b = Frame { pos: Position::Bottom, depth: 150.0 };
        assert_eq!(b.label_at(100.0, 60.0, 40.0, 20.0, 8.0), (80.0, 32.0));
        let l = Frame { pos: Position::Left, depth: 150.0 };
        // Icon far side at v=60 -> window x 90; label starts 8 past it.
        assert_eq!(l.label_at(100.0, 60.0, 40.0, 20.0, 8.0), (98.0, 90.0));
        let rt = Frame { pos: Position::Right, depth: 150.0 };
        assert_eq!(rt.label_at(100.0, 60.0, 40.0, 20.0, 8.0), (12.0, 90.0));
    }

    #[test]
    fn window_rect_hugs_the_edge() {
        let area = RECT { left: 0, top: 0, right: 2560, bottom: 1600 };
        let b = Frame { pos: Position::Bottom, depth: 0.0 }.window_rect(area, 150);
        assert_eq!((b.left, b.top, b.right, b.bottom), (0, 1450, 2560, 1600));
        let l = Frame { pos: Position::Left, depth: 0.0 }.window_rect(area, 150);
        assert_eq!((l.left, l.top, l.right, l.bottom), (0, 0, 150, 1600));
        let rt = Frame { pos: Position::Right, depth: 0.0 }.window_rect(area, 150);
        assert_eq!((rt.left, rt.top, rt.right, rt.bottom), (2410, 0, 2560, 1600));
    }

    #[test]
    fn sensors_sit_on_the_edge_centered() {
        // A second monitor left of the primary, negative coordinates.
        let area = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        let b = sensor_rect(Position::Bottom, area, 600, 2);
        assert_eq!((b.left, b.top, b.right, b.bottom), (-1260, 1078, -660, 1080));
        let l = sensor_rect(Position::Left, area, 600, 2);
        assert_eq!((l.left, l.top, l.right, l.bottom), (-1920, 240, -1918, 840));
        let r = sensor_rect(Position::Right, area, 600, 2);
        assert_eq!((r.left, r.right), (-2, 0));
        // Longer than the monitor: clipped to it.
        let w = sensor_rect(Position::Bottom, area, 5000, 2);
        assert_eq!((w.left, w.right), (-1920, 0));
    }

    #[test]
    fn over_a_bottom_bar_the_dock_and_its_sensor_end_on_the_bar() {
        // The top bar moved to the bottom (64 px at 200 %), the taskbar
        // hidden: the window, its auto-hide strip and the follow sensor all
        // sit right above the bar, none on it.
        let mon = RECT { left: 0, top: 0, right: 2560, bottom: 1600 };
        let bar = crate::placement::topbar_rect(mon, &[], true, 64, true);
        let space = crate::placement::BarSpace { top: 0, bottom: mon.bottom - bar.top, bottom_free: 0 };
        let area = crate::placement::dock_area(mon, mon, &[], true, space);
        let r = Frame { pos: Position::Bottom, depth: 0.0 }.window_rect(area, 150);
        assert_eq!((r.top, r.bottom), (1386, 1536));
        assert_eq!(r.bottom, bar.top);
        let s = sensor_rect(Position::Bottom, area, 600, 1);
        assert_eq!((s.top, s.bottom), (bar.top - 1, bar.top));
        // Side docks end above it too.
        let l = Frame { pos: Position::Left, depth: 0.0 }.window_rect(area, 150);
        assert_eq!((l.top, l.bottom), (0, bar.top));
    }

    #[test]
    fn fullscreen_cover() {
        let mon = RECT { left: 0, top: 0, right: 2560, bottom: 1600 };
        assert!(covers(mon, mon));
        assert!(covers(RECT { left: -8, top: -8, right: 2568, bottom: 1608 }, mon));
        assert!(!covers(RECT { left: 0, top: 0, right: 2560, bottom: 1500 }, mon));
        assert!(!covers(mon, RECT::default()));
    }
}
