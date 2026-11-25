//! Arranging Launchpad like a phone's home screen (pure): where a dragged
//! icon would go, how the rest make room, picking up and putting down with
//! the overflow pushed on to the next page, empty pages going, the arrow
//! keys in edit mode, and the edit mode's numbers.
//!
//! Edit mode is calm (Android / One UI, not iOS): nothing wiggles; the
//! page's grid glides a little smaller on a soft plate, and every intent
//! shows at once where the pointer is: the middle of an icon means "make a
//! folder" (the target grows, a plate grows behind it), anywhere else
//! means "go here" (a faint copy marks the place, the others make room
//! once the pointer stays), a side edge means "next page" (the edge
//! lights).
//!
//! No length or curve is edit mode's own (design system v2.1, 7.1 and
//! 7.4): what changes place or size rides crate::motion's GLIDE, what
//! comes and goes fades 90 / 120; not animating, nothing moves or grows
//! and 60 ms straight fades are all there is (12.2).
//!
//! A drag takes the item out of the pages when it starts (its page stays,
//! maybe empty, so page numbers hold still) and puts it back where it is
//! dropped; a cancelled drag puts the pages back as they were.

use super::grid::{Arrow, Grid};
use super::layout_store::Pages;
use crate::config::LpItem;

/// Holding still on an icon this long starts edit mode (and lifts it).
pub const LONG_PRESS_MS: u32 = 500;

// ---- edit mode's look ----
/// The page's grid glides to this scale (around its middle) on a plate
/// this far around it (a big plate: the theme's sheet corners). Not
/// animating (small animations off, reduced motion, high contrast): no
/// scale, the plate and Done only.
pub const EDIT_SCALE: f32 = 0.92;
pub const PANEL_MARGIN: f32 = crate::theme::metrics::space::S5;

// ---- the picked icon ----
/// Picked up, it is this much bigger than it showed in its cell (2.6:
/// dragging lifts a thing to 1.04 and to elevation E3) and follows the
/// pointer 1:1 (its position is set on every move: no animation, no lag).
pub const LIFT_SCALE: f32 = 1.04;
/// Where it would go (first: the slot it left) shows it this faintly: the
/// one opacity of what cannot be used now.
pub const GHOST_OPACITY: f32 = crate::theme::DISABLED_OPACITY;
/// The grid behind an open folder fades to this (the folder's own value:
/// the tokens have none for it).
pub const BEHIND_FOLDER: f32 = 0.18;

// ---- folder intent ----
/// Only inside the target's icon box, its inner share this wide and high
/// (a square around its middle).
pub const FOLDER_ZONE: f32 = 0.52;
/// Shown at once: the target grows to TARGET_SCALE (a small thing's
/// state: SNAP) and a plate fades in behind it.
pub const TARGET_SCALE: f32 = 1.1;
/// The plate: this big against the icon, growing from PLATE_FROM of that
/// over FOLDER_DWELL_MS (the progress, straight: it shows a wait). Dropping
/// makes the folder once it is full: a quick sweep across icons makes
/// none. Leaving the zone ends it.
pub const PLATE: f32 = 1.34;
pub const PLATE_FROM: f32 = 0.7;
pub const FOLDER_DWELL_MS: u32 = 220;

// ---- reorder intent ----
/// Anywhere else on a cell: before or after it by which half of the cell
/// the pointer is in. The others make room once the pointer has stayed
/// with the same place SETTLE_MS (not on every pixel), gliding there
/// (GLIDE). Dropping goes where the pointer says, settled or not.
pub const SETTLE_MS: u32 = 100;

// ---- edge page turn ----
/// This close to the screen's side edges the edge lights at once; the page
/// turns after EDGE_MS, and again every EDGE_MS while it stays.
pub const EDGE: f32 = 48.0;
pub const EDGE_MS: u32 = 450;

// ---- the open folder's panel ----
/// Dragging an app this close inside its border lights the border (one
/// step more and the app is out of the folder).
pub const PANEL_EXIT: f32 = 28.0;

/// A place on the pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pos {
    pub page: usize,
    pub slot: usize,
}

