//! One way to move every widget (note, inbox, battery): press anywhere on
//! the card and move more than 6 px, and it follows the pointer; let go,
//! and it lands on the nearest free place (widget::snap). A press that
//! never moved is a click, the widget's own. What a press may not start
//! (the card's buttons, a note's text while it is typed, the inbox's
//! rows) the widget keeps out before calling press().
//!
//! One grab per thread (the card's owner thread); the pointer is captured
//! by the card window while it is held. A retained Composition face
//! replaces the card until its owner paints at the landing position.
use super::{drag_input::Sample, grid, icons, widget};
use crate::monitors::Monitor;
use std::cell::RefCell;
use windows::Win32::{
    Foundation::{HWND, POINT, RECT},
    UI::{
        Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture},
        WindowsAndMessaging::{GetCursorPos, GetWindowRect},
    },
};

/// How far the pointer moves (px, either way) before a press is a drag
/// (the long press (crate::desklayer::edit) gives up at the same).
pub const THRESHOLD: i32 = 6;

/// Pure: the pointer moved from `start` to `p` far enough to drag.
pub fn beyond(start: POINT, p: POINT) -> bool {
    (p.x - start.x).abs() > THRESHOLD || (p.y - start.y).abs() > THRESHOLD
}

struct Grab {
    hwnd: HWND,
    start: POINT,
    /// The card where it lay (screen px).
    origin: RECT,
    /// Its size in places, fixed when it was pressed: the drag hint, the
    /// landing place and what is kept all take it (at the display it is
    /// on), never its px read again.
    size: widget::Size,
    crossed_threshold: bool,
    moving: bool,
    placement: Option<Placement>,
    _grid: super::grid_state::Frozen,
}

/// Pure: card `origin`'s size in places: `saved` when known, else read
/// once from its px on the display it lies on.
pub fn fixed(saved: Option<widget::Size>, origin: RECT, mons: &[crate::monitors::Monitor], desk: impl Fn(f32) -> super::grid::Desk) -> widget::Size {
    saved.unwrap_or_else(|| {
        let Some(mi) = super::grid::monitor_of(origin, mons) else { return widget::Size::SMALL };
        let s = mons[mi].scale();
        super::grid::grids(&desk(s), mons)[mi].map_or(widget::Size::SMALL, |g| widget::Size::nearest((origin.right - origin.left, origin.bottom - origin.top), &g, s))
    })
}

/// Pure: where card `origin` (`size` places) is when the pointer went from
/// `start` to `p`: its top left follows the pointer, its size is `size`
/// at the display it is mostly on there (as it will land).
fn carried(at: RECT, size: widget::Size, mons: &[Monitor], grids: &[Option<grid::Grid>]) -> RECT {
    let resized_at = |i: usize| {
        let (w, h) = size.pixels(&grids[i]?, mons[i].scale());
        Some(RECT { right: at.left + w, bottom: at.top + h, ..at })
    };
    let Some(i) = grid::monitor_of(at, mons) else { return at };
    let Some(sized) = resized_at(i) else { return at };
    // Its size there decides the display it is on; once more with it.
    grid::monitor_of(sized, mons).filter(|&j| j != i).and_then(resized_at).unwrap_or(sized)
}

/// Frozen geometry shared by the card capture and the editor overlay.
pub(super) struct Placement {
    pub mons: Vec<Monitor>,
    desk: grid::Desk,
    grids: Vec<Option<grid::Grid>>,
    occupied: Vec<RECT>,
    history: widget::Target,
    cell: Option<(usize, grid::Cell)>,
    target: Option<RECT>,
}

#[derive(Clone, Copy)]
pub(super) struct Step {
    pub at: RECT,
    pub target: Option<RECT>,
    pub monitor: Option<isize>,
    pub offset: (f32, f32),
}

impl Placement {
    pub fn new(card: HWND) -> Self {
        Self::from_layout(super::grid_state::monitors(), widget::desk(1.), &icons::widget_rects(Some(card)))
    }

