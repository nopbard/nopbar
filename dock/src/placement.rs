//! Pure math for sharing the screen with the Windows taskbar: where the
//! dock and the top bar may sit when the taskbar stays (or could not be
//! hidden), whether hiding it really freed its strip, and whether a
//! restore really gave it back. Everything here is plain data in, data out
//! (unit-tested for every edge); taskbar.rs gathers the data.
//!
//! Coexistence rules (hide_taskbar off, or hiding refused):
//! - The dock lives inside the monitor's work area (which already leaves
//!   out a taskbar that reserves its strip) and also stays clear of every
//!   visible taskbar's full strip. That second part covers an auto-hide
//!   taskbar (it reserves nothing, so the work area is the whole monitor):
//!   the dock sits one taskbar-thickness in from that edge, so the taskbar
//!   slides in under it instead of over it. On the same edge the dock is
//!   stacked on top of (inside) the taskbar; on another edge it is only
//!   kept out of the taskbar's corner.
//! - With the dock's own auto-hide on, its reveal strip is its window edge,
//!   i.e. right next to the taskbar: push to the screen edge for the
//!   taskbar, a little further in for the dock. Both keep working.
//! - The top bar sits below a taskbar on the top edge (auto-hide or not)
//!   and reserves down to its own bottom edge; beside one on the left or
//!   right edge, so the Start button in that corner stays reachable.
//!   Moved to the bottom edge (topbar.position) it is the mirror: above a
//!   taskbar on the bottom edge, reserving up to its own top edge; the
//!   dock keeps clear of it there as it does at the top, a bottom dock
//!   sitting right above it (its auto-hide strip too). What hangs off the
//!   bar (menus, panels, tips) opens upward then (`hang_y`).

use windows::Win32::Foundation::RECT;

use crate::config::Position;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl Edge {
    pub fn name(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Top => "top",
            Edge::Right => "right",
            Edge::Bottom => "bottom",
        }
    }
}

impl From<Position> for Edge {
    fn from(p: Position) -> Self {
        match p {
            Position::Bottom => Edge::Bottom,
            Position::Left => Edge::Left,
            Position::Right => Edge::Right,
        }
    }
}

fn w(r: RECT) -> i32 {
    r.right - r.left
}

fn h(r: RECT) -> i32 {
    r.bottom - r.top
}

/// Which edge of `mon` a taskbar window `bar` belongs to, and how thick it
/// is. Works for a slid-away auto-hide taskbar too (its rect then hangs
/// mostly off the monitor): the long side gives the orientation, the
/// center the side. None for an empty rect.
pub fn bar_edge(mon: RECT, bar: RECT) -> Option<(Edge, i32)> {
    let (bw, bh) = (w(bar), h(bar));
    if bw <= 0 || bh <= 0 || w(mon) <= 0 || h(mon) <= 0 {
        return None;
    }
    if bw >= bh {
        // Horizontal: top or bottom.
        let thick = bh.min(h(mon) / 2);
        let edge = if (bar.top + bar.bottom) < (mon.top + mon.bottom) { Edge::Top } else { Edge::Bottom };
        Some((edge, thick))
    } else {
        let thick = bw.min(w(mon) / 2);
        let edge = if (bar.left + bar.right) < (mon.left + mon.right) { Edge::Left } else { Edge::Right };
        Some((edge, thick))
    }
}

/// Which of `monitors` a taskbar window `bar` belongs to. A rect inside
/// one monitor is on it. A rect across several is a slid-away auto-hide
/// taskbar on a shared edge: it slid *out of* its own monitor, so that is
/// the one with the smaller (non-empty) overlap, its few px remnant, not
/// the neighbour it mostly hangs over (where MonitorFromRect would put it).
pub fn bar_monitor(bar: RECT, monitors: &[RECT]) -> Option<usize> {
    let overlap = |m: RECT| -> i64 {
        let w = (bar.right.min(m.right) - bar.left.max(m.left)).max(0) as i64;
        let h = (bar.bottom.min(m.bottom) - bar.top.max(m.top)).max(0) as i64;
        w * h
    };
    let hits: Vec<(usize, i64)> = monitors.iter().enumerate().map(|(i, &m)| (i, overlap(m))).filter(|&(_, a)| a > 0).collect();
    match hits.len() {
        0 => {
            // Wholly off-screen: the monitor whose edge it is nearest.
            let dist = |m: RECT| {
                let dx = (m.left - bar.right).max(bar.left - m.right).max(0) as i64;
                let dy = (m.top - bar.bottom).max(bar.top - m.bottom).max(0) as i64;
                dx + dy
            };
            monitors.iter().enumerate().min_by_key(|(_, &m)| dist(m)).map(|(i, _)| i)
        }
        1 => Some(hits[0].0),
        _ => hits.iter().min_by_key(|&&(_, a)| a).map(|&(i, _)| i),
    }
}

/// How far the work area starts in from the monitor's `edge`.
pub fn work_inset(mon: RECT, work: RECT, edge: Edge) -> i32 {
    match edge {
        Edge::Left => work.left - mon.left,
        Edge::Top => work.top - mon.top,
        Edge::Right => mon.right - work.right,
        Edge::Bottom => mon.bottom - work.bottom,
    }
    .max(0)
}

/// Shrinks `area` so it stays at least `px` in from `mon`'s `edge`.
fn keep_off(area: &mut RECT, mon: RECT, edge: Edge, px: i32) {
    match edge {
        Edge::Left => area.left = area.left.max(mon.left + px),
        Edge::Top => area.top = area.top.max(mon.top + px),
        Edge::Right => area.right = area.right.min(mon.right - px),
        Edge::Bottom => area.bottom = area.bottom.min(mon.bottom - px),
    }
}

/// How far in from a monitor's top and bottom edges the top bar keeps
/// the dock (and snapped windows) out, px (0 = nothing on that edge). On
/// top: what it reserves (0 when it reserves nothing). At the bottom: its
/// whole strip, reserved or not, since a bottom dock would cover it whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BarSpace {
    pub top: i32,
    pub bottom: i32,
    /// At the bottom, the bar's own height when it reserves nothing
    /// (reserve_work_area off): a bottom dock reserves that strip on its
    /// behalf (dock_reserve). 0 when the bar reserves it, or sits on top.
    pub bottom_free: i32,
}

/// Where the dock may be on a monitor. `hiding`: NopBar has the taskbar
/// hidden (the whole monitor is ours). Otherwise the work area, kept clear
/// of every visible taskbar strip on this monitor (`bars`: their window
/// rects), and clear of the top bar (`bar`: how far in from the top or
/// the bottom it reaches).
pub fn dock_area(mon: RECT, work: RECT, bars: &[RECT], hiding: bool, bar: BarSpace) -> RECT {
    let mut area = if hiding { mon } else { work };
    if !hiding {
        for &b in bars {
            if let Some((edge, thick)) = bar_edge(mon, b) {
                keep_off(&mut area, mon, edge, thick);
            }
        }
    }
    area.top = area.top.max(mon.top + bar.top.max(0));
    area.bottom = area.bottom.min(mon.bottom - bar.bottom.max(0));
    area
}

/// What a dock reserving `thickness` px asks the shell for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DockReserve {
    /// How far in from the edge its strip must reach at least (0: just
    /// its thickness, wherever the shell stacks it).
    pub reach: i32,
    /// How much of that edge is NopBar's own (the taskbar hide check
    /// subtracts it): its thickness, plus a bar strip it covers.
    pub own: i32,
}