impl Pos {
    pub fn new(page: usize, slot: usize) -> Self {
        Pos { page, slot }
    }
}

/// Takes the item at `at` out (its page stays, maybe empty).
pub fn take(pages: &mut Pages, at: Pos) -> Option<LpItem> {
    let page = pages.get_mut(at.page)?;
    (at.slot < page.len()).then(|| page.remove(at.slot))
}

/// Puts `item` at `at`: the slot at most the page's length (no holes), a
/// page past the last one is a new last page. A page that gets more than
/// `per_page` items pushes its last one to the front of the next page (and
/// so on). Where it landed.
pub fn put(pages: &mut Pages, at: Pos, item: LpItem, per_page: usize) -> Pos {
    let pp = per_page.max(1);
    let page = at.page.min(pages.len());
    if page == pages.len() {
        pages.push(Vec::new());
    }
    let slot = at.slot.min(pages[page].len()).min(pp - 1);
    pages[page].insert(slot, item);
    let mut p = page;
    while pages[p].len() > pp {
        let over = pages[p].pop().expect("longer than per_page");
        if p + 1 == pages.len() {
            pages.push(Vec::new());
        }
        pages[p + 1].insert(0, over);
        p += 1;
    }
    Pos::new(page, slot)
}

/// Drops the empty pages (one stays if all are). `keep` is a place on the
/// pages before: where it is after (None if its page went).
pub fn tidy(pages: &mut Pages, keep: Option<Pos>) -> Option<Pos> {
    let mut out: Pages = Vec::new();
    let mut moved = None;
    for (i, p) in std::mem::take(pages).into_iter().enumerate() {
        if p.is_empty() {
            continue;
        }
        if let Some(k) = keep.filter(|k| k.page == i) {
            moved = Some(Pos::new(out.len(), k.slot.min(p.len() - 1)));
        }
        out.push(p);
    }
    if out.is_empty() {
        out.push(Vec::new());
    }
    *pages = out;
    moved
}

/// Pages to show while dragging: one more, empty, after the last when the
/// last has anything on it (dropping there makes it).
pub fn drag_pages(pages: &Pages) -> usize {
    pages.len() + usize::from(pages.last().is_some_and(|p| !p.is_empty()))
}

/// Where on a cell the pointer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    /// The first half (left; right to left: right) beside the middle.
    Before,
    Middle,
    After,
}

/// The zone of a point `dx`, `dy` DIP from an icon's centre (`icon` its
/// edge; the icon sits in the middle of its cell): the middle square
/// (FOLDER_ZONE of the icon box), else the half of the cell it is in.
pub fn zone(dx: f32, dy: f32, icon: f32, rtl: bool) -> Zone {
    let half = icon * FOLDER_ZONE / 2.0;
    if dx.abs() <= half && dy.abs() <= half {
        return Zone::Middle;
    }
    if (dx < 0.0) != rtl { Zone::Before } else { Zone::After }
}

/// What dropping now would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aim {
    /// Go in at this index of the page's items (without the dragged one):
    /// the gap the others make room for.
    Gap(usize),
    /// Onto the item at this index (a folder is made, or it goes in one).
    Onto(usize),
}

/// The aim for the pointer over shown slot `shown` (and where on it), with
/// the gap at `gap` now and `n` items on the page besides the dragged one.
/// Over the gap itself, or off every cell: the gap stays. Past the last
/// item: the end.
pub fn aim(shown: Option<(usize, Zone)>, gap: usize, n: usize) -> Aim {
    let gap = gap.min(n);
    let Some((s, z)) = shown else { return Aim::Gap(gap) };
    if s == gap {
        return Aim::Gap(gap);
    }
    let i = if s < gap { s } else { s - 1 };
    if i >= n {
        return Aim::Gap(n);
    }
    match z {
        Zone::Middle => Aim::Onto(i),
        Zone::Before => Aim::Gap(i),
        Zone::After => Aim::Gap(i + 1),
    }
}

/// The shown slot of item `k` with the gap at `gap` (None: no gap).
pub fn shown_slot(k: usize, gap: Option<usize>) -> usize {
    match gap {
        Some(g) if k >= g => k + 1,
        _ => k,
    }
}

