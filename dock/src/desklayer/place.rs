//! Where a desktop widget is (pure): remembered relative to its display's
//! work area in DIP, so it comes back on that display at the same spot
//! whatever its scale, and is kept fully on screen when displays change.

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::RECT;

use crate::monitors::Monitor;

/// A widget's place.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(default)]
pub struct Place {
    /// The display it was on: that display's rectangle (px) when saved
    /// (how it is told apart from the others next time).
    pub mon: [i32; 4],
    /// Left and top from the display's work area's top left, width and
    /// height, in DIP.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Default for Place {
    fn default() -> Self {
        Place { mon: [0; 4], x: 32.0, y: 32.0, w: 240.0, h: 200.0 }
    }
}

pub fn r4(r: RECT) -> [i32; 4] {
    [r.left, r.top, r.right, r.bottom]
}

/// Pure: the display a place belongs on: the one with the saved
/// rectangle, else one at the same top left (resized), else the primary.
pub fn pick(saved: [i32; 4], mons: &[Monitor]) -> Option<&Monitor> {
    connected(saved, mons)
        .or_else(|| mons.iter().find(|m| m.primary))
        .or_else(|| mons.first())
}

/// Saved monitor identity only. A disconnected monitor has no primary fallback.
pub fn connected(saved: [i32; 4], mons: &[Monitor]) -> Option<&Monitor> {
    mons.iter().find(|m| r4(m.rect) == saved)
        .or_else(|| mons.iter().find(|m| m.rect.left == saved[0] && m.rect.top == saved[1]))
}
/// A queued geometry action must still belong to a connected, stable layout.
pub fn geometry_allowed(saved: Option<Place>, mons: &[Monitor], settling: bool) -> bool {
    !settling && saved.is_none_or(|p| connected(p.mon, mons).is_some())
}

/// Pure: `r` moved (and shrunk if it must be) to lie fully inside `work`.
pub fn clamp(r: RECT, work: RECT) -> RECT {
    let ww = (work.right - work.left).max(1);
    let wh = (work.bottom - work.top).max(1);
    let w = (r.right - r.left).clamp(1, ww);
    let h = (r.bottom - r.top).clamp(1, wh);
    let left = r.left.clamp(work.left, work.right - w);
    let top = r.top.clamp(work.top, work.bottom - h);
    RECT { left, top, right: left + w, bottom: top + h }
}

/// Pure: where `p` is on screen now (px), at least `min` DIP big, inside
/// its display's work area. None when the saved display is disconnected.
pub fn to_px(p: &Place, mons: &[Monitor], min: (f32, f32)) -> Option<RECT> {
    let m = connected(p.mon, mons)?;
    let s = m.scale();
    let fin = |v: f32, d: f32| if v.is_finite() { v } else { d };
    let w = (fin(p.w, 240.0).max(min.0) * s).round() as i32;
    let h = (fin(p.h, 200.0).max(min.1) * s).round() as i32;
    let left = m.work.left + (fin(p.x, 0.0) * s).round() as i32;
    let top = m.work.top + (fin(p.y, 0.0) * s).round() as i32;
    Some(clamp(RECT { left, top, right: left + w, bottom: top + h }, m.work))
}

/// Pure: where `p` is on screen now (px) for a widget placed against its
/// whole display, not its work area (a piece cut from the screen: it may
/// sit over the taskbar's place): its own size (at least 1 px), only moved
/// (or shrunk) to lie inside the display.
pub fn to_px_screen(p: &Place, mons: &[Monitor]) -> Option<RECT> {
    let m = pick(p.mon, mons)?;
    let s = m.scale();
    let fin = |v: f32, d: f32| if v.is_finite() { v } else { d };
    let w = ((fin(p.w, 240.0) * s).round() as i32).max(1);
    let h = ((fin(p.h, 200.0) * s).round() as i32).max(1);
    let left = m.work.left + (fin(p.x, 0.0) * s).round() as i32;
    let top = m.work.top + (fin(p.y, 0.0) * s).round() as i32;
    Some(clamp(RECT { left, top, right: left + w, bottom: top + h }, m.rect))
}

/// Pure: how much of `a` and `b` overlap (px²).
fn overlap(a: RECT, b: RECT) -> i64 {
    let w = (a.right.min(b.right) - a.left.max(b.left)).max(0) as i64;
    let h = (a.bottom.min(b.bottom) - a.top.max(b.top)).max(0) as i64;
    w * h
}