/// The bottom edge's stacking (a dock right above a top bar moved to the
/// bottom). Each of our AppBars reserves its own thickness and the shell
/// stacks them one inside the other, in whichever order it keeps: the
/// union, and so the work area, is the same either way and ends on the
/// dock's top, as drawn. So the dock adds no reach when the bar reserves
/// its strip itself (a reach would then stack on top of the bar's strip in
/// one of the orders and leave a band). Only over a bar that reserves
/// nothing (`bar.bottom_free`) does the dock reserve the bar's space too,
/// reaching past it; that strip is ours, so it counts as own, while a
/// taskbar beyond the bar (in `bar.bottom`, reached past as well) does
/// not. Plain thickness on the other edges.
pub fn dock_reserve(pos: Position, thickness: i32, bar: BarSpace) -> DockReserve {
    if pos == Position::Bottom && thickness > 0 && bar.bottom_free > 0 {
        DockReserve { reach: bar.bottom.max(bar.bottom_free) + thickness, own: thickness + bar.bottom_free }
    } else {
        DockReserve { reach: 0, own: thickness.max(0) }
    }
}

/// How much of `mon`'s `edge` NopBar's own AppBars reserve (`own`: each
/// one's monitor, edge and own px), for the hide check (BarSeen::own_px).
pub fn own_inset(own: &[(RECT, Edge, i32)], mon: RECT, edge: Edge) -> i32 {
    own.iter().filter(|(m, e, _)| *m == mon && *e == edge).map(|(_, _, px)| (*px).max(0)).sum()
}

/// An AppBar's place as the shell answered ABM_SETPOS on the log line
/// (`who` and its edge, the rect it got, and the one asked for when the
/// shell moved it).
pub fn seat_text(who: &str, edge: Edge, asked: RECT, got: RECT) -> String {
    let r = |r: RECT| format!("({},{})-({},{})", r.left, r.top, r.right, r.bottom);
    if asked == got {
        format!("{who} {} {}", edge.name(), r(got))
    } else {
        format!("{who} {} {} (asked {})", edge.name(), r(got), r(asked))
    }
}

/// Whether an AppBar must send ABM_SETPOS again: the strip it would ask
/// for now (`edge`, `asked`) differs from the one the shell already has
/// from it (`last`; None since it registered). Answering every
/// ABN_POSCHANGED with an unchanged SETPOS would notify the other bars
/// again: two AppBars on one edge could keep each other busy.
pub fn setpos_needed(last: Option<(Edge, RECT)>, edge: Edge, asked: RECT) -> bool {
    last != Some((edge, asked))
}

/// The dock window's rect, `depth` px deep, on `area`'s `pos` edge (as
/// dock::edge::Frame::window_rect lays it out).
pub fn dock_rect(pos: Position, area: RECT, depth: i32) -> RECT {
    match pos {
        Position::Bottom => RECT { top: area.bottom - depth, ..area },
        Position::Left => RECT { right: area.left + depth, ..area },
        Position::Right => RECT { left: area.right - depth, ..area },
    }
}

/// The top bar's window rect, `height` px tall: along the monitor's top
/// (`bottom`: its bottom), past a visible taskbar on that edge and beside
/// one on the left or right (its Start button stays reachable). The whole
/// edge while NopBar hides the taskbar.
pub fn topbar_rect(mon: RECT, bars: &[RECT], hiding: bool, height: i32, bottom: bool) -> RECT {
    let far = if bottom { Edge::Top } else { Edge::Bottom };
    let mut r = mon;
    if !hiding {
        for &b in bars {
            if let Some((edge, thick)) = bar_edge(mon, b) {
                if edge != far {
                    keep_off(&mut r, mon, edge, thick);
                }
            }
        }
    }
    if bottom {
        RECT { top: r.bottom - height, ..r }
    } else {
        RECT { bottom: r.top + height, ..r }
    }
}

/// The one space (DIP) between the top bar or the dock and everything that
/// opens off it, like macOS's menus under the menu bar: the bar's app
/// menus, its own menu, its status menus and panels, the calendar, the
/// Control Center, the island's card, the hidden items' menu, the point a
/// tray icon's app gets for its menu; the dock's menus, window previews and
/// stacks. At the bar's top or bottom and the dock's three sides alike.
/// Measured between the edges seen: the bar's or the dock's (a magnified
/// icon's when it stands out further) and the popup's own rect (its shadow
/// margin is outside it).
pub const POPUP_GAP_DIP: f32 = 6.0;

/// POPUP_GAP_DIP in whole px on a display at `scale`.
pub fn popup_gap(scale: f32) -> i32 {
    (POPUP_GAP_DIP * scale).round() as i32
}

/// The left edge (screen px) of a panel `w_px` wide that sits at the
/// work area's far end: its right edge `gap` px in from `work.right`
/// (right to left: its left edge `gap` px in from `work.left`), the same
/// gap as from the bar (POPUP_GAP_DIP). The Control Center and the calendar
/// both open there, so they share one spot. Kept inside `work`.
pub fn edge_x(work: RECT, w_px: i32, gap: i32, rtl: bool) -> i32 {
    let x = if rtl { work.left + gap } else { work.right - gap - w_px };
    x.min(work.right - w_px).max(work.left)
}

/// Where something hanging off bar cell `anchor` starts (screen px): its
/// top edge `gap` px under the bar, or its bottom edge `gap` px over it
/// (`up`, a bar at the bottom). A work area that ends short of the bar
/// (another bar under ours, a dock over a bar at the bottom) moves that
/// edge in: the gap is kept from what is really there. `anchor` spans the
/// bar's height (a pill inside it is widened to the bar first).
pub fn hang_edge(anchor: RECT, gap: i32, up: bool, work: RECT) -> i32 {
    if up { anchor.top.min(work.bottom) - gap } else { anchor.bottom.max(work.top) + gap }
}

/// Pure: how tall (px) something hanging from `edge` (hang_edge; the top
/// of a menu hanging down, the bottom of one opening `up`) can be and stay
/// inside `work`: the room between that edge and the far side, at least
/// 1 px, at most the work area's height.
pub fn room_from(edge: i32, up: bool, work: RECT) -> i32 {
    let room = if up { edge.min(work.bottom) - work.top } else { work.bottom - edge.max(work.top) };
    room.clamp(1, (work.bottom - work.top).max(1))
}

/// Pure: the room (px) a panel hanging off bar cell `anchor` has, `gap`
/// px from the bar (hang_edge): a panel sized to it never moves over the
/// gap or the bar.
pub fn hang_room(anchor: RECT, gap: i32, up: bool, work: RECT) -> i32 {
    room_from(hang_edge(anchor, gap, up, work), up, work)
}

/// The top edge (screen px) of something `h_px` tall that hangs off a bar
/// cell `anchor` (a menu, panel, card or tip), `gap` px away (hang_edge):
/// under it while the bar is on top, over it (`up`) while the bar is at
/// the bottom. Kept inside `work` either way: moved up / down as needed
/// (only when taller than hang_room), the top edge winning when it is
/// taller than `work`.
pub fn hang_y(anchor: RECT, h_px: i32, gap: i32, up: bool, work: RECT) -> i32 {
    let edge = hang_edge(anchor, gap, up, work);
    let y = if up { edge - h_px } else { edge };
    y.min(work.bottom - h_px).max(work.top)
}

/// The strip an AppBar on `edge` ends up reserving: the shell's answer to
/// ABM_QUERYPOS (`queried`, which may start further in, past the taskbar
/// and other bars), `thickness` px from where it starts, but reaching at
/// least `reach` px in from the monitor's edge (so a bar placed below an
/// auto-hide taskbar, which the shell does not step around, still keeps
/// maximized windows off itself).
pub fn reserve_rect(mon: RECT, queried: RECT, edge: Edge, thickness: i32, reach: i32) -> RECT {
    match edge {
        Edge::Top => RECT { bottom: (queried.top + thickness).max(mon.top + reach), ..queried },
        Edge::Left => RECT { right: (queried.left + thickness).max(mon.left + reach), ..queried },
        Edge::Right => RECT { left: (queried.right - thickness).min(mon.right - reach), ..queried },
        Edge::Bottom => RECT { top: (queried.bottom - thickness).min(mon.bottom - reach), ..queried },
    }
}