    fn from_layout(mons: Vec<Monitor>, desk: grid::Desk, others: &[RECT]) -> Self {
        let grids = mons.iter().map(|m| grid::Grid::widget(m, desk.cell)).collect();
        let occupied = widget::obstacles(&desk, others);
        Self { mons, desk, grids, occupied, history: widget::Target::default(), cell: None, target: None }
    }

    pub fn slots(&self, index: usize) -> Vec<RECT> {
        self.grids[index].map_or_else(Vec::new, |g| widget::free_slots(&g, 0, &self.occupied))
    }

    pub fn step(&mut self, origin: RECT, start: POINT, point: (f32, f32), size: widget::Size) -> Step {
        let p = POINT { x: point.0.round() as i32, y: point.1.round() as i32 };
        let at = carried(follow(origin, start, p), size, &self.mons, &self.grids);
        let index = grid::monitor_of(at, &self.mons);
        let cell = index.and_then(|i| self.grids[i].map(|g| (i, g.drag_cell(at))));
        if self.cell != cell {
            self.cell = cell;
            self.target = cell.and_then(|(i, _)| {
                self.grids[i].and_then(|g| widget::drag_target_in(&mut self.history, g, at, size, &self.occupied))
            });
        }
        Step {
            at,
            target: self.target,
            monitor: index.map(|i| self.mons[i].handle),
            offset: (point.0 - p.x as f32, point.1 - p.y as f32),
        }
    }

    /// Resolve using the actual pointer at button-up, with no prediction history.
    pub fn drop_at(&self, origin: RECT, start: POINT, p: POINT, size: widget::Size) -> Option<RECT> {
        let at = carried(follow(origin, start, p), size, &self.mons, &self.grids);
        let target = grid::monitor_of(at, &self.mons)
            .and_then(|i| self.grids[i])
            .and_then(|g| widget::nearest_free(&g, at, size.cells(), 0, &self.occupied));
        widget::drop_log(&self.desk, &self.mons, at, size, target);
        target
    }
}

thread_local! {
    static GRAB: RefCell<Option<Grab>> = const { RefCell::new(None) };
}

/// How a press ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Up {
    /// Nothing was held here.
    None,
    /// Let go where it was pressed: a click.
    Click,
    /// Crossed the drag threshold while a scene was unavailable.
    Suppressed,
    /// Dropped: the card lies at this place now (screen px).
    Dropped,
}

fn end_press(crossed_threshold: bool, moving: bool) -> Up {
    if moving {
        Up::Dropped
    } else if crossed_threshold {
        Up::Suppressed
    } else {
        Up::Click
    }
}

/// The pointer went down on card `h` (its saved size `size` when known,
/// else read now from its px, once): it may be a click or the start of a
/// drag.
pub fn press(h: HWND, size: Option<widget::Size>) {
    if super::grid_state::display_settling() { return; }
    let mut start = POINT::default();
    let mut origin = RECT::default();
    unsafe {
        let _ = GetCursorPos(&mut start);
        let _ = GetWindowRect(h, &mut origin);
        SetCapture(h);
    }
    let frozen = super::grid_state::Frozen::new();
    let size = fixed(size, origin, &super::grid_state::monitors(), widget::desk);
    GRAB.with(|g| *g.borrow_mut() = Some(Grab { hwnd: h, start, origin, size, crossed_threshold: false, moving: false, placement: None, _grid: frozen }));
}

/// Pure: a press at `p` (card DIP) on a card of kind `kind`, `w` DIP
/// wide, may start a drag: never on its buttons (top right), on a note's
/// text while it is typed (`typing_text`), or on the inbox's rows (their
/// top and foot, `rows`: the check and the text).
pub fn grabbable(kind: super::card::Kind, w: f32, p: (f32, f32), typing_text: bool, rows: Option<(f32, f32)>) -> bool {
    if super::card::button_at(kind, w, p.0, p.1).is_some() {
        return false;
    }
    match kind {
        super::card::Kind::Note => !typing_text,
        super::card::Kind::Inbox => rows.is_none_or(|(top, foot)| !(top..foot).contains(&p.1)),
        super::card::Kind::Battery => true,
    }
}