/// Pure: the place of a widget at `r` (px): on the display it covers
/// most (the primary if none).
pub fn from_px(r: RECT, mons: &[Monitor]) -> Option<Place> {
    let m = mons
        .iter()
        .max_by_key(|m| (overlap(r, m.rect), m.primary))
        .filter(|m| overlap(r, m.rect) > 0)
        .or_else(|| mons.iter().find(|m| m.primary))
        .or_else(|| mons.first())?;
    let s = m.scale();
    Some(Place {
        mon: r4(m.rect),
        x: (r.left - m.work.left) as f32 / s,
        y: (r.top - m.work.top) as f32 / s,
        w: (r.right - r.left) as f32 / s,
        h: (r.bottom - r.top) as f32 / s,
    })
}

/// Pure: a new widget's spot next to the one at `r` (same size, `gap` px
/// apart): to its right, else its left, below, above; inside `work`.
pub fn beside(r: RECT, work: RECT, gap: i32) -> RECT {
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let fits = |c: RECT| c.left >= work.left && c.top >= work.top && c.right <= work.right && c.bottom <= work.bottom;
    let at = |left: i32, top: i32| RECT { left, top, right: left + w, bottom: top + h };
    let tries = [at(r.right + gap, r.top), at(r.left - gap - w, r.top), at(r.left, r.bottom + gap), at(r.left, r.top - gap - h)];
    tries.into_iter().find(|c| fits(*c)).unwrap_or_else(|| clamp(at(r.left + gap * 2, r.top + gap * 2), work))
}