/// One taskbar as the hide check sees it: its monitor, that monitor's
/// work area, its window rect, and how much of that edge NopBar's own
/// AppBars reserve.
#[derive(Clone, Copy, Debug)]
pub struct BarSeen {
    pub mon: RECT,
    pub work: RECT,
    pub bar: RECT,
    pub own_px: i32,
}

/// The taskbar's strip is still kept out of the work area: more of the
/// edge is reserved than NopBar's own bars account for, at least half the
/// taskbar's thickness.
pub fn strip_reserved(s: &BarSeen) -> bool {
    let Some((edge, thick)) = bar_edge(s.mon, s.bar) else { return false };
    let extra = work_inset(s.mon, s.work, edge) - s.own_px.max(0);
    extra > 0 && extra * 2 >= thick
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HideCheck {
    /// Every taskbar's strip is free: its windows may be hidden.
    Freed,
    /// Not yet (or not at all: the caller counts the tries).
    Waiting,
    /// Hiding is no longer wanted (restored meanwhile).
    NotWanted,
}

/// After auto-hide was asked for: are the windows safe to hide? Never with
/// no taskbar seen (explorer starting) or a strip still reserved, which
/// would leave an empty band nothing can use.
pub fn hide_verdict(bars: &[BarSeen]) -> HideCheck {
    if bars.is_empty() || bars.iter().any(strip_reserved) {
        HideCheck::Waiting
    } else {
        HideCheck::Freed
    }
}

/// A taskbar window sits wholly on its monitor (a 2 px slack for
/// rounding): shown in its place, not slid away the way auto-hide leaves
/// it. A visible taskbar off its place, with its strip reserved, is an
/// empty band at the screen edge.
pub fn bar_seated(mon: RECT, bar: RECT) -> bool {
    const SLACK: i32 = 2;
    w(bar) > 0
        && h(bar) > 0
        && bar.left >= mon.left - SLACK
        && bar.top >= mon.top - SLACK
        && bar.right <= mon.right + SLACK
        && bar.bottom <= mon.bottom + SLACK
}

/// How far a restore got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restore {
    /// A window still hidden, the auto-hide bit not back, or no taskbar.
    NotBack,
    /// State back and every window shown, but an always-shown taskbar is
    /// not in its place yet (explorer slides it in; retried once).
    NotInPlace,
    /// Given back: shown, state as saved, and in its place.
    Done,
}