/// Card `h` is being dragged (moved past THRESHOLD).
pub fn moving(h: HWND) -> bool {
    GRAB.with(|g| g.borrow().as_ref().is_some_and(|g| g.hwnd == h && g.moving))
}

/// Pure: where a card that lay at `origin` is when the pointer went from
/// `start` to `p`.
pub fn follow(origin: RECT, start: POINT, p: POINT) -> RECT {
    let (dx, dy) = (p.x - start.x, p.y - start.y);
    RECT { left: origin.left + dx, top: origin.top + dy, right: origin.right + dx, bottom: origin.bottom + dy }
}

/// The pointer moved: true once the press has crossed the drag threshold.
pub fn moved(h: HWND) -> bool {
    let started = super::drag_metrics::now();
    let mut p = POINT::default();
    if unsafe { GetCursorPos(&mut p) }.is_err() {
        return moving(h);
    }
    if moving(h) {
        sample(h, started, p);
        return true;
    }
    let prepare = GRAB.with(|g| {
        let mut g = g.borrow_mut();
        let g = g.as_mut().filter(|g| g.hwnd == h)?;
        g.crossed_threshold |= beyond(g.start, p);
        g.crossed_threshold.then(|| (g.origin, g.size, g.placement.take()))
    });
    let Some((origin, size, placement)) = prepare else { return false };
    let placement = placement.unwrap_or_else(|| Placement::new(h));
    let began = super::drag_hint::begin(h, origin, size, false, started, &placement);
    let retained = GRAB.with(|g| {
        let mut g = g.borrow_mut();
        let Some(g) = g.as_mut().filter(|g| g.hwnd == h) else { return false };
        g.placement = Some(placement);
        g.moving = began;
        true
    });
    if !retained {
        if began { super::drag_hint::hide(); }
        return false;
    }
    if began {
        sample(h, started, p);
    }
    true
}

fn sample(h: HWND, started: f64, p: POINT) {
    if let Some(sample) = super::drag_hint::sample(h, started, p) {
        tick(h, sample);
    }
}

pub(super) fn tick(h: HWND, sample: Sample) {
    let step = GRAB.with(|slot| {
        let mut slot = slot.borrow_mut();
        let g = slot.as_mut().filter(|g| g.hwnd == h && g.moving)?;
        Some(g.placement.as_mut()?.step(g.origin, g.start, sample.point, g.size))
    });
    if let Some(step) = step {
        super::drag_hint::update(h, step, sample.at);
    }
}

/// The pointer was let go over card `h`.
pub fn release(h: HWND) -> Up {
    let released = super::drag_metrics::now();
    let Some(g) = GRAB.with(|g| g.borrow_mut().take_if(|g| g.hwnd == h)) else { return Up::None };
    let up = end_press(g.crossed_threshold, g.moving);
    let point = (up == Up::Dropped).then(|| super::drag_hint::release_point(h)).flatten();
    unsafe {
        if GetCapture() == h {
            let _ = ReleaseCapture();
        }
    }
    if up != Up::Dropped {
        return up;
    }
    let target = point.zip(g.placement.as_ref())
        .and_then(|(p, placement)| placement.drop_at(g.origin, g.start, p, g.size));
    let to = widget::landing(target, icons::committed(h).unwrap_or(g.origin));
    icons::commit(h, to);
    super::drag_hint::settle(h, to, released);
    Up::Dropped
}

/// The capture was taken away (a menu, another window): a drag goes back
/// to where the card lay. True when card `h` was being dragged.
pub fn cancel(h: HWND) -> bool {
    let released = super::drag_metrics::now();
    let Some(g) = GRAB.with(|g| g.borrow_mut().take_if(|g| g.hwnd == h)) else { return false };
    if g.moving {
        super::drag_hint::settle(h, g.origin, released);
    }
    g.moving
}