/// Pure: the place of the `n`th widget made with nothing to sit beside:
/// the top right of `m`'s work area, `margin` DIP in, each next one a
/// step down and left.
pub fn first_on(m: &Monitor, w: f32, h: f32, margin: f32, n: usize) -> Place {
    let s = m.scale();
    let ww = (m.work.right - m.work.left) as f32 / s;
    let step = 24.0 * (n % 8) as f32;
    Place { mon: r4(m.rect), x: (ww - w - margin - step).max(0.0), y: margin + step, w, h }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(l: i32, t: i32, w: i32, h: i32, dpi: u32, primary: bool) -> Monitor {
        let rect = RECT { left: l, top: t, right: l + w, bottom: t + h };
        // A bar of 40 px at the bottom.
        let work = RECT { bottom: t + h - 40, ..rect };
        Monitor { handle: l as isize + 1, rect, work, primary, dpi }
    }

    fn two() -> Vec<Monitor> {
        vec![mon(0, 0, 1920, 1080, 96, true), mon(1920, 0, 2560, 1440, 144, false)]
    }

    #[test]
    fn a_screen_piece_comes_back_where_it_was_cut() {
        let mons = two();
        // Over the taskbar's place (the work area ends at 1040).
        let r = RECT { left: 100, top: 1030, right: 300, bottom: 1070 };
        let p = from_px(r, &mons).unwrap();
        assert_eq!(to_px_screen(&p, &mons), Some(r), "not pushed into the work area");
        assert_ne!(to_px(&p, &mons, (1.0, 1.0)), Some(r), "(what a work-area widget would do)");
        // The whole display, its size kept.
        let full = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        assert_eq!(to_px_screen(&from_px(full, &mons).unwrap(), &mons), Some(full));
        // 8 px at 250 %: stays 8 px (no minimum in DIP to grow it).
        let hi = vec![mon(0, 0, 3840, 2160, 240, true)];
        let tiny = RECT { left: 500, top: 2140, right: 508, bottom: 2148 };
        assert_eq!(to_px_screen(&from_px(tiny, &hi).unwrap(), &hi), Some(tiny));
        // A display that went away: on the primary, inside it.
        let gone = Place { mon: [9000, 0, 9100, 100], x: 5000.0, y: 10.0, w: 100.0, h: 50.0 };
        let back = to_px_screen(&gone, &mons).unwrap();
        assert!(back.left >= 0 && back.right <= 1920 && back.bottom <= 1080);
    }

    #[test]
    fn a_place_round_trips_on_its_display() {
        let mons = two();
        let r = RECT { left: 2020, top: 150, right: 2380, bottom: 450 };
        let p = from_px(r, &mons).unwrap();
        assert_eq!(p.mon, [1920, 0, 4480, 1440]);
        // 150 % scale: 100 px from the left edge is 66.7 DIP.
        assert!((p.x - 100.0 / 1.5).abs() < 1e-3 && (p.w - 240.0).abs() < 1e-3);
        assert_eq!(to_px(&p, &mons, (100.0, 80.0)), Some(r));
    }

    #[test]
    fn review_missing_display_keeps_place_until_it_returns() {
        let p = Place { mon: [1920, 0, 4480, 1440], x: 2000.0, y: 1300.0, w: 240.0, h: 200.0 };
        let only = vec![mon(0, 0, 1920, 1080, 96, true)];
        assert_eq!(to_px(&p, &only, (100.0, 80.0)), None);
        let original = to_px(&p, &two(), (100.0, 80.0));
        assert!(original.is_some());
        assert_eq!(p.mon, [1920, 0, 4480, 1440]);
        assert_eq!(to_px(&p, &two(), (100.0, 80.0)), original);
        assert!(!geometry_allowed(Some(p), &only, false));
        assert!(!geometry_allowed(Some(p), &two(), true));
        assert!(geometry_allowed(Some(p), &two(), false));
    }

    #[test]
    fn a_resized_display_is_still_found_by_its_corner() {
        let p = Place { mon: [1920, 0, 4480, 1440], x: 10.0, y: 10.0, w: 200.0, h: 100.0 };
        let mons = vec![mon(0, 0, 1920, 1080, 96, true), mon(1920, 0, 1920, 1080, 96, false)];
        assert_eq!(pick(p.mon, &mons).unwrap().rect.left, 1920);
        assert_eq!(to_px(&p, &mons, (100.0, 80.0)).unwrap().left, 1930);
    }

    #[test]
    fn clamping_keeps_it_whole_and_shrinks_what_cannot_fit() {
        let work = RECT { left: 0, top: 0, right: 800, bottom: 600 };
        assert_eq!(clamp(RECT { left: -50, top: 550, right: 150, bottom: 700 }, work), RECT { left: 0, top: 450, right: 200, bottom: 600 });
        assert_eq!(clamp(RECT { left: 10, top: 10, right: 2000, bottom: 50 }, work), RECT { left: 0, top: 10, right: 800, bottom: 50 });
        // Garbage sizes come back as something usable.
        let p = Place { w: f32::NAN, x: f32::INFINITY, ..Place::default() };
        let r = to_px(&p, &[mon(0, 0, 800, 640, 96, true)], (100.0, 80.0)).unwrap();
        assert!(r.right - r.left == 240 && r.right <= 800);
    }

    #[test]
    fn small_sizes_grow_to_the_minimum() {
        let p = Place { mon: [0, 0, 1920, 1080], x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        let r = to_px(&p, &[mon(0, 0, 1920, 1080, 192, true)], (140.0, 90.0)).unwrap();
        assert_eq!((r.right - r.left, r.bottom - r.top), (280, 180));
    }

    #[test]
    fn a_new_one_goes_beside_then_around() {
        let work = RECT { left: 0, top: 0, right: 1000, bottom: 800 };
        let r = RECT { left: 100, top: 100, right: 300, bottom: 300 };
        assert_eq!(beside(r, work, 12).left, 312);
        let right_edge = RECT { left: 800, top: 100, right: 1000, bottom: 300 };
        assert_eq!(beside(right_edge, work, 12).right, 788);
        let full_width = RECT { left: 0, top: 100, right: 1000, bottom: 300 };
        assert_eq!(beside(full_width, work, 12).top, 312);
        let everything = RECT { left: 0, top: 0, right: 1000, bottom: 800 };
        let c = beside(everything, work, 12);
        assert_eq!(clamp(c, work), c);
    }

    #[test]
    fn the_first_ones_start_top_right_and_step_down() {
        let m = mon(0, 0, 1920, 1080, 96, true);
        let a = first_on(&m, 240.0, 200.0, 32.0, 0);
        let b = first_on(&m, 240.0, 200.0, 32.0, 1);
        assert_eq!((a.x, a.y), (1920.0 - 240.0 - 32.0, 32.0));
        assert!(b.x < a.x && b.y > a.y);
    }
}