/// Which item (index) is shown in slot `s` with the gap at `gap`; None for
/// the gap.
pub fn item_in_slot(s: usize, gap: Option<usize>) -> Option<usize> {
    match gap {
        Some(g) if s == g => None,
        Some(g) if s > g => Some(s - 1),
        _ => Some(s),
    }
}

/// Is `x` (DIP across the screen, `w` wide) at a side edge while dragging?
/// Some(true) for the edge that turns to the next page (right; left right
/// to left).
pub fn edge(x: f32, w: f32, rtl: bool) -> Option<bool> {
    let right = if x >= w - EDGE {
        true
    } else if x <= EDGE {
        false
    } else {
        return None;
    };
    Some(right != rtl)
}

/// Arrow key on the item at `at` with `lens` items on each page: the item
/// it goes to (left / right through the pages, mirrored right to left; up
/// / down a row on the page, into a short row's last item).
pub fn step(at: Pos, lens: &[usize], g: &Grid, key: Arrow, rtl: bool) -> Option<Pos> {
    let len = *lens.get(at.page)?;
    if len == 0 {
        return None;
    }
    let slot = at.slot.min(len - 1);
    let cols = g.cols.max(1);
    let forward = match key {
        Arrow::Right => !rtl,
        Arrow::Left => rtl,
        Arrow::Up | Arrow::Down => {
            let row = slot / cols;
            let to_row = if key == Arrow::Down { row + 1 } else { row.checked_sub(1)? };
            if to_row >= g.rows || to_row * cols >= len {
                return None;
            }
            return Some(Pos::new(at.page, (to_row * cols + slot % cols).min(len - 1)));
        }
    };
    if forward {
        if slot + 1 < len {
            return Some(Pos::new(at.page, slot + 1));
        }
        (at.page + 1..lens.len()).find(|&p| lens[p] > 0).map(|p| Pos::new(p, 0))
    } else {
        if slot > 0 {
            return Some(Pos::new(at.page, slot - 1));
        }
        (0..at.page).rev().find(|&p| lens[p] > 0).map(|p| Pos::new(p, lens[p] - 1))
    }
}

/// Edit mode, an icon lifted from the keyboard: arrow `key` moves it one
/// place (left / right across pages, onto a new last page past the end;
/// up / down a row on its page). Empty pages go. Where it is after, None if
/// it cannot go that way.
pub fn shift(pages: &mut Pages, at: Pos, g: &Grid, key: Arrow, rtl: bool, per_page: usize) -> Option<Pos> {
    let len = pages.get(at.page)?.len();
    if at.slot >= len {
        return None;
    }
    let cols = g.cols.max(1);
    let pp = per_page.max(1);
    let to = match key {
        Arrow::Up | Arrow::Down => {
            let row = at.slot / cols;
            let to_row = if key == Arrow::Down { row + 1 } else { row.checked_sub(1)? };
            if to_row >= g.rows || to_row * cols >= len {
                return None;
            }
            Pos::new(at.page, (to_row * cols + at.slot % cols).min(len - 1))
        }
        _ => {
            let forward = (key == Arrow::Right) != rtl;
            if forward {
                if at.slot + 1 < len {
                    Pos::new(at.page, at.slot + 1)
                } else if at.page + 1 < pages.len() {
                    Pos::new(at.page + 1, 0)
                } else if len > 1 {
                    // Past the end: a new last page.
                    Pos::new(pages.len(), 0)
                } else {
                    return None;
                }
            } else if at.slot > 0 {
                Pos::new(at.page, at.slot - 1)
            } else if at.page > 0 {
                // The end of the page before (before its last when it is
                // full: that one is pushed on to this page).
                let prev = pages[at.page - 1].len();
                Pos::new(at.page - 1, prev.min(pp - 1))
            } else {
                return None;
            }
        }
    };
    let item = take(pages, at)?;
    let landed = put(pages, to, item, pp);
    tidy(pages, Some(landed))
}

/// Edit mode's grid scale: EDIT_SCALE when Launchpad animates (`animate`:
/// its effect, Windows' animations, not high contrast) and the small
/// animations are on, else 1 (the outline alone shows edit mode).
pub fn edit_scale(animate: bool, small: bool) -> f32 {
    if animate && small { EDIT_SCALE } else { 1.0 }
}