/// A restore's outcome: the taskbar windows are all visible (and there
/// are some), the auto-hide bit is back to what was saved (`saved` None:
/// we never changed it), and, when we changed it to "always shown",
/// every window sits in its place on the screen (`seated` of `windows`).
pub fn restore_verdict(saved: Option<u32>, now: u32, windows: usize, visible: usize, seated: usize) -> Restore {
    const AUTOHIDE: u32 = 1;
    if windows == 0 || visible != windows || saved.is_some_and(|s| s & AUTOHIDE != now & AUTOHIDE) {
        return Restore::NotBack;
    }
    // Only a state we put back can be expected to re-seat the windows; an
    // untouched taskbar (no marker) is the user's as it is.
    if saved.is_some() && now & AUTOHIDE == 0 && seated < windows {
        return Restore::NotInPlace;
    }
    Restore::Done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_x_sits_a_gap_in_from_the_far_end() {
        let work = RECT { left: -1920, top: 0, right: 0, bottom: 1040 };
        assert_eq!(edge_x(work, 300, 8, false), -308, "right end, 8 px in");
        assert_eq!(edge_x(work, 300, 8, true), -1912, "right to left: left end");
        // Wider than the room left by the gap: inside the work area.
        assert_eq!(edge_x(work, 1915, 8, false), -1920);
        assert_eq!(edge_x(work, 1915, 8, true), -1915);
        assert_eq!(edge_x(work, 4000, 8, false), -1920);
    }

    const MON: RECT = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
    const T: i32 = 48;
    const TOPBAR: i32 = 28;
    const DEPTH: i32 = 120;

    fn bar_on(edge: Edge, mon: RECT, t: i32) -> RECT {
        match edge {
            Edge::Bottom => RECT { top: mon.bottom - t, ..mon },
            Edge::Top => RECT { bottom: mon.top + t, ..mon },
            Edge::Left => RECT { right: mon.left + t, ..mon },
            Edge::Right => RECT { left: mon.right - t, ..mon },
        }
    }

    /// A slid-away auto-hide taskbar: 2 px left on the monitor.
    fn hidden_bar(edge: Edge, mon: RECT, t: i32) -> RECT {
        match edge {
            Edge::Bottom => RECT { top: mon.bottom - 2, bottom: mon.bottom - 2 + t, ..mon },
            Edge::Top => RECT { top: mon.top + 2 - t, bottom: mon.top + 2, ..mon },
            Edge::Left => RECT { left: mon.left + 2 - t, right: mon.left + 2, ..mon },
            Edge::Right => RECT { left: mon.right - 2, right: mon.right - 2 + t, ..mon },
        }
    }

    fn work_without(edge: Edge, mon: RECT, t: i32) -> RECT {
        let mut r = mon;
        keep_off(&mut r, mon, edge, t);
        r
    }

    fn overlap(a: RECT, b: RECT) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    fn inside(a: RECT, m: RECT) -> bool {
        a.left >= m.left && a.top >= m.top && a.right <= m.right && a.bottom <= m.bottom
    }

    const NONE: BarSpace = BarSpace { top: 0, bottom: 0, bottom_free: 0 };

    /// A top bar on top reaching `px` down.
    fn top(px: i32) -> BarSpace {
        BarSpace { top: px, ..NONE }
    }

    const EDGES: [Edge; 4] = [Edge::Left, Edge::Top, Edge::Right, Edge::Bottom];
    const POSITIONS: [Position; 3] = [Position::Bottom, Position::Left, Position::Right];

    #[test]
    fn bar_edges_and_thickness() {
        for e in EDGES {
            assert_eq!(bar_edge(MON, bar_on(e, MON, T)), Some((e, T)), "{e:?}");
            assert_eq!(bar_edge(MON, hidden_bar(e, MON, T)), Some((e, T)), "slid away {e:?}");
        }
        // A secondary monitor left of the primary, negative coordinates.
        let m2 = RECT { left: -1280, top: 0, right: 0, bottom: 1024 };
        for e in EDGES {
            assert_eq!(bar_edge(m2, bar_on(e, m2, 40)), Some((e, 40)), "secondary {e:?}");
        }
        assert_eq!(bar_edge(MON, RECT::default()), None);
    }

    /// Every combination: taskbar edge x auto-hide x dock edge x top bar.
    /// The dock never covers the taskbar (shown or revealed), never the top
    /// bar, and stays on the monitor.
    #[test]
    fn dock_never_overlaps_a_kept_taskbar() {
        for tb in EDGES {
            for autohide in [false, true] {
                // Auto-hide taskbars reserve nothing: the work area is the
                // whole monitor and the window hangs off the edge.
                let work = if autohide { MON } else { work_without(tb, MON, T) };
                let seen = if autohide { hidden_bar(tb, MON, T) } else { bar_on(tb, MON, T) };
                let revealed = bar_on(tb, MON, T);
                for topbar in [false, true] {
                    let topbar_rect = topbar_rect(MON, &[seen], false, TOPBAR, false);
                    let top_px = if topbar { topbar_rect.bottom - MON.top } else { 0 };
                    if topbar {
                        assert!(!overlap(topbar_rect, revealed), "top bar vs taskbar {tb:?} autohide={autohide}");
                    }
                    for pos in POSITIONS {
                        let area = dock_area(MON, work, &[seen], false, top(top_px));
                        let r = dock_rect(pos, area, DEPTH);
                        let what = format!("taskbar {tb:?} autohide={autohide} topbar={topbar} dock {pos:?}");
                        assert!(!overlap(r, revealed), "{what}: {r:?}");
                        assert!(inside(r, MON), "{what}");
                        if topbar {
                            assert!(!overlap(r, topbar_rect), "{what}: dock under the top bar");
                        }
                        // Same edge: stacked right against the taskbar.
                        if Edge::from(pos) == tb {
                            let gap = work_inset(MON, r, tb);
                            assert_eq!(gap, T, "{what}: stacked on the taskbar");
                        }
                        // Other edges: still flush with the dock's own edge.
                        if Edge::from(pos) != tb {
                            assert_eq!(work_inset(MON, r, Edge::from(pos)), 0, "{what}: on its edge");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hidden_taskbar_gives_the_whole_monitor() {
        for tb in EDGES {
            let area = dock_area(MON, MON, &[bar_on(tb, MON, T)], true, NONE);
            assert_eq!(area, MON);
            assert_eq!(topbar_rect(MON, &[bar_on(tb, MON, T)], true, TOPBAR, false), RECT { bottom: TOPBAR, ..MON });
        }
        // The top bar still counts.
        assert_eq!(dock_area(MON, MON, &[], true, top(TOPBAR)).top, TOPBAR);
    }

    #[test]
    fn stale_work_area_is_still_kept_clear() {
        // Explorer has not re-reserved the strip yet (just restored): the
        // work area is the whole monitor, but the window is there.
        for tb in EDGES {
            let area = dock_area(MON, MON, &[bar_on(tb, MON, T)], false, NONE);
            assert_eq!(work_inset(MON, area, tb), T, "{tb:?}");
        }
    }

    #[test]
    fn secondary_taskbars_count_on_their_own_monitor_only() {
        // Primary at 0..1920 with a bottom taskbar, secondary to its right
        // with a right-edge Shell_SecondaryTrayWnd.
        let m2 = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
        let sec = bar_on(Edge::Right, m2, 40);
        let area = dock_area(m2, work_without(Edge::Right, m2, 40), &[sec], false, NONE);
        assert_eq!(area.right, m2.right - 40);
        assert_eq!(area.bottom, m2.bottom);
        let r = dock_rect(Position::Right, area, DEPTH);
        assert!(!overlap(r, sec) && inside(r, m2));
    }

    #[test]
    fn a_dock_on_every_display_keeps_to_its_own() {
        // Main display with a bottom taskbar and the top bar; a second one
        // to its right with a right-edge secondary taskbar; a third above
        // the main one (negative coordinates) with no taskbar, top bar only.
        let a = MON;
        let b = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
        let c = RECT { left: 0, top: -1080, right: 1920, bottom: 0 };
        let mons = [a, b, c];
        let bars = [bar_on(Edge::Bottom, a, T), bar_on(Edge::Right, b, 40)];
        let works = [work_without(Edge::Bottom, a, T), work_without(Edge::Right, b, 40), c];
        for hiding in [false, true] {
            for pos in POSITIONS {
                let mut docks = Vec::new();
                for (i, &m) in mons.iter().enumerate() {
                    // Each monitor's taskbars only (as taskbar::bars_on).
                    let own: Vec<RECT> = bars.iter().copied().filter(|&b| bar_monitor(b, &mons) == Some(i)).collect();
                    let area = dock_area(m, works[i], &own, hiding, top(TOPBAR));
                    let r = dock_rect(pos, area, DEPTH);
                    assert!(inside(r, m), "{pos:?} dock {i} leaves its display (hiding {hiding})");
                    assert!(r.top >= m.top + TOPBAR, "{pos:?} dock {i} under the top bar");
                    if !hiding {
                        for &bar in &bars {
                            assert!(!overlap(r, bar), "{pos:?} dock {i} over a taskbar");
                        }
                    }
                    docks.push(r);
                }
                for i in 0..docks.len() {
                    for j in i + 1..docks.len() {
                        assert!(!overlap(docks[i], docks[j]), "{pos:?}: docks {i} and {j} overlap");
                    }
                }
            }
        }
    }

    #[test]
    fn slid_away_taskbar_stays_with_its_own_monitor() {
        // A (0..1920) with B to its right; A's right taskbar auto-hidden:
        // 2 px on A, 46 px over B (the review's case).
        let a = MON;
        let b = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
        let mons = [a, b];
        let on_a = hidden_bar(Edge::Right, a, T);
        assert_eq!(bar_monitor(on_a, &mons), Some(0));
        assert_eq!(bar_edge(a, on_a), Some((Edge::Right, T)));
        // A on A: kept clear of the reveal; B gets no phantom left bar.
        let area_a = dock_area(a, a, &[on_a], false, NONE);
        assert_eq!(area_a.right, a.right - T);
        let r = dock_rect(Position::Right, area_a, DEPTH);
        assert!(!overlap(r, bar_on(Edge::Right, a, T)));
        // B's own left taskbar slid away towards A: B.
        let on_b = hidden_bar(Edge::Left, b, T);
        assert_eq!(bar_monitor(on_b, &mons), Some(1));
        assert_eq!(bar_edge(b, on_b), Some((Edge::Left, T)));
        // Stacked vertically, negative coordinates: A above B.
        let up = RECT { left: 0, top: -1080, right: 1920, bottom: 0 };
        let mons = [MON, up];
        assert_eq!(bar_monitor(hidden_bar(Edge::Bottom, up, T), &mons), Some(1));
        assert_eq!(bar_monitor(hidden_bar(Edge::Top, MON, T), &mons), Some(0));
        // Shown taskbars: wholly on one monitor.
        assert_eq!(bar_monitor(bar_on(Edge::Right, a, T), &[a, b]), Some(0));
        assert_eq!(bar_monitor(bar_on(Edge::Left, b, T), &[a, b]), Some(1));
        // Wholly off-screen (no remnant): the nearest monitor.
        let off = RECT { left: 3840, top: 0, right: 3888, bottom: 1080 };
        assert_eq!(bar_monitor(off, &[a, b]), Some(1));
        assert_eq!(bar_monitor(off, &[]), None);
    }

    #[test]
    fn two_bars_on_one_monitor() {
        // Taskbar on the right plus a toolbar-style second bar at the top.
        let bars = [bar_on(Edge::Right, MON, T), bar_on(Edge::Top, MON, 30)];
        let work = RECT { left: 0, top: 30, right: 1920 - T, bottom: 1080 };
        let area = dock_area(MON, work, &bars, false, NONE);
        assert_eq!(area, work);
        assert_eq!(topbar_rect(MON, &bars, false, TOPBAR, false), RECT { left: 0, top: 30, right: 1920 - T, bottom: 30 + TOPBAR });
    }

    #[test]
    fn top_bar_beside_or_below_the_taskbar() {
        let full = RECT { bottom: TOPBAR, ..MON };
        for autohide in [false, true] {
            let bar = |e| if autohide { hidden_bar(e, MON, T) } else { bar_on(e, MON, T) };
            assert_eq!(topbar_rect(MON, &[bar(Edge::Bottom)], false, TOPBAR, false), full);
            assert_eq!(topbar_rect(MON, &[bar(Edge::Top)], false, TOPBAR, false), RECT { top: T, bottom: T + TOPBAR, ..MON });
            assert_eq!(topbar_rect(MON, &[bar(Edge::Left)], false, TOPBAR, false), RECT { left: T, ..full });
            assert_eq!(topbar_rect(MON, &[bar(Edge::Right)], false, TOPBAR, false), RECT { right: 1920 - T, ..full });
        }
        // No taskbar on this monitor.
        assert_eq!(topbar_rect(MON, &[], false, TOPBAR, false), full);
    }

    #[test]
    fn appbar_reservation_reaches_past_an_autohide_taskbar() {
        // Top bar 28 px below a 48 px top taskbar.
        // The taskbar reserves: the shell starts us at 48.
        let q = RECT { top: 48, bottom: 48 + 28, ..MON };
        assert_eq!(reserve_rect(MON, q, Edge::Top, 28, 48 + 28), RECT { top: 48, bottom: 76, ..MON });
        // Auto-hide taskbar: the shell starts us at 0, we still reach 76.
        let q = RECT { top: 0, bottom: 28, ..MON };
        assert_eq!(reserve_rect(MON, q, Edge::Top, 28, 48 + 28), RECT { top: 0, bottom: 76, ..MON });
        // Nothing on top: as before.
        assert_eq!(reserve_rect(MON, q, Edge::Top, 28, 28), RECT { top: 0, bottom: 28, ..MON });
        // Another bar the shell steps around keeps our full thickness.
        let q = RECT { top: 40, bottom: 68, ..MON };
        assert_eq!(reserve_rect(MON, q, Edge::Top, 28, 28).bottom, 68);
        // The other edges, plain thickness.
        let q = bar_on(Edge::Bottom, MON, 70);
        assert_eq!(reserve_rect(MON, q, Edge::Bottom, 70, 0), bar_on(Edge::Bottom, MON, 70));
        let q = bar_on(Edge::Left, MON, 70);
        assert_eq!(reserve_rect(MON, q, Edge::Left, 70, 0), bar_on(Edge::Left, MON, 70));
        let q = bar_on(Edge::Right, MON, 70);
        assert_eq!(reserve_rect(MON, q, Edge::Right, 70, 0), bar_on(Edge::Right, MON, 70));
    }

    #[test]
    fn hide_is_verified_on_the_work_area() {
        for e in EDGES {
            let bar = bar_on(e, MON, T);
            // Auto-hide took: the strip is free.
            let freed = BarSeen { mon: MON, work: MON, bar, own_px: 0 };
            assert_eq!(hide_verdict(&[freed]), HideCheck::Freed, "{e:?}");
            // Refused / ignored (the reported right-edge case): reserved.
            let kept = BarSeen { mon: MON, work: work_without(e, MON, T), bar, own_px: 0 };
            assert_eq!(hide_verdict(&[kept]), HideCheck::Waiting, "{e:?}");
            // Our own dock reserving that edge is not the taskbar.
            let ours = BarSeen { mon: MON, work: work_without(e, MON, 70), bar, own_px: 70 };
            assert_eq!(hide_verdict(&[ours]), HideCheck::Freed, "{e:?} own bar");
            // Our dock stacked next to a taskbar that still reserves.
            let both = BarSeen { mon: MON, work: work_without(e, MON, 70 + T), bar, own_px: 70 };
            assert_eq!(hide_verdict(&[both]), HideCheck::Waiting, "{e:?} stacked");
        }
        // One secondary taskbar still reserving is enough to wait.
        let m2 = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
        let ok = BarSeen { mon: MON, work: MON, bar: bar_on(Edge::Bottom, MON, T), own_px: 0 };
        let stuck = BarSeen { mon: m2, work: work_without(Edge::Bottom, m2, T), bar: bar_on(Edge::Bottom, m2, T), own_px: 0 };
        assert_eq!(hide_verdict(&[ok, stuck]), HideCheck::Waiting);
        // No taskbar seen at all (explorer starting): never "freed".
        assert_eq!(hide_verdict(&[]), HideCheck::Waiting);
        // A slid-away taskbar with a full work area is freed.
        let slid = BarSeen { mon: MON, work: MON, bar: hidden_bar(Edge::Right, MON, T), own_px: 0 };
        assert_eq!(hide_verdict(&[slid]), HideCheck::Freed);
    }

    #[test]
    fn restore_is_verified() {
        use Restore::*;
        // Back to the saved "always shown" state, all windows visible and in place.
        assert_eq!(restore_verdict(Some(0), 0, 2, 2, 2), Done);
        // The user's own auto-hide comes back as auto-hide (slid away is fine).
        assert_eq!(restore_verdict(Some(1), 1, 1, 1, 0), Done);
        assert_eq!(restore_verdict(Some(1), 0, 1, 1, 1), NotBack);
        // Still in our auto-hide state.
        assert_eq!(restore_verdict(Some(0), 1, 1, 1, 0), NotBack);
        // A window still hidden.
        assert_eq!(restore_verdict(Some(0), 0, 2, 1, 1), NotBack);
        // No taskbar at all (explorer gone): cannot be confirmed.
        assert_eq!(restore_verdict(Some(0), 0, 0, 0, 0), NotBack);
        // Never hidden by us: only visibility counts.
        assert_eq!(restore_verdict(None, 1, 1, 1, 0), Done);
        assert_eq!(restore_verdict(None, 0, 1, 1, 0), Done);
        // Other state bits (always-on-top on old Windows) do not matter.
        assert_eq!(restore_verdict(Some(2), 0, 1, 1, 1), Done);
        // Shown and "always shown" again, but left where auto-hide slid it:
        // the empty band. Not done.
        assert_eq!(restore_verdict(Some(0), 0, 1, 1, 0), NotInPlace);
        assert_eq!(restore_verdict(Some(0), 0, 2, 2, 1), NotInPlace);
    }

    #[test]
    fn seated_taskbars_and_slid_away_ones() {
        for e in EDGES {
            assert!(bar_seated(MON, bar_on(e, MON, T)), "{e:?} in place");
            assert!(!bar_seated(MON, hidden_bar(e, MON, T)), "{e:?} slid away");
        }
        // Measured on the 2560x1600 test PC: in place, and slid away (the
        // window explorer leaves visible-but-off-screen).
        let m = RECT { left: 0, top: 0, right: 2560, bottom: 1600 };
        assert!(bar_seated(m, RECT { left: 0, top: 1536, right: 2560, bottom: 1600 }));
        assert!(!bar_seated(m, RECT { left: 0, top: 1598, right: 2560, bottom: 1662 }));
        // A 1 px rounding overhang still counts as in place.
        assert!(bar_seated(m, RECT { left: -1, top: 1537, right: 2561, bottom: 1601 }));
        assert!(!bar_seated(m, RECT::default()));
    }

    /// The dock window never reaches past its area (the work area while
    /// the taskbar is kept, the monitor while it is hidden), for every
    /// edge, odd and even depths and every scale. Measured on 2560x1600 at
    /// 200%: work bottom 1536 -> dock bottom 1536 in physical px. (A
    /// DPI-unaware probe shows 769 / 801 for an odd depth: it rounds the
    /// top and the height up separately, 1317/2 -> 659 plus 219/2 -> 110.)
    #[test]
    fn dock_stays_inside_its_area() {
        let m = RECT { left: 0, top: 0, right: 2560, bottom: 1600 };
        let work = RECT { bottom: 1536, ..m };
        let bar = RECT { top: 1536, ..m };
        for scale in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.25, 3.0] {
            for depth_dip in [100.0f32, 109.5, 110.0, 120.3] {
                let depth = (depth_dip * scale).ceil() as i32;
                for pos in POSITIONS {
                    for (hiding, bars, topbar) in [(false, vec![bar], 0), (false, vec![bar], 56), (true, vec![], 0), (true, vec![], 56)] {
                        let area = dock_area(m, work, &bars, hiding, top(topbar));
                        let r = dock_rect(pos, area, depth);
                        let what = format!("scale {scale} depth {depth} {pos:?} hiding {hiding} topbar {topbar}: {r:?} in {area:?}");
                        assert!(inside(r, area), "{what}");
                        assert!(inside(r, if hiding { m } else { work }), "{what}");
                        assert!(hiding || !overlap(r, bar), "{what}: over the taskbar");
                        match pos {
                            Position::Bottom => assert_eq!(r.bottom, area.bottom, "{what}"),
                            Position::Left => assert_eq!(r.left, area.left, "{what}"),
                            Position::Right => assert_eq!(r.right, area.right, "{what}"),
                        }
                    }
                }
            }
        }
        // The top bar likewise stays on the monitor, below / beside a kept bar.
        for (bars, hiding) in [(vec![bar], false), (vec![], true)] {
            let t = topbar_rect(m, &bars, hiding, 56, false);
            assert!(inside(t, m) && !overlap(t, bar), "{t:?}");
        }
    }

    /// The bar's space at the bottom: its whole strip, from the monitor's
    /// bottom edge up to its top (as topbar/mod.rs publishes it).
    fn below(bar: RECT, mon: RECT) -> BarSpace {
        BarSpace { bottom: mon.bottom - bar.top, ..NONE }
    }

    #[test]
    fn bottom_bar_above_or_beside_the_taskbar() {
        let full = RECT { top: 1080 - TOPBAR, ..MON };
        for autohide in [false, true] {
            let bar = |e| if autohide { hidden_bar(e, MON, T) } else { bar_on(e, MON, T) };
            // The mirror of the top: above a bottom taskbar, a top one ignored.
            assert_eq!(topbar_rect(MON, &[bar(Edge::Top)], false, TOPBAR, true), full);
            assert_eq!(topbar_rect(MON, &[bar(Edge::Bottom)], false, TOPBAR, true), RECT { top: 1080 - T - TOPBAR, bottom: 1080 - T, ..MON });
            assert_eq!(topbar_rect(MON, &[bar(Edge::Left)], false, TOPBAR, true), RECT { left: T, ..full });
            assert_eq!(topbar_rect(MON, &[bar(Edge::Right)], false, TOPBAR, true), RECT { right: 1920 - T, ..full });
            // Never over the taskbar, shown or revealed.
            for e in EDGES {
                let r = topbar_rect(MON, &[bar(e)], false, TOPBAR, true);
                assert!(!overlap(r, bar_on(e, MON, T)) && inside(r, MON), "{e:?} autohide={autohide}");
            }
        }
        // No taskbar, or NopBar hides it: the whole bottom edge.
        assert_eq!(topbar_rect(MON, &[], false, TOPBAR, true), full);
        for e in EDGES {
            assert_eq!(topbar_rect(MON, &[bar_on(e, MON, T)], true, TOPBAR, true), full, "{e:?}");
        }
        // A display above the main one (negative coordinates).
        let up = RECT { left: 0, top: -1080, right: 1920, bottom: 0 };
        assert_eq!(topbar_rect(up, &[], false, TOPBAR, true), RECT { top: -TOPBAR, bottom: 0, ..up });
    }

    /// Every combination with the bar at the bottom: taskbar edge x
    /// auto-hide x hidden by NopBar x dock edge. The dock never covers the
    /// bar or the taskbar; a bottom dock sits right on the bar (its
    /// auto-hide strip, its window's bottom row, just above it); side docks
    /// keep above it and stay on their edge.
    #[test]
    fn dock_sits_right_above_a_bottom_bar() {
        for tb in EDGES {
            for autohide in [false, true] {
                for hiding in [false, true] {
                    let seen = if autohide { hidden_bar(tb, MON, T) } else { bar_on(tb, MON, T) };
                    let bars: Vec<RECT> = if hiding { Vec::new() } else { vec![seen] };
                    let bar = topbar_rect(MON, &bars, hiding, TOPBAR, true);
                    // The bar reserves its strip (and the taskbar its own).
                    let mut work = if autohide || hiding { MON } else { work_without(tb, MON, T) };
                    work.bottom = work.bottom.min(bar.top);
                    for pos in POSITIONS {
                        let area = dock_area(MON, work, &bars, hiding, below(bar, MON));
                        let r = dock_rect(pos, area, DEPTH);
                        let what = format!("taskbar {tb:?} autohide={autohide} hiding={hiding} dock {pos:?}: {r:?} bar {bar:?}");
                        assert!(!overlap(r, bar), "{what}: over the bar");
                        assert!(inside(r, MON), "{what}");
                        if !hiding {
                            assert!(!overlap(r, bar_on(tb, MON, T)), "{what}: over the taskbar");
                        }
                        match pos {
                            Position::Bottom => {
                                assert_eq!(r.bottom, bar.top, "{what}: right above the bar");
                                // The auto-hide strip: the window's bottom row.
                                let strip = RECT { top: r.bottom - 1, ..r };
                                assert!(!overlap(strip, bar) && strip.bottom == bar.top, "{what}: strip");
                            }
                            _ => {
                                assert!(r.bottom <= bar.top, "{what}: side dock above the bar");
                                let on_edge = if !hiding && Edge::from(pos) == tb { T } else { 0 };
                                assert_eq!(work_inset(MON, r, Edge::from(pos)), on_edge, "{what}: on its edge");
                            }
                        }
                    }
                }
            }
        }
        // Even when the bar reserves nothing (the work area is the whole
        // monitor), its strip still counts at the bottom.
        let bar = topbar_rect(MON, &[], true, TOPBAR, true);
        let r = dock_rect(Position::Bottom, dock_area(MON, MON, &[], true, below(bar, MON)), DEPTH);
        assert_eq!(r.bottom, bar.top);
        // A bar on top changes nothing at the bottom (as before).
        let r = dock_rect(Position::Bottom, dock_area(MON, MON, &[], true, top(TOPBAR)), DEPTH);
        assert_eq!(r.bottom, MON.bottom);
    }

    #[test]
    fn three_displays_each_with_its_bar_at_the_bottom() {
        // Left, main and right, the right one taller and offset; a bottom
        // taskbar on the main one only.
        let a = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        let b = MON;
        let c = RECT { left: 1920, top: -200, right: 4480, bottom: 1240 };
        let mons = [a, b, c];
        let taskbar = bar_on(Edge::Bottom, b, T);
        for hiding in [false, true] {
            for pos in POSITIONS {
                for (i, &m) in mons.iter().enumerate() {
                    let own: Vec<RECT> = if hiding { Vec::new() } else { [taskbar].into_iter().filter(|&t| bar_monitor(t, &mons) == Some(i)).collect() };
                    let bar = topbar_rect(m, &own, hiding, TOPBAR, true);
                    let floor = if own.is_empty() { m.bottom } else { m.bottom - T };
                    assert!(inside(bar, m) && bar.bottom == floor, "bar {i} hiding={hiding}: {bar:?}");
                    let mut work = if own.is_empty() { m } else { work_without(Edge::Bottom, m, T) };
                    work.bottom = work.bottom.min(bar.top);
                    let r = dock_rect(pos, dock_area(m, work, &own, hiding, below(bar, m)), DEPTH);
                    assert!(inside(r, m) && !overlap(r, bar), "{pos:?} dock {i} hiding={hiding}: {r:?}");
                    if pos == Position::Bottom {
                        assert_eq!(r.bottom, bar.top, "{pos:?} dock {i}");
                    }
                }
            }
        }
    }

    /// The bar at the bottom as topbar/mod.rs publishes it: `h` px tall,
    /// `beyond` px of taskbar under it, reserving its strip or not.
    fn bottom_bar(h: i32, beyond: i32, reserves: bool) -> BarSpace {
        BarSpace { top: 0, bottom: beyond + h, bottom_free: if reserves { 0 } else { h } }
    }

    /// The shell's stacking on one bottom edge: AppBars in the order it
    /// keeps them, each queried right inside the strips before it, then
    /// sized as reserve_now does (reserve_rect). Returns each one's rect
    /// and how far in the work area ends.
    fn shell_stack(mon: RECT, bars: &[(i32, i32)]) -> (Vec<RECT>, i32) {
        let mut floor = mon.bottom;
        let mut got = Vec::new();
        for &(thickness, reach) in bars {
            let queried = RECT { top: floor - thickness, bottom: floor, ..mon };
            let r = reserve_rect(mon, queried, Edge::Bottom, thickness, if thickness > 0 { reach } else { 0 });
            if thickness > 0 {
                floor = floor.min(r.top);
            }
            got.push(r);
        }
        (got, mon.bottom - floor)
    }

    #[test]
    fn a_bottom_dock_reserves_above_a_bottom_bar() {
        // The bar reserves its own strip: the dock its thickness only.
        assert_eq!(dock_reserve(Position::Bottom, 70, bottom_bar(32, 0, true)), DockReserve { reach: 0, own: 70 });
        // It reserves nothing: the dock reaches past it, and that strip
        // is ours.
        assert_eq!(dock_reserve(Position::Bottom, 70, bottom_bar(32, 0, false)), DockReserve { reach: 102, own: 102 });
        // A taskbar under the bar is reached past, but is not ours.
        assert_eq!(dock_reserve(Position::Bottom, 70, bottom_bar(32, 48, false)), DockReserve { reach: 150, own: 102 });
        // Auto-hide reserves nothing; side docks and a bar on top: plain.
        assert_eq!(dock_reserve(Position::Bottom, 0, bottom_bar(32, 0, false)), DockReserve { reach: 0, own: 0 });
        assert_eq!(dock_reserve(Position::Left, 70, bottom_bar(32, 0, false)), DockReserve { reach: 0, own: 70 });
        assert_eq!(dock_reserve(Position::Right, 70, bottom_bar(32, 0, true)), DockReserve { reach: 0, own: 70 });
        assert_eq!(dock_reserve(Position::Bottom, 70, top(32)), DockReserve { reach: 0, own: 70 });
        // The shell steps around the bar: the dock's strip sits on it.
        let q = RECT { top: 1080 - 32 - 70, bottom: 1080 - 32, ..MON };
        assert_eq!(reserve_rect(MON, q, Edge::Bottom, 70, 102), q);
        // It does not: the strip still reaches past the bar.
        let q = RECT { top: 1080 - 70, ..MON };
        assert_eq!(reserve_rect(MON, q, Edge::Bottom, 70, 102), RECT { top: 1080 - 102, ..MON });
        // The bar above an auto-hide bottom taskbar: up to its own top.
        let q = RECT { top: 1080 - 28, ..MON };
        assert_eq!(reserve_rect(MON, q, Edge::Bottom, 28, 48 + 28), RECT { top: 1080 - 76, ..MON });
    }

    /// Whichever order the shell keeps our two bottom AppBars in (the bar
    /// registers first at startup and after an explorer restart, and
    /// keeps its place when it moves top -> bottom live; a shell could
    /// still stack them the other way), the work area ends on the dock's
    /// top, as drawn: no band, nothing under the dock.
    #[test]
    fn two_bottom_appbars_end_the_work_area_on_the_dock_either_way() {
        const H: i32 = 32;
        const D: i32 = 70;
        for reserves in [true, false] {
            let space = bottom_bar(H, 0, reserves);
            let bar_rect = topbar_rect(MON, &[], true, H, true);
            let drawn = dock_rect(Position::Bottom, dock_area(MON, MON, &[], true, space), DEPTH);
            let r = dock_reserve(Position::Bottom, D, space);
            let bar = (if reserves { H } else { 0 }, if reserves { H } else { 0 });
            let dock = (D, r.reach);
            for (name, order) in [("bar first", [bar, dock]), ("dock first", [dock, bar])] {
                let (_, inset) = shell_stack(MON, &order);
                assert_eq!(inset, H + D, "reserves={reserves} {name}");
                // The dock's icons end on the bar; its reserved part (D
                // of its DEPTH window) is what the work area keeps.
                assert_eq!(drawn.bottom, bar_rect.top);
                assert_eq!(MON.bottom - inset, drawn.bottom - D, "reserves={reserves} {name}");
            }
        }
        // Why no reach over a bar that reserves: stacked dock first, the
        // bar would go on top of the dock's reach and leave a band.
        let (_, inset) = shell_stack(MON, &[(D, H + D), (H, H)]);
        assert_eq!(inset, H + D + H);
        // The bar's own reach past a kept auto-hide taskbar (48) stays
        // whole with the dock stacked in either order.
        for order in [[(H, 48 + H), (D, 0)], [(D, 0), (H, 48 + H)]] {
            assert!(shell_stack(MON, &order).1 >= 48 + H, "{order:?}");
        }
    }

    /// The hide check after NopBar hid the taskbar, through what the dock
    /// and the bar reserve (dock_reserve, own_inset) and the shell's
    /// stacking: freed whenever explorer really let go of its strip, for
    /// the bar reserving or not and the dock hiding itself or not, on each
    /// of three displays; still waiting while the taskbar keeps its strip.
    #[test]
    fn the_hide_check_counts_what_the_dock_reserves_for_the_bar() {
        const H: i32 = 32;
        const D: i32 = 70;
        let mons = [MON, RECT { left: 1920, top: 0, right: 3840, bottom: 1080 }, RECT { left: -1920, top: -200, right: 0, bottom: 880 }];
        for reserves in [true, false] {
            for dock_autohide in [false, true] {
                let thickness = if dock_autohide { 0 } else { D };
                let space = bottom_bar(H, 0, reserves);
                let mut own = Vec::new();
                let mut works = Vec::new();
                for (i, &m) in mons.iter().enumerate() {
                    // The dock on the first display only (one dock); a
                    // bar at the bottom of every display.
                    let bar_t = if reserves { H } else { 0 };
                    if bar_t > 0 {
                        own.push((m, Edge::Bottom, bar_t));
                    }
                    let mut stack = vec![(bar_t, bar_t)];
                    if i == 0 && thickness > 0 {
                        let r = dock_reserve(Position::Bottom, thickness, space);
                        own.push((m, Edge::Bottom, r.own));
                        stack.push((thickness, r.reach));
                    }
                    let (_, inset) = shell_stack(m, &stack);
                    works.push(RECT { bottom: m.bottom - inset, ..m });
                }
                for (i, &m) in mons.iter().enumerate() {
                    let what = format!("bar reserves={reserves} dock autohide={dock_autohide} display {i}");
                    let own_px = own_inset(&own, m, Edge::Bottom);
                    // Auto-hide took: the taskbar slid away, reserving nothing.
                    let freed = BarSeen { mon: m, work: works[i], bar: hidden_bar(Edge::Bottom, m, T), own_px };
                    assert_eq!(hide_verdict(&[freed]), HideCheck::Freed, "{what}");
                    // Refused: the taskbar still reserves its 48 px under ours.
                    let kept = BarSeen { work: RECT { bottom: works[i].bottom - T, ..works[i] }, bar: bar_on(Edge::Bottom, m, T), ..freed };
                    assert_eq!(hide_verdict(&[kept]), HideCheck::Waiting, "{what}: a kept strip still shows");
                }
            }
        }
        // The review's case: 102 px reserved by the dock for itself and the
        // bar, 70 counted before; now 102 are ours.
        let r = dock_reserve(Position::Bottom, D, bottom_bar(H, 0, false));
        let seen = BarSeen { mon: MON, work: RECT { bottom: 1080 - r.reach, ..MON }, bar: hidden_bar(Edge::Bottom, MON, T), own_px: r.own };
        assert_eq!(hide_verdict(&[seen]), HideCheck::Freed);
        assert_eq!(hide_verdict(&[BarSeen { own_px: D, ..seen }]), HideCheck::Waiting, "the old count");
        // Other edges and displays do not add up.
        let own = [(MON, Edge::Bottom, 70), (MON, Edge::Top, 32), (mons[1], Edge::Bottom, 40)];
        assert_eq!(own_inset(&own, MON, Edge::Bottom), 70);
        assert_eq!(own_inset(&own, mons[1], Edge::Bottom), 40);
        assert_eq!(own_inset(&own, mons[2], Edge::Bottom), 0);
    }

    #[test]
    fn setpos_only_when_the_strip_changes() {
        let a = RECT { top: 1048, ..MON };
        assert!(setpos_needed(None, Edge::Bottom, a), "first time after registering");
        assert!(!setpos_needed(Some((Edge::Bottom, a)), Edge::Bottom, a), "an echo of another bar's change");
        assert!(setpos_needed(Some((Edge::Top, a)), Edge::Bottom, a), "moved edges");
        assert!(setpos_needed(Some((Edge::Bottom, a)), Edge::Bottom, RECT { top: 978, ..a }));
        assert_eq!(seat_text("dock", Edge::Bottom, a, a), "dock bottom (0,1048)-(1920,1080)");
        assert_eq!(seat_text("top bar", Edge::Bottom, a, RECT { top: 978, ..a }), "top bar bottom (0,978)-(1920,1080) (asked (0,1048)-(1920,1080))");
    }

    #[test]
    fn things_hang_under_a_top_bar_and_over_a_bottom_one() {
        let work = RECT { left: 0, top: 32, right: 1920, bottom: 1080 };
        let cell = RECT { left: 100, top: 0, right: 140, bottom: 32 };
        // Bar on top: under the cell.
        assert_eq!(hang_y(cell, 300, 2, false, work), 34);
        // Bar at the bottom: over the cell, its bottom `gap` above it.
        let work_b = RECT { left: 0, top: 0, right: 1920, bottom: 1048 };
        let cell_b = RECT { left: 100, top: 1048, right: 140, bottom: 1080 };
        let y = hang_y(cell_b, 300, 2, true, work_b);
        assert_eq!((y, y + 300), (746, 1046));
        // Never past the work area: taller than the room over the cell...
        assert_eq!(hang_y(cell_b, 2000, 2, true, work_b), 0);
        // ...or a cell in the middle of the screen (a panel would run off
        // the bottom).
        let mid = RECT { top: 900, bottom: 932, ..cell };
        assert_eq!(hang_y(mid, 300, 2, false, work_b), 1048 - 300);
        let low = RECT { top: 100, bottom: 132, ..cell };
        assert_eq!(hang_y(low, 300, 2, true, work_b), 0, "no room above: down to the top edge");
        // Negative coordinates (a display above the main one).
        let w = RECT { left: 0, top: -1080, right: 1920, bottom: -32 };
        let c = RECT { left: 0, top: -32, right: 40, bottom: 0 };
        assert_eq!(hang_y(c, 100, 4, true, w), -136);
    }

    #[test]
    fn every_panel_hangs_the_same_gap_from_the_bar() {
        // 6 DIP (macOS's menus under the menu bar) at every scale, rounded
        // to whole px.
        assert_eq!(POPUP_GAP_DIP, 6.0);
        let gaps: Vec<i32> = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0].iter().map(|&s| popup_gap(s)).collect();
        assert_eq!(gaps, [6, 8, 9, 11, 12, 15, 18]);
        // A bar on top reserving its strip: the panel's top edge is the gap
        // under the bar's bottom edge, and that much room is left under it.
        let work = RECT { left: 0, top: 32, right: 1920, bottom: 1080 };
        let cell = RECT { left: 100, top: 0, right: 140, bottom: 32 };
        let gap = popup_gap(1.0);
        assert_eq!(hang_edge(cell, gap, false, work), 38);
        assert_eq!(hang_y(cell, 300, gap, false, work), 38);
        assert_eq!(hang_room(cell, gap, false, work), 1080 - 38);
        // A panel exactly that tall keeps the gap (not moved up over it).
        assert_eq!(hang_y(cell, hang_room(cell, gap, false, work), gap, false, work), 38);
        // A bar at the bottom: the panel's bottom edge the gap over the bar.
        let work_b = RECT { left: 0, top: 0, right: 1920, bottom: 1048 };
        let cell_b = RECT { left: 100, top: 1048, right: 140, bottom: 1080 };
        let gap2 = popup_gap(2.0);
        assert_eq!(hang_edge(cell_b, gap2, true, work_b), 1048 - 12);
        let y = hang_y(cell_b, 300, gap2, true, work_b);
        assert_eq!((y, y + 300), (1048 - 12 - 300, 1048 - 12));
        assert_eq!(hang_room(cell_b, gap2, true, work_b), 1048 - 12);
        let room = hang_room(cell_b, gap2, true, work_b);
        assert_eq!(hang_y(cell_b, room, gap2, true, work_b) + room, 1048 - 12, "a full-height panel keeps it too");
        // The same gap at either edge.
        assert_eq!(hang_y(cell, 300, gap2, false, work) - cell.bottom, cell_b.top - (hang_y(cell_b, 300, gap2, true, work_b) + 300));
    }

    #[test]
    fn the_gap_counts_from_what_is_really_there() {
        let cell = RECT { left: 100, top: 0, right: 140, bottom: 32 };
        // A bar that reserves nothing (the work area runs under it): still
        // under the bar, with the room under it only.
        let mon = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        assert_eq!(hang_edge(cell, 8, false, mon), 40);
        assert_eq!(hang_room(cell, 8, false, mon), 1040);
        // Another bar reserving under ours: the gap under that one.
        let lower = RECT { top: 60, ..mon };
        assert_eq!(hang_edge(cell, 8, false, lower), 68);
        assert_eq!(hang_room(cell, 8, false, lower), 1080 - 68);
        // A bar at the bottom with the dock over it (the work area ends at
        // the dock's top): the gap over the dock, not over the bar under it.
        let cell_b = RECT { left: 100, top: 1048, right: 140, bottom: 1080 };
        let over_dock = RECT { bottom: 960, ..mon };
        assert_eq!(hang_edge(cell_b, 8, true, over_dock), 952);
        assert_eq!(hang_room(cell_b, 8, true, over_dock), 952);
        assert_eq!(hang_y(cell_b, 300, 8, true, over_dock), 652);
        // A pill inside the bar (the island), widened to the bar's height
        // first, hangs like any cell.
        let pill = RECT { left: 900, top: 4, right: 1000, bottom: 28 };
        let widened = RECT { top: cell.top, bottom: cell.bottom, ..pill };
        assert_eq!(hang_y(widened, 200, 8, false, RECT { top: 32, ..mon }), 40);
        // Room is never more than the work area, never under 1 px.
        assert_eq!(room_from(2000, false, mon), 1);
        assert_eq!(room_from(-500, false, mon), 1080);
        assert_eq!(room_from(-500, true, mon), 1);
        assert_eq!(room_from(5000, true, mon), 1080);
    }
}