/// Release the press's grid hold after its window has been destroyed.
pub(super) fn discard(h: HWND) {
    GRAB.with(|g| g.borrow_mut().take_if(|g| g.hwnd == h));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocked_drag_never_turns_into_a_click_on_release() {
        assert_eq!(end_press(false, false), Up::Click);
        assert_eq!(end_press(true, false), Up::Suppressed);
        assert_eq!(end_press(true, true), Up::Dropped);
    }

    /// A press is a click until the pointer moves more than 6 px either
    /// way; then the card follows it by the same amount.
    #[test]
    fn a_drag_starts_past_six_px() {
        let s = POINT { x: 100, y: 100 };
        for (x, y, drag) in [(106, 100, false), (100, 94, false), (106, 106, false), (107, 100, true), (100, 93, true), (94, 107, true)] {
            assert_eq!(beyond(s, POINT { x, y }), drag, "{x},{y}");
        }
        let r = RECT { left: 10, top: 20, right: 110, bottom: 120 };
        assert_eq!(follow(r, s, POINT { x: 130, y: 90 }), RECT { left: 40, top: 10, right: 140, bottom: 110 });
    }
    /// Review P5: a 2×1 card carried from a 100 % display onto a 200 %
    /// one keeps the size it had when it was picked up (its saved places):
    /// the card dragged, its landing place and where it is kept (the hub
    /// snaps it again from its px) are one size, that size at the 200 %
    /// display. Its px never make it another size.
    #[test]
    fn a_card_keeps_its_size_across_mixed_dpi() {
        use crate::monitors::Monitor;
        use super::super::grid::{Desk, Grid};
        let a = Monitor { handle: 1, rect: RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, work: RECT { left: 0, top: 0, right: 1920, bottom: 1040 }, primary: true, dpi: 96 };
        let b = Monitor { handle: 2, rect: RECT { left: 1920, top: 0, right: 5760, bottom: 2160 }, work: RECT { left: 1920, top: 0, right: 5760, bottom: 2080 }, primary: false, dpi: 192 };
        let mons = [a, b];
        // Explorer's one icon spacing (px) on every display.
        let desk = |_: f32| Desk { cell: (76, 96), ..Default::default() };
        let ga = Grid::widget(&a, desk(1.).cell).unwrap();
        let gb = Grid::widget(&b, desk(1.).cell).unwrap();
        let size = widget::Size::MEDIUM;
        let origin = widget::snap(&desk(1.), &mons, RECT { left: 200, top: 200, right: 488, bottom: 376 }, Some(size), &[]).unwrap();
        assert_eq!(ga.touched(origin).map(|s| (s.w, s.h)), Some(size.cells()));
        // Picked up with no saved size: read once, from where it lay.
        assert_eq!(fixed(None, origin, &mons, desk), size);
        // A saved size stands, whatever its px.
        assert_eq!(fixed(Some(widget::Size::LARGE), origin, &mons, desk), widget::Size::LARGE);
        let start = POINT { x: origin.left + 20, y: origin.top + 20 };
        for p in [POINT { x: 2600, y: 400 }, POINT { x: 4000, y: 900 }] {
            let held = carried(follow(origin, start, p), size, &mons, &[Some(ga), Some(gb)]);
            let px = size.pixels(&gb, 2.);
            assert_eq!((held.left, held.top), (origin.left + p.x - start.x, origin.top + p.y - start.y), "it follows the pointer");
            assert_eq!((held.right - held.left, held.bottom - held.top), px, "dragged at its size there");
            let to = widget::snap(&desk(2.), &mons, held, Some(size), &[]).unwrap();
            assert_eq!(gb.touched(to).map(|s| (s.w, s.h)), Some(size.cells()));
            // Kept: snapped again from its px (no saved size): the same place.
            assert_eq!(widget::snap(&desk(2.), &mons, to, None, &[]), Some(to));
        }
        // Back on the 100 % display: its size there again.
        let back = carried(follow(origin, start, POINT { x: start.x + 300, y: start.y }), size, &mons, &[Some(ga), Some(gb)]);
        assert_eq!((back.right - back.left, back.bottom - back.top), size.pixels(&ga, 1.));
    }

    #[test]
    fn a_target_search_runs_only_when_the_card_enters_another_cell() {
        let work = RECT { right: 800, bottom: 600, ..Default::default() };
        let monitor = Monitor { handle: 1, rect: work, work, primary: true, dpi: 96 };
        let desk = grid::Desk { cell: (80, 100), ..Default::default() };
        let mut placement = Placement::from_layout(vec![monitor], desk, &[]);
        let g = placement.grids[0].unwrap();
        let origin = g.rect(grid::Span { c: 0, r: 0, w: 2, h: 2 }, 0);
        let start = POINT::default();
        let size = widget::Size::SMALL;
        assert_eq!(placement.step(origin, start, (0., 0.), size).target, Some(origin));
        assert_eq!(placement.cell, Some((0, (0, 0))));
        // Occupancy is frozen in production. Invalidate it here to detect a second search.
        placement.occupied.push(work);
        assert_eq!(placement.step(origin, start, (10., 0.), size).target, Some(origin));
        assert_eq!(placement.step(origin, start, (g.cw as f32, 0.), size).target, None);
        assert_eq!(placement.cell, Some((0, (1, 0))));
    }

    #[test]
    fn release_uses_the_actual_cell_when_prediction_crossed_a_boundary() {
        let work = RECT { right: 800, bottom: 600, ..Default::default() };
        let monitor = Monitor { handle: 1, rect: work, work, primary: true, dpi: 96 };
        let desk = grid::Desk { cell: (80, 100), ..Default::default() };
        let mut placement = Placement::from_layout(vec![monitor], desk, &[]);
        let g = placement.grids[0].unwrap();
        let origin = g.rect(grid::Span { c: 0, r: 0, w: 2, h: 2 }, 0);
        let start = POINT::default();
        let size = widget::Size::SMALL;
        let preview = placement.step(origin, start, (g.cw as f32, 0.), size).target;
        assert_ne!(preview, Some(origin));
        assert_eq!(placement.drop_at(origin, start, POINT { x: g.cw / 4, y: 0 }, size), Some(origin));
    }
    /// Anywhere on a card but its buttons, a note's text being typed and
    /// the inbox's rows.
    #[test]
    fn a_drag_starts_anywhere_but_the_exceptions() {
        use super::super::card::Kind;
        let w = 288.;
        for kind in [Kind::Note, Kind::Inbox, Kind::Battery] {
            // The header, the foot, the middle, an edge: all grab.
            for p in [(30., 20.), (140., 160.), (140., 100.), (4., 90.)] {
                assert!(grabbable(kind, w, p, false, Some((44., 134.))) || (kind == Kind::Inbox && (44. ..134.).contains(&p.1)), "{kind:?} {p:?}");
            }
            // The … button (top right): never.
            assert!(!grabbable(kind, w, (w - 23., 23.), false, None), "{kind:?}");
        }
        // A note's + button; its text while typed; its text at rest grabs.
        assert!(!grabbable(Kind::Note, w, (w - 55., 23.), false, None));
        assert!(!grabbable(Kind::Note, w, (100., 100.), true, None));
        assert!(grabbable(Kind::Note, w, (100., 100.), false, None));
        // The inbox's rows (checks and text) do not; below them does.
        assert!(!grabbable(Kind::Inbox, w, (24., 60.), false, Some((44., 134.))));
        assert!(!grabbable(Kind::Inbox, w, (150., 120.), false, Some((44., 134.))));
        assert!(grabbable(Kind::Inbox, w, (150., 150.), false, Some((44., 134.))));
    }
}