/// Pure: how far along a GLIDE is at time share `t` (0..=1) of its run:
/// crate::motion's spring sampled as the Composition animations have it
/// (SAMPLES keys over GLIDE_MAX_MS, straight between).
pub fn glide_at(t: f32) -> f32 {
    use crate::motion::{GLIDE_MAX_MS, SAMPLES, SPRING_GLIDE};
    if !t.is_finite() || t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let keys = SPRING_GLIDE.keys(GLIDE_MAX_MS, SAMPLES);
    let i = keys.iter().rposition(|k| k.0 <= t).unwrap_or(0).min(keys.len() - 2);
    let ((p0, d0), (p1, d1)) = (keys[i], keys[i + 1]);
    d0 + (d1 - d0) * ((t - p0) / (p1 - p0)).clamp(0.0, 1.0)
}

/// The grid's scale on screen `elapsed` ms into its glide from `from` to
/// `to` that plays over `len` ms (input and screen use the same at every
/// moment).
pub fn shown_scale(from: f32, to: f32, elapsed: f32, len: f32) -> f32 {
    if !(len > 0.0) {
        return to;
    }
    from + (to - from) * glide_at(elapsed / len)
}

/// A point on screen in the grid's own coordinates, the grid shown scaled
/// by `k` around `c`.
pub fn unscale(p: (f32, f32), c: (f32, f32), k: f32) -> (f32, f32) {
    let k = if k > 0.0 { k } else { 1.0 };
    (c.0 + (p.0 - c.0) / k, c.1 + (p.1 - c.1) / k)
}

/// A point of the grid where it shows on screen (the inverse of unscale).
pub fn scale(p: (f32, f32), c: (f32, f32), k: f32) -> (f32, f32) {
    (c.0 + (p.0 - c.0) * k, c.1 + (p.1 - c.1) * k)
}

/// What the reorder intent does with gap `gap`, a move waiting for
/// `pending`, and the pointer now aiming at `aimed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settle {
    /// As it is (waiting on for the same place, or nothing to move).
    Keep,
    /// A new place: wait SETTLE_MS from now, then move the gap there.
    Arm(usize),
    /// Back to where the gap is: nothing waits.
    Clear,
}

pub fn settle(gap: usize, pending: Option<usize>, aimed: usize) -> Settle {
    if aimed == gap {
        if pending.is_some() { Settle::Clear } else { Settle::Keep }
    } else if pending == Some(aimed) {
        Settle::Keep
    } else {
        Settle::Arm(aimed)
    }
}

/// What the folder intent does with the target held now (`current`) and
/// the one the pointer is in the middle of (`aimed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    Keep,
    /// A new target: shown at once, ready after FOLDER_DWELL_MS.
    Start(usize),
    /// Out of every middle: the target goes at once.
    Clear,
}

pub fn hold(current: Option<usize>, aimed: Option<usize>) -> Hold {
    match (current, aimed) {
        (c, Some(a)) if c == Some(a) => Hold::Keep,
        (_, Some(a)) => Hold::Start(a),
        (Some(_), None) => Hold::Clear,
        (None, None) => Hold::Keep,
    }
}

/// Where a point is against the open folder's panel `r` (l, t, r, b).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Border {
    Inside,
    /// Within PANEL_EXIT inside its border: the border lights.
    Near,
    /// Out of the panel: out of the folder.
    Out,
}

pub fn border(r: (f32, f32, f32, f32), x: f32, y: f32) -> Border {
    let (l, t, rr, b) = r;
    if !(x >= l && x < rr && y >= t && y < b) {
        return Border::Out;
    }
    let d = (x - l).min(rr - x).min(y - t).min(b - y);
    if d <= PANEL_EXIT { Border::Near } else { Border::Inside }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str) -> LpItem {
        LpItem::App(id.into())
    }
    fn page(ids: &[&str]) -> Vec<LpItem> {
        ids.iter().map(|i| app(i)).collect()
    }
    fn grid(cols: usize, rows: usize) -> Grid {
        Grid { cols, rows, left: 0.0, top: 0.0, cell_w: 100.0, cell_h: 100.0, label_h: super::super::grid::LABEL_H, icon_edge: super::super::grid::ICON }
    }

    #[test]
    fn take_and_put_back() {
        let mut p = vec![page(&["a", "b", "c"])];
        assert_eq!(take(&mut p, Pos::new(0, 1)), Some(app("b")));
        assert_eq!(p, vec![page(&["a", "c"])]);
        assert_eq!(take(&mut p, Pos::new(0, 5)), None);
        assert_eq!(take(&mut p, Pos::new(3, 0)), None);
        assert_eq!(put(&mut p, Pos::new(0, 0), app("b"), 4), Pos::new(0, 0));
        assert_eq!(p, vec![page(&["b", "a", "c"])]);
        // Past the end of the page: its end (no holes).
        assert_eq!(put(&mut p, Pos::new(0, 9), app("d"), 4), Pos::new(0, 3));
        assert_eq!(p, vec![page(&["b", "a", "c", "d"])]);
        // Past the last page: a new page.
        assert_eq!(put(&mut p, Pos::new(7, 3), app("e"), 4), Pos::new(1, 0));
        assert_eq!(p, vec![page(&["b", "a", "c", "d"]), page(&["e"])]);
    }

    #[test]
    fn a_full_page_pushes_its_last_on() {
        let mut p = vec![page(&["a", "b"]), page(&["c", "d"])];
        assert_eq!(put(&mut p, Pos::new(0, 1), app("x"), 2), Pos::new(0, 1));
        assert_eq!(p, vec![page(&["a", "x"]), page(&["b", "c"]), page(&["d"])], "pushed on page after page");
        // Dropped at the full page's end: before its last.
        let mut p = vec![page(&["a", "b"])];
        assert_eq!(put(&mut p, Pos::new(0, 2), app("x"), 2), Pos::new(0, 1));
        assert_eq!(p, vec![page(&["a", "x"]), page(&["b"])]);
    }

    #[test]
    fn empty_pages_go() {
        let mut p = vec![page(&[]), page(&["a"]), page(&[]), page(&["b", "c"])];
        assert_eq!(tidy(&mut p, Some(Pos::new(3, 1))), Some(Pos::new(1, 1)));
        assert_eq!(p, vec![page(&["a"]), page(&["b", "c"])]);
        let mut p = vec![page(&[]), page(&[])];
        assert_eq!(tidy(&mut p, Some(Pos::new(0, 0))), None);
        assert_eq!(p, vec![page(&[])], "one stays");
        assert_eq!(drag_pages(&vec![page(&["a"])]), 2, "a new last page to drop on");
        assert_eq!(drag_pages(&vec![page(&["a"]), page(&[])]), 2, "the emptied last page is that page");
    }

    #[test]
    fn zones_mirror_right_to_left() {
        // The middle: the inner 52 % of a 64 DIP icon box, 16.6 DIP each way.
        assert_eq!(zone(0.0, 0.0, 64.0, false), Zone::Middle);
        assert_eq!(zone(16.0, -16.0, 64.0, false), Zone::Middle, "a corner of the square");
        assert_eq!(zone(17.0, 0.0, 64.0, false), Zone::After, "just out: reorder");
        assert_eq!(zone(-17.0, 0.0, 64.0, false), Zone::Before);
        assert_eq!(zone(0.0, 17.0, 64.0, false), Zone::After, "below the middle: by the half it is in");
        assert_eq!(zone(-1.0, 30.0, 64.0, false), Zone::Before);
        assert_eq!(zone(-30.0, 0.0, 64.0, false), Zone::Before);
        assert_eq!(zone(30.0, 5.0, 64.0, false), Zone::After);
        assert_eq!(zone(-30.0, 0.0, 64.0, true), Zone::After);
        assert_eq!(zone(30.0, 0.0, 64.0, true), Zone::Before);
        // Scales with the icon.
        assert_eq!(zone(30.0, 0.0, 128.0, false), Zone::Middle);
    }

    #[test]
    fn the_numbers_the_owner_asked_for() {
        assert!((0.90..=0.94).contains(&EDIT_SCALE));
        assert_eq!(LIFT_SCALE, 1.04, "the design system's lift (2.6)");
        assert!((0.50..=0.55).contains(&FOLDER_ZONE), "the inner 50-55 % of the icon box");
        assert!((200..=250).contains(&FOLDER_DWELL_MS), "a short dwell, shown growing");
        assert!((80..=120).contains(&SETTLE_MS), "reflow after about 100 ms still");
        assert!((400..=500).contains(&EDGE_MS), "page turn after about 450 ms");
        assert!(PLATE_FROM < 1.0 && PLATE > 1.0 && TARGET_SCALE > 1.0);
        assert!(GHOST_OPACITY > 0.0 && GHOST_OPACITY < 0.5);
        // The plate around the grid sits on the spacing scale.
        assert!(crate::theme::metrics::space::SCALE.contains(&PANEL_MARGIN));
    }

    #[test]
    fn reflow_waits_for_the_pointer_to_settle() {
        assert_eq!(settle(2, None, 2), Settle::Keep, "on the gap: nothing");
        assert_eq!(settle(2, None, 4), Settle::Arm(4), "a new place: wait");
        assert_eq!(settle(2, Some(4), 4), Settle::Keep, "the same place: the wait goes on (not restarted per pixel)");
        assert_eq!(settle(2, Some(4), 5), Settle::Arm(5), "another place: wait anew");
        assert_eq!(settle(2, Some(4), 2), Settle::Clear, "back on the gap: nothing waits");
    }

    #[test]
    fn folder_intent_starts_and_ends_at_once() {
        assert_eq!(hold(None, Some(3)), Hold::Start(3));
        assert_eq!(hold(Some(3), Some(3)), Hold::Keep, "staying: it grows on");
        assert_eq!(hold(Some(3), Some(4)), Hold::Start(4), "another target: anew");
        assert_eq!(hold(Some(3), None), Hold::Clear, "out of the middle: gone");
        assert_eq!(hold(None, None), Hold::Keep);
    }

    #[test]
    fn edit_scale_and_its_inverse() {
        assert_eq!(edit_scale(true, true), EDIT_SCALE);
        assert_eq!(edit_scale(true, false), 1.0, "small animations off: no scale");
        assert_eq!(edit_scale(false, true), 1.0, "reduced motion / high contrast: no scale");
        let c = (960.0, 540.0);
        for p in [(0.0, 0.0), (960.0, 540.0), (1500.0, 200.0)] {
            let s = scale(p, c, EDIT_SCALE);
            let back = unscale(s, c, EDIT_SCALE);
            assert!((back.0 - p.0).abs() < 0.01 && (back.1 - p.1).abs() < 0.01);
        }
        assert_eq!(scale(c, c, 0.5), c, "the middle stays");
        assert_eq!(unscale((10.0, 10.0), c, 0.0), (10.0, 10.0), "a bad scale: as it is");
    }

    #[test]
    fn the_shown_scale_follows_the_glide() {
        use crate::motion::{GLIDE_MAX_MS, SAMPLES, SPRING_GLIDE};
        assert_eq!(glide_at(0.0), 0.0);
        assert_eq!(glide_at(1.0), 1.0);
        assert_eq!(glide_at(f32::NAN), 0.0);
        let mut last = 0.0;
        for k in 1..=40 {
            let v = glide_at(k as f32 / 40.0);
            assert!(v >= last - 1e-4 && v <= 1.01, "{k}: {v}");
            last = v;
        }
        // On the keys it is the spring itself; between them, between them.
        for (p, done) in SPRING_GLIDE.keys(GLIDE_MAX_MS, SAMPLES) {
            assert!((glide_at(p) - done).abs() < 1e-4, "{p}");
        }
        assert!(glide_at(0.5) > 0.75, "mostly there by half time");
        let len = GLIDE_MAX_MS as f32;
        assert_eq!(shown_scale(1.0, EDIT_SCALE, 0.0, len), 1.0, "at the start: as it was");
        assert_eq!(shown_scale(1.0, EDIT_SCALE, len, len), EDIT_SCALE);
        assert_eq!(shown_scale(1.0, EDIT_SCALE, 999.0, len), EDIT_SCALE, "done");
        assert_eq!(shown_scale(1.0, EDIT_SCALE, 5.0, 0.0), EDIT_SCALE, "no animation: the target");
        let mid = shown_scale(1.0, EDIT_SCALE, 28.0, len);
        assert!(mid < 1.0 && mid > EDIT_SCALE, "{mid}");
        // Early in the change a point is read with the scale shown then,
        // not the final one: the second column stays the second column.
        let c = (960.0, 540.0);
        let k = shown_scale(1.0, EDIT_SCALE, 2.0, len);
        let on_screen = scale((440.0, 300.0), c, k);
        let back = unscale(on_screen, c, k);
        assert!((back.0 - 440.0).abs() < 0.01);
    }

    #[test]
    fn the_panel_border_lights_before_the_app_leaves() {
        let r = (100.0, 100.0, 500.0, 400.0);
        assert_eq!(border(r, 300.0, 250.0), Border::Inside);
        assert_eq!(border(r, 110.0, 250.0), Border::Near);
        assert_eq!(border(r, 300.0, 395.0), Border::Near);
        assert_eq!(border(r, 99.0, 250.0), Border::Out);
        assert_eq!(border(r, 300.0, 400.0), Border::Out);
    }

    #[test]
    fn aiming_is_stable_around_the_gap() {
        // Five items besides the dragged one, the gap at 2: a b _ c d e.
        assert_eq!(aim(Some((2, Zone::Middle)), 2, 5), Aim::Gap(2), "over the gap");
        assert_eq!(aim(Some((0, Zone::Middle)), 2, 5), Aim::Onto(0));
        assert_eq!(aim(Some((3, Zone::Middle)), 2, 5), Aim::Onto(2), "slot 3 shows item 2 (c)");
        assert_eq!(aim(Some((3, Zone::After)), 2, 5), Aim::Gap(3));
        assert_eq!(aim(Some((3, Zone::Before)), 2, 5), Aim::Gap(2), "before c is where the gap is: no change");
        assert_eq!(aim(Some((1, Zone::Before)), 2, 5), Aim::Gap(1));
        assert_eq!(aim(Some((1, Zone::After)), 2, 5), Aim::Gap(2), "after b is the gap itself");
        assert_eq!(aim(Some((9, Zone::Middle)), 2, 5), Aim::Gap(5), "past the items: the end");
        assert_eq!(aim(None, 2, 5), Aim::Gap(2), "between cells: as it is");
        assert_eq!(aim(None, 9, 5), Aim::Gap(5), "a gap past the end is the end");
        // After moving the gap, the same spot asks for the same thing.
        let g = match aim(Some((1, Zone::Before)), 2, 5) {
            Aim::Gap(g) => g,
            _ => unreachable!(),
        };
        assert_eq!(aim(Some((1, Zone::Before)), g, 5), Aim::Gap(g));
        // An empty page: the only place is the start.
        assert_eq!(aim(Some((0, Zone::Middle)), 0, 0), Aim::Gap(0));
        assert_eq!(aim(Some((4, Zone::After)), 0, 0), Aim::Gap(0));
    }

    #[test]
    fn slots_around_the_gap() {
        assert_eq!((0..4).map(|k| shown_slot(k, Some(2))).collect::<Vec<_>>(), [0, 1, 3, 4]);
        assert_eq!(shown_slot(3, None), 3);
        assert_eq!((0..5).map(|s| item_in_slot(s, Some(2))).collect::<Vec<_>>(), [Some(0), Some(1), None, Some(2), Some(3)]);
        for k in 0..6 {
            assert_eq!(item_in_slot(shown_slot(k, Some(3)), Some(3)), Some(k));
        }
    }

    #[test]
    fn edges_turn_pages_the_way_the_screen_reads() {
        assert_eq!(edge(10.0, 1920.0, false), Some(false), "left: back");
        assert_eq!(edge(1900.0, 1920.0, false), Some(true), "right: on");
        assert_eq!(edge(900.0, 1920.0, false), None);
        assert_eq!(edge(10.0, 1920.0, true), Some(true), "right to left: the left edge goes on");
        assert_eq!(edge(1900.0, 1920.0, true), Some(false));
    }

    #[test]
    fn arrow_steps_over_pages_with_room_left() {
        let g = grid(3, 2);
        let lens = [5, 0, 2];
        assert_eq!(step(Pos::new(0, 4), &lens, &g, Arrow::Right, false), Some(Pos::new(2, 0)), "over the empty page");
        assert_eq!(step(Pos::new(2, 0), &lens, &g, Arrow::Left, false), Some(Pos::new(0, 4)));
        assert_eq!(step(Pos::new(2, 1), &lens, &g, Arrow::Right, false), None);
        assert_eq!(step(Pos::new(0, 0), &lens, &g, Arrow::Left, false), None);
        assert_eq!(step(Pos::new(0, 1), &lens, &g, Arrow::Left, true), Some(Pos::new(0, 2)), "mirrored");
        assert_eq!(step(Pos::new(0, 2), &lens, &g, Arrow::Down, false), Some(Pos::new(0, 4)), "into the short row's last");
        assert_eq!(step(Pos::new(0, 4), &lens, &g, Arrow::Up, false), Some(Pos::new(0, 1)));
        assert_eq!(step(Pos::new(0, 4), &lens, &g, Arrow::Down, false), None);
        assert_eq!(step(Pos::new(2, 0), &lens, &g, Arrow::Down, false), None, "no second row");
        assert_eq!(step(Pos::new(1, 0), &lens, &g, Arrow::Right, false), None, "an empty page");
    }

    #[test]
    fn keyboard_moves_the_lifted_icon() {
        let g = grid(2, 2);
        let mut p = vec![page(&["a", "b", "c", "d"]), page(&["e"])];
        // On a place: swaps with the next.
        assert_eq!(shift(&mut p, Pos::new(0, 0), &g, Arrow::Right, false, 4), Some(Pos::new(0, 1)));
        assert_eq!(p, vec![page(&["b", "a", "c", "d"]), page(&["e"])]);
        // Down a row.
        assert_eq!(shift(&mut p, Pos::new(0, 1), &g, Arrow::Down, false, 4), Some(Pos::new(0, 3)));
        assert_eq!(p, vec![page(&["b", "c", "d", "a"]), page(&["e"])]);
        // On to the next page's start.
        assert_eq!(shift(&mut p, Pos::new(0, 3), &g, Arrow::Right, false, 4), Some(Pos::new(1, 0)));
        assert_eq!(p, vec![page(&["b", "c", "d"]), page(&["a", "e"])]);
        // Back onto a page with room: its end.
        assert_eq!(shift(&mut p, Pos::new(1, 0), &g, Arrow::Left, false, 4), Some(Pos::new(0, 3)));
        assert_eq!(p, vec![page(&["b", "c", "d", "a"]), page(&["e"])]);
        // Back onto a full page: before its last, which is pushed on.
        assert_eq!(shift(&mut p, Pos::new(1, 0), &g, Arrow::Left, false, 4), Some(Pos::new(0, 3)));
        assert_eq!(p, vec![page(&["b", "c", "d", "e"]), page(&["a"])]);
        // Alone on the last page: nowhere further; with company, a new page.
        assert_eq!(shift(&mut p, Pos::new(1, 0), &g, Arrow::Right, false, 4), None);
        let mut q = vec![page(&["a", "b"])];
        assert_eq!(shift(&mut q, Pos::new(0, 1), &g, Arrow::Right, false, 4), Some(Pos::new(1, 0)));
        assert_eq!(q, vec![page(&["a"]), page(&["b"])]);
        // The page it left empty goes.
        let mut r = vec![page(&["a"]), page(&["b"])];
        assert_eq!(shift(&mut r, Pos::new(1, 0), &g, Arrow::Left, false, 4), Some(Pos::new(0, 1)));
        assert_eq!(r, vec![page(&["a", "b"])]);
        assert_eq!(shift(&mut r, Pos::new(0, 0), &g, Arrow::Left, false, 4), None, "the very first");
        assert_eq!(shift(&mut r, Pos::new(0, 0), &g, Arrow::Up, false, 4), None);
    }
}
