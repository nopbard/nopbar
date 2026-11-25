//! Explorer keeps its own pitch and published origin. Widgets use that
//! pitch to choose a cell count, then fill each work area with equal
//! margins and gaps. Placement compares actual rectangles in screen pixels.

use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;

use windows::Win32::Foundation::{POINT, RECT};

use crate::monitors::Monitor;

/// A cell: (column, row) from a display's grid origin.
pub type Cell = (i32, i32);

pub const WIDGET_GAP: f32 = 16.;

/// The desktop as Explorer shows it (desklayer::icons).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Desk {
    /// The icons' spacing (a cell), px.
    pub cell: (i32, i32),
    /// DPI of the source view; invalidates cached origins, never rescales physical cells.
    pub spacing_dpi: u32,
    /// Stable per-monitor geometry, published with this reading.
    pub layout: Vec<(Monitor, Option<Grid>)>,
    /// Explorer align-to-grid flag, included in change detection.
    pub align: bool,
    /// Each icon's place: its box's top left, screen px.
    pub icons: Vec<POINT>,
    /// "Auto arrange icons" is on: Explorer places every icon itself.
    pub auto_arrange: bool,
    /// The icons show at all ("Show desktop icons").
    pub shown: bool,
    /// The icon list is mirrored (a right-to-left desktop): icons flow
    /// from the right, so widgets keep to the left.
    pub rtl: bool,
    /// Read completely and surely (every icon's place, the folder's flags,
    /// a plain left-to-right list): only then may icons be moved. A desk
    /// read with any doubt still places widgets clear of what it saw.
    pub movable: bool,
}

/// One display's grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Grid {
    /// The top left of cell (0, 0), px.
    pub ox: i32,
    pub oy: i32,
    pub cw: i32,
    pub ch: i32,
    /// Whole cells inside the work area.
    pub cols: i32,
    pub rows: i32,
    // Distribute fractional pixels over the whole axis.
    pub extra_x: i32,
    pub extra_y: i32,
    pub gap: i32,
}

/// A block of cells: its first column and row, its width and height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub c: i32,
    pub r: i32,
    pub w: i32,
    pub h: i32,
}

impl Span {
    #[cfg(test)]
    pub fn contains(&self, (c, r): Cell) -> bool {
        c >= self.c && c < self.c + self.w && r >= self.r && r < self.r + self.h
    }

    #[cfg(test)]
    pub fn cells(self) -> impl Iterator<Item = Cell> {
        (self.c..self.c + self.w).flat_map(move |c| (self.r..self.r + self.h).map(move |r| (c, r)))
    }
}

impl Grid {
    /// The grid of a display with work area `work`, cells `cell` big,
    /// starting `residue` px in from the work area's corner (None: at the
    /// corner). None when not even one cell fits.
    pub fn on(work: RECT, cell: (i32, i32), residue: Option<(i32, i32)>) -> Option<Grid> {
        let (cw, ch) = cell;
        if cw <= 0 || ch <= 0 {
            return None;
        }
        let (rx, ry) = residue.unwrap_or((0, 0));
        let (ox, oy) = (work.left + rx.rem_euclid(cw), work.top + ry.rem_euclid(ch));
        let (cols, rows) = ((work.right - ox) / cw, (work.bottom - oy) / ch);
        (cols >= 1 && rows >= 1).then_some(Grid { ox, oy, cw, ch, cols, rows, ..Default::default() })
    }

    pub fn widget(m: &Monitor, icon_pitch: (i32, i32)) -> Option<Self> {
        let gap = (WIDGET_GAP * m.scale()).round() as i32;
        let axis = |length: i32, pitch: i32| {
            let space = length - gap;
            let count = space.checked_div(pitch)?;
            (pitch > 0 && count > 0 && space > gap).then(|| {
                (count, space / count, space % count)
            })
        };
        let (cols, cw, extra_x) = axis(m.work.right - m.work.left, icon_pitch.0)?;
        let (rows, ch, extra_y) = axis(m.work.bottom - m.work.top, icon_pitch.1)?;
        Some(Self {
            ox: m.work.left + gap, oy: m.work.top + gap, cw, ch,
            cols, rows, extra_x, extra_y, gap,
        })
    }

    /// The cell under screen point (x, y), if inside the grid.
    pub fn cell_at(&self, x: i32, y: i32) -> Option<Cell> {
        let (c, r) = (self.column_at(x), self.row_at(y));
        (c >= 0 && r >= 0 && c < self.cols && r < self.rows).then_some((c, r))
    }

    /// Cell of the closest grid anchor to the carried card's top left.
    pub(super) fn drag_cell(&self, at: RECT) -> Cell {
        (
            self.column_at(at.left + self.cw / 2).clamp(0, self.cols - 1),
            self.row_at(at.top + self.ch / 2).clamp(0, self.rows - 1),
        )
    }

    fn axis_offset(index: i32, pitch: i32, extra: i32, count: i32) -> i32 {
        let remainder = i64::from(index) * i64::from(extra);
        index * pitch + ((remainder + i64::from(count / 2)) / i64::from(count)) as i32
    }

    fn axis_index(pixel: i32, count: i32, edge: impl Fn(i32) -> i32) -> i32 {
        if pixel < edge(0) { return -1; }
        if pixel >= edge(count) { return count; }
        let (mut lo, mut hi) = (0, count);
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if pixel < edge(mid) { hi = mid; } else { lo = mid; }
        }
        lo
    }

    fn column_at(&self, x: i32) -> i32 {
        if self.extra_x == 0 { return (x - self.ox).div_euclid(self.cw); }
        Self::axis_index(x, self.cols, |i| self.cell_pos((i, 0)).x)
    }

    fn row_at(&self, y: i32) -> i32 {
        if self.extra_y == 0 { return (y - self.oy).div_euclid(self.ch); }
        Self::axis_index(y, self.rows, |i| self.cell_pos((0, i)).y)
    }

    /// The cell an icon whose box starts at `p` sits in (by its box's
    /// centre, so a few pixels off the grid still count).
    #[cfg(test)]
    pub fn icon_cell(&self, p: POINT) -> Option<Cell> {
        self.cell_at(p.x + self.cw / 2, p.y + self.ch / 2)
    }

    /// The top left of cell `c` (where an icon placed there starts).
    pub fn cell_pos(&self, (c, r): Cell) -> POINT {
        POINT {
            x: self.ox + Self::axis_offset(c, self.cw, self.extra_x, self.cols),
            y: self.oy + Self::axis_offset(r, self.ch, self.extra_y, self.rows),
        }
    }

    /// Span `s` on screen, `inset` px in from its cells' edges.
    pub fn rect(&self, s: Span, inset: i32) -> RECT {
        let p = self.cell_pos((s.c, s.r));
        let end = self.cell_pos((s.c + s.w, s.r + s.h));
        RECT { left: p.x + inset, top: p.y + inset, right: end.x - self.gap - inset, bottom: end.y - self.gap - inset }
    }

    /// The cells rectangle `r` touches, inside the grid (None: none).
    pub fn touched(&self, r: RECT) -> Option<Span> {
        if r.right <= r.left || r.bottom <= r.top {
            return None;
        }
        let c0 = self.column_at(r.left).max(0);
        let r0 = self.row_at(r.top).max(0);
        let c1 = self.column_at(r.right - 1).min(self.cols - 1);
        let r1 = self.row_at(r.bottom - 1).min(self.rows - 1);
        (c1 >= c0 && r1 >= r0).then_some(Span { c: c0, r: r0, w: c1 - c0 + 1, h: r1 - r0 + 1 })
    }

    /// A widget whose cells are `size` px (its window plus `inset` on each
    /// side) in whole cells: the nearest count, never so few that the
    /// window (the cells less the insets) is under `min`, never over the
    /// grid (a grid smaller than that: the whole grid).
    #[cfg(test)]
    pub fn span_for(&self, size: (i32, i32), min: (i32, i32), inset: i32) -> (i32, i32) {
        let fit = |v: i32, m: i32, cell: i32, most: i32| {
            let near = (v as f32 / cell as f32).round() as i32;
            let least = (m + 2 * inset + cell - 1) / cell;
            near.max(least).max(1).min(most)
        };
        (fit(size.0, min.0, self.cw, self.cols), fit(size.1, min.1, self.ch, self.rows))
    }

    /// Widget `r` (drawn `inset` px in from its cells) snapped: its size in
    /// whole cells, its corner on the nearest cell, kept inside the grid.
    #[cfg(test)]
    pub fn snap(&self, r: RECT, min: (i32, i32), inset: i32) -> Span {
        let (w, h) = self.span_for((r.right - r.left + 2 * inset, r.bottom - r.top + 2 * inset), min, inset);
        let c = (((r.left - inset - self.ox) as f32 / self.cw as f32).round() as i32).clamp(0, self.cols - w);
        let rr = (((r.top - inset - self.oy) as f32 / self.ch as f32).round() as i32).clamp(0, self.rows - h);
        Span { c, r: rr, w, h }
    }

    fn all(&self) -> impl Iterator<Item = Cell> + '_ {
        (0..self.cols).flat_map(move |c| (0..self.rows).map(move |r| (c, r)))
    }
}

/// Pure: where the cells start on a display, from the icons on it: the
/// most common offset (px) of their boxes from the work area's corner,
/// modulo the cell. None without icons.
pub fn residue(icons: &[POINT], work: RECT, cell: (i32, i32)) -> Option<(i32, i32)> {
    if icons.is_empty() || cell.0 <= 0 || cell.1 <= 0 {
        return None;
    }
    let mode = |vals: Vec<i32>| {
        let mut n: HashMap<i32, usize> = HashMap::new();
        for v in vals {
            *n.entry(v).or_default() += 1;
        }
        n.into_iter().max_by_key(|&(v, k)| (k, -v)).map(|(v, _)| v).unwrap_or(0)
    };
    let rx = mode(icons.iter().map(|p| (p.x - work.left).rem_euclid(cell.0)).collect());
    let ry = mode(icons.iter().map(|p| (p.y - work.top).rem_euclid(cell.1)).collect());
    Some((rx, ry))
}

/// Pure: the display an icon at `p` (its box `cell` big) is on: the one
/// holding its box's centre.
#[cfg(test)]
fn icon_monitor(p: POINT, cell: (i32, i32), mons: &[Monitor]) -> Option<usize> {
    let (x, y) = (p.x + cell.0 / 2, p.y + cell.1 / 2);
    mons.iter().position(|m| x >= m.rect.left && x < m.rect.right && y >= m.rect.top && y < m.rect.bottom)
}

/// Pure: every monitor uses the one view's physical spacing. Empty monitors
/// start at their own work origin. Align-to-grid never samples movable icons.
pub fn grids(desk: &Desk, mons: &[Monitor]) -> Vec<Option<Grid>> {
    mons.iter().map(|m| {
        // Gallery/drag overlays ask for a single monitor from the cached layout.
        if let Some((_, g)) = desk.layout.iter().find(|(cached, _)| cached == m) { return *g; }
        let on: Vec<_> = desk.icons.iter().copied().filter(|p| {
            let (x, y) = (p.x + desk.cell.0 / 2, p.y + desk.cell.1 / 2);
            x >= m.rect.left && x < m.rect.right && y >= m.rect.top && y < m.rect.bottom
        }).collect();
        let origin = if desk.align { None } else { residue(&on, m.work, desk.cell) };
        Grid::on(m.work, desk.cell, origin)
    }).collect()
}

/// Only layout settings or monitor geometry invalidate an established origin.
pub fn stabilize(desk: &mut Desk, previous: Option<&Desk>, mons: &[Monitor]) {
    desk.layout.clear();
    if let Some(old) = previous.filter(|old| old.cell == desk.cell
        && old.spacing_dpi == desk.spacing_dpi && old.align == desk.align
        && old.auto_arrange == desk.auto_arrange
        && old.layout.len() == mons.len()
        && old.layout.iter().zip(mons).all(|((m, _), n)| m == n)) {
        desk.layout.clone_from(&old.layout);
    } else {
        desk.layout = mons.iter().copied().zip(grids(desk, mons)).collect();
    }
}

/// Pure: the icons on display `mi` as (index into desk.icons, cell).
#[cfg(test)]
pub fn icon_cells(desk: &Desk, mons: &[Monitor], mi: usize, g: &Grid) -> Vec<(usize, Cell)> {
    if !desk.shown {
        return Vec::new();
    }
    desk.icons.iter().enumerate().filter(|(_, p)| icon_monitor(**p, (g.cw, g.ch), mons) == Some(mi)).filter_map(|(i, p)| g.icon_cell(*p).map(|c| (i, c))).collect()
}

/// Pure: the cells auto-arrange keeps for icons: every column they flow
/// through, whole (Explorer fills columns top to bottom from the left; on
/// a right-to-left desktop, `rtl`, from the right).
#[cfg(test)]
pub fn flow_columns(g: &Grid, icons: &[(usize, Cell)], rtl: bool) -> HashSet<Cell> {
    let cols: Vec<i32> = if rtl {
        match icons.iter().map(|(_, (c, _))| *c).min() {
            Some(first) => (first.max(0)..g.cols).collect(),
            None => Vec::new(),
        }
    } else {
        match icons.iter().map(|(_, (c, _))| *c).max() {
            Some(last) => (0..=last.min(g.cols - 1)).collect(),
            None => Vec::new(),
        }
    };
    cols.into_iter().flat_map(|c| (0..g.rows).map(move |r| (c, r))).collect()
}

/// Pure: a `w` x `h` block on grid `g` placed automatically: the top-right
/// most block (rightmost column first, then the top; the top left on a
/// right-to-left desktop, `rtl`, the icons' far side) covering no `hard`
/// cell (other widgets', auto-arrange's columns) and no icon; else the one
/// covering the fewest hard cells, then the fewest icons.
#[cfg(test)]
pub fn spot(g: &Grid, (w, h): (i32, i32), icons: &HashSet<Cell>, hard: &HashSet<Cell>, rtl: bool) -> Option<Span> {
    if w > g.cols || h > g.rows || w < 1 || h < 1 {
        return None;
    }
    let mut best: Option<((usize, usize), Span)> = None;
    let cols: Vec<i32> = if rtl { (0..=g.cols - w).collect() } else { (0..=g.cols - w).rev().collect() };
    for c in cols {
        for r in 0..=g.rows - h {
            let s = Span { c, r, w, h };
            let hits = s.cells().filter(|x| hard.contains(x)).count();
            let under = s.cells().filter(|x| icons.contains(x)).count();
            if hits == 0 && under == 0 {
                return Some(s);
            }
            let score = (hits, under);
            if best.is_none_or(|(b, _)| score < b) {
                best = Some((score, s));
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Pure: the free cell nearest `from` on grid `g` (not in `taken`), by
/// distance, then up, then left. None: the grid is full.
#[cfg(test)]
pub fn nearest_free(g: &Grid, from: Cell, taken: &HashSet<Cell>) -> Option<Cell> {
    g.all().filter(|c| !taken.contains(c)).min_by_key(|&(c, r)| {
        let (dc, dr) = (c - from.0, r - from.1);
        (dc * dc + dr * dr, r, c)
    })
}

/// Pure: where the icons under widget `w` go (auto-arrange off): each to
/// the nearest cell that is free (no icon, not `reserved` by a widget),
/// in reading order. Icons that find no cell stay.
#[cfg(test)]
pub fn pushes(g: &Grid, w: Span, icons: &[(usize, Cell)], reserved: &HashSet<Cell>) -> Vec<(usize, Cell)> {
    let mut taken: HashSet<Cell> = reserved.iter().copied().chain(w.cells()).collect();
    taken.extend(icons.iter().filter(|(_, c)| !w.contains(*c)).map(|(_, c)| *c));
    let mut under: Vec<(usize, Cell)> = icons.iter().copied().filter(|(_, c)| w.contains(*c)).collect();
    under.sort_by_key(|&(i, (c, r))| (r, c, i));
    let mut out = Vec::new();
    for (i, at) in under {
        if let Some(to) = nearest_free(g, at, &taken) {
            taken.insert(to);
            out.push((i, to));
        }
    }
    out
}

/// Pure: the display a widget at `r` belongs to (the one it covers most).
pub fn monitor_of(r: RECT, mons: &[Monitor]) -> Option<usize> {
    let area = |m: &Monitor| {
        let w = (r.right.min(m.rect.right) - r.left.max(m.rect.left)).max(0) as i64;
        let h = (r.bottom.min(m.rect.bottom) - r.top.max(m.rect.top)).max(0) as i64;
        w * h
    };
    let best = (0..mons.len()).max_by_key(|&i| (area(&mons[i]), mons[i].primary))?;
    if area(&mons[best]) > 0 {
        Some(best)
    } else {
        mons.iter().position(|m| m.primary).or((!mons.is_empty()).then_some(0))
    }
}

/// How a widget sits on the grid.
#[derive(Clone, Copy, Debug)]
#[cfg(test)]
pub struct Fit {
    /// Its smallest size, px on its display.
    pub min: (i32, i32),
    /// Its drawing's inset from its cells, DIP.
    pub inset: f32,
}

#[cfg(test)]
fn inset_px(m: &Monitor, f: &Fit) -> i32 {
    (f.inset * m.scale()).round() as i32
}

/// The cells a widget wanting a `size` px window takes (its insets added).
#[cfg(test)]
fn cells_for(g: &Grid, size: (i32, i32), fit: &Fit, inset: i32) -> (i32, i32) {
    g.span_for((size.0 + 2 * inset, size.1 + 2 * inset), fit.min, inset)
}

/// The cells taken on grid `g` by the widgets at `others`.
#[cfg(test)]
fn taken_by(g: &Grid, others: &[RECT]) -> HashSet<Cell> {
    others.iter().filter_map(|r| g.touched(*r)).flat_map(|s| s.cells().collect::<Vec<_>>()).collect()
}

/// Pure: where a widget `size` px big goes by itself on display `mi`
/// (other widgets at `others`): top-right free cells. None without a grid.
#[cfg(test)]
pub fn auto_place(desk: &Desk, mons: &[Monitor], mi: usize, size: (i32, i32), fit: &Fit, others: &[RECT]) -> Option<RECT> {
    let m = mons.get(mi)?;
    let g = grids(desk, mons).get(mi).copied().flatten()?;
    let inset = inset_px(m, fit);
    let icons = icon_cells(desk, mons, mi, &g);
    let mut hard = taken_by(&g, others);
    if !super::icons::MOVE_ICONS { hard.extend(icon_boxes(desk, &g)); }
    if desk.auto_arrange {
        hard.extend(flow_columns(&g, &icons, desk.rtl));
    }
    let under: HashSet<Cell> = icons.iter().map(|(_, c)| *c).collect();
    let s = spot(&g, cells_for(&g, size, fit, inset), &under, &hard, desk.rtl)?;
    Some(g.rect(s, inset))
}

/// Pure: widget `r` (dropped or resized by the user) snapped to its
/// display's cells. None without a grid there.
#[cfg(test)]
pub fn snap_rect(desk: &Desk, mons: &[Monitor], r: RECT, fit: &Fit) -> Option<RECT> {
    let mi = monitor_of(r, mons)?;
    let g = grids(desk, mons).get(mi).copied().flatten()?;
    let inset = inset_px(&mons[mi], fit);
    Some(g.rect(g.snap(r, fit.min, inset), inset))
}

/// Pure: every cell on grid `g` some icon's whole box (its spacing: the
/// glyph and its label) touches, whatever display the icon is on. With
/// "Align icons to grid" off an icon may sit anywhere, its box across up
/// to four cells; each of them is taken.
#[cfg(test)]
pub fn icon_boxes(desk: &Desk, g: &Grid) -> HashSet<Cell> {
    if !desk.shown {
        return HashSet::new();
    }
    let (cw, ch) = (g.cw, g.ch);
    desk.icons.iter().filter_map(|p| g.touched(RECT { left: p.x, top: p.y, right: p.x + cw, bottom: p.y + ch })).flat_map(|s| s.cells().collect::<Vec<_>>()).collect()
}

/// Pure: the icons to move out from under widget `r` (auto-arrange off;
/// every widget, `r`'s too, at `widgets`): (index into desk.icons, its new
/// box's top left on screen). A destination is a cell no widget and no
/// icon's box touches at all (icon_boxes), so an icon off the grid is
/// never overlapped. Nothing with auto-arrange on or the icons hidden.
pub fn plan_pushes(desk: &Desk, mons: &[Monitor], r: RECT, widgets: &[RECT]) -> Vec<(usize, POINT)> {
    use super::widget::intersects;
    if desk.auto_arrange || !desk.shown {
        return Vec::new();
    }
    let Some(mi) = monitor_of(r, mons) else { return Vec::new() };
    let Some(g) = grids(desk, mons).get(mi).copied().flatten() else { return Vec::new() };
    let boxes = icon_rects(desk);
    let mut under: Vec<_> = boxes.iter().enumerate().filter(|(_, rect)| intersects(r, **rect)).collect();
    under.sort_by_key(|&(i, rect)| (rect.top, rect.left, i));
    let mut reserved = Vec::new();
    let mut result = Vec::new();
    for (i, rect) in under {
        let to = g.all().map(|(c, r)| g.rect(Span { c, r, w: 1, h: 1 }, 0))
            .filter(|to| !intersects(*to, r) && !widgets.iter().chain(&boxes).chain(&reserved).any(|b| intersects(*to, *b)))
            .min_by_key(|to| {
                let dx = i64::from(to.left) - i64::from(rect.left);
                let dy = i64::from(to.top) - i64::from(rect.top);
                (dx * dx + dy * dy, to.top, to.left)
            });
        if let Some(to) = to {
            reserved.push(to);
            result.push((i, POINT { x: to.left, y: to.top }));
        }
    }
    result
}

pub fn icon_rects(desk: &Desk) -> Vec<RECT> {
    if !desk.shown { return Vec::new(); }
    desk.icons.iter().map(|p| RECT {
        left: p.x, top: p.y, right: p.x + desk.cell.0, bottom: p.y + desk.cell.1,
    }).collect()
}

/// Pure: size in 2-cell units, position at every icon cell, clear of every `hard` cell
/// (other widgets, auto-arrange's columns): the first place on the side
/// spot() starts from with no icon under it, else the one with the fewest.
/// None: no place is clear of the hard cells (review 7: moving a spot()
/// result to an even cell afterwards covered auto-arranged icons).
#[cfg(test)]
pub fn slot_spot(g: &Grid, (w, h): (i32, i32), icons: &HashSet<Cell>, hard: &HashSet<Cell>, rtl: bool) -> Option<Span> {
    if w > g.cols || h > g.rows || w < 1 || h < 1 {
        return None;
    }
    let step = 1;
    let mut cols: Vec<i32> = (0..=g.cols - w).step_by(step).collect();
    if !rtl {
        cols.reverse();
    }
    let mut best: Option<(usize, Span)> = None;
    for c in cols {
        for r in (0..=g.rows - h).step_by(step) {
            let s = Span { c, r, w, h };
            if s.cells().any(|x| hard.contains(&x)) {
                continue;
            }
            let under = s.cells().filter(|x| icons.contains(x)).count();
            if under == 0 {
                return Some(s);
            }
            if best.is_none_or(|(b, _)| under < b) {
                best = Some((under, s));
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Cycle through places on the side opposite Explorer's icon flow.
pub fn auto_slot(desk: &Desk, mons: &[Monitor], mi: usize, (w, h): (i32, i32), others: &[RECT]) -> Option<RECT> {
    use super::widget::intersects;
    let m = mons.get(mi)?;
    let g = Grid::widget(m, desk.cell)?;
    if w > g.cols || h > g.rows || w < 1 || h < 1 { return None; }
    let icons = icon_rects(desk);
    let flow = if desk.auto_arrange {
        let on_display = icons.iter().filter(|r| intersects(**r, m.rect));
        if desk.rtl {
            on_display.map(|r| r.left).min().map(|left| RECT { left, ..m.work })
        } else {
            on_display.map(|r| r.right).max().map(|right| RECT { right, ..m.work })
        }
    } else {
        None
    };
    let mut best = None;
    for col in 0..=g.cols - w {
        let c = if desk.rtl { col } else { g.cols - w - col };
        for r in 0..=g.rows - h {
            let rect = g.rect(Span { c, r, w, h }, 0);
            if others.iter().any(|o| intersects(rect, *o)) || flow.is_some_and(|f| intersects(rect, f)) {
                continue;
            }
            let hits = icons.iter().filter(|o| intersects(rect, **o)).count();
            if hits == 0 { return Some(rect); }
            if super::icons::MOVE_ICONS && desk.movable && best.is_none_or(|(count, _)| hits < count) {
                best = Some((hits, rect));
            }
        }
    }
    best.map(|(_, rect)| rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(l: i32, t: i32, w: i32, h: i32, dpi: u32, primary: bool) -> Monitor {
        let rect = RECT { left: l, top: t, right: l + w, bottom: t + h };
        let work = RECT { bottom: t + h - 48, ..rect };
        Monitor { handle: l as isize + 1, rect, work, primary, dpi }
    }

    fn pt(x: i32, y: i32) -> POINT {
        POINT { x, y }
    }

    #[test]
    fn widget_edges_and_gaps_match_at_every_scale() {
        for (dpi, left, top, width, height) in [
            (96, 0, 0, 2560, 1440),
            (120, -1919, -111, 1919, 1081),
            (144, 2560, -1600, 2560, 1600),
            (192, -3840, 1440, 3839, 2161),
        ] {
            let mut m = mon(left, top, width, height, dpi, dpi == 96);
            m.work.top += 31;
            m.work.right -= 49;
            let g = Grid::widget(&m, (76, 96)).unwrap();
            let gap = (WIDGET_GAP * m.scale()) as i32;
            assert_eq!(g.cols, (m.work.right - m.work.left - gap) / 76);
            assert_eq!(g.rows, (m.work.bottom - m.work.top - gap) / 96);
            let all = g.rect(Span { c: 0, r: 0, w: g.cols, h: g.rows }, 0);
            assert_eq!(all, RECT {
                left: m.work.left + gap, top: m.work.top + gap,
                right: m.work.right - gap, bottom: m.work.bottom - gap,
            });
            for c in 0..g.cols - 1 {
                for r in 0..g.rows - 1 {
                    let a = g.rect(Span { c, r, w: 1, h: 1 }, 0);
                    let b = g.cell_pos((c + 1, r + 1));
                    assert_eq!((b.x - a.right, b.y - a.bottom), (gap, gap));
                    assert_eq!(g.touched(a), Some(Span { c, r, w: 1, h: 1 }));
                }
            }
        }
    }

    #[test]
    fn widget_auto_uses_the_right_edge_and_keeps_clear_of_icon_flow() {
        let m = mon(-801, 0, 801, 644, 120, false);
        let mut desk = Desk { cell: (76, 96), auto_arrange: true, shown: true, ..Default::default() };
        let first = auto_slot(&desk, &[m], 0, (2, 2), &[]).unwrap();
        assert_eq!(first.right, m.work.right - 20);
        assert_eq!(first.top, m.work.top + 20);
        let next = auto_slot(&desk, &[m], 0, (2, 2), &[first]).unwrap();
        assert_eq!(next.top - first.bottom, 20);
        desk.icons.push(pt(first.left - 30, m.work.bottom - 96));
        assert_eq!(auto_slot(&desk, &[m], 0, (2, 2), &[]), None);
    }

    /// Icons Explorer-style: `n` of them in columns from the top left of
    /// the work area, 2 px in, 76 x 96 cells.
    fn column_icons(m: &Monitor, n: i32, rows: i32) -> Vec<POINT> {
        (0..n).map(|i| pt(m.work.left + 2 + (i / rows) * 76, m.work.top + 2 + (i % rows) * 96)).collect()
    }

    const FIT: Fit = Fit { min: (180, 160), inset: 4.0 };

    #[test]
    fn a_display_grid_starts_where_its_icons_do() {
        let m = mon(0, 0, 1920, 1080, 96, true);
        let icons = column_icons(&m, 5, 10);
        assert_eq!(residue(&icons, m.work, (76, 96)), Some((2, 2)));
        let g = Grid::on(m.work, (76, 96), Some((2, 2))).unwrap();
        assert_eq!((g.ox, g.oy), (2, 2));
        assert_eq!((g.cols, g.rows), ((1920 - 2) / 76, (1032 - 2) / 96));
        // An icon a few px off its cell still belongs to it.
        assert_eq!(g.icon_cell(pt(2 + 76 * 3 + 5, 2 + 96 - 4)), Some((3, 1)));
        assert_eq!(g.cell_at(-5, 10), None);
        assert_eq!(g.cell_pos((2, 1)), pt(2 + 152, 2 + 96));
        // No icons: the work area's corner.
        assert_eq!(residue(&[], m.work, (76, 96)), None);
        assert_eq!(Grid::on(m.work, (0, 96), None), None, "no spacing, no grid");
        assert_eq!(Grid::on(RECT { left: 0, top: 0, right: 50, bottom: 50 }, (76, 96), None), None, "not one cell");
    }

    #[test]
    fn widgets_are_whole_cells_inset_and_snap_to_the_nearest() {
        let g = Grid { ox: 0, oy: 0, cw: 76, ch: 96, cols: 25, rows: 10, ..Default::default() };
        // 336 x 408 DIP at 100 %: 4 x 4 cells (304 x 384), the nearest.
        assert_eq!(g.span_for((336, 408), (180, 160), 4), (4, 4));
        // Never under its minimum, never over the grid.
        assert_eq!(g.span_for((10, 10), (180, 160), 4), (3, 2));
        assert_eq!(g.span_for((99_999, 99_999), (1, 1), 4), (25, 10));
        let s = Span { c: 2, r: 1, w: 4, h: 3 };
        assert_eq!(g.rect(s, 4), RECT { left: 156, top: 100, right: 452, bottom: 380 });
        // Dropped a little off: back on the nearest cells, same size.
        let dropped = RECT { left: 170, top: 90, right: 466, bottom: 370 };
        assert_eq!(g.snap(dropped, (180, 160), 4), s);
        // Dropped past the edge: kept inside.
        let off = RECT { left: 1880, top: 900, right: 2176, bottom: 1180 };
        let s2 = g.snap(off, (180, 160), 4);
        assert_eq!((s2.c + s2.w, s2.r + s2.h), (25, 10));
        assert_eq!(g.snap(g.rect(s2, 4), (180, 160), 4), s2, "a snapped widget stays");
    }

    #[test]
    fn the_first_spot_is_top_right_and_clear_of_icons() {
        let m = mon(0, 0, 1920, 1080, 96, true);
        let desk = Desk { cell: (76, 96), icons: column_icons(&m, 14, 10), auto_arrange: false, shown: true, ..Desk::default() };
        let r = auto_place(&desk, std::slice::from_ref(&m), 0, (304, 384), &FIT, &[]).unwrap();
        let g = grids(&desk, std::slice::from_ref(&m))[0].unwrap();
        assert_eq!(g.touched(r).unwrap(), Span { c: g.cols - 4, r: 0, w: 4, h: 4 }, "rightmost columns, at the top");
        // A widget there already takes those cells: the next one goes left of it.
        let r2 = auto_place(&desk, std::slice::from_ref(&m), 0, (304, 384), &FIT, &[r]).unwrap();
        assert_eq!(g.touched(r2).unwrap(), Span { c: g.cols - 4, r: 4, w: 4, h: 4 }, "below it first (same columns)");
        // The DIP inset at 150 %: 6 px.
        let hi = mon(0, 0, 2880, 1620, 144, true);
        let d2 = Desk { cell: (114, 144), icons: Vec::new(), auto_arrange: false, shown: true, ..Desk::default() };
        let r3 = auto_place(&d2, std::slice::from_ref(&hi), 0, (456, 576), &FIT, &[]).unwrap();
        let g3 = grids(&d2, std::slice::from_ref(&hi))[0].unwrap();
        assert_eq!(r3.right, g3.ox + g3.cols * 114 - 6);
        assert_eq!(r3.top, g3.oy + 6);
    }

    #[test]
    fn a_full_desktop_takes_the_spot_covering_the_fewest_icons() {
        let g = Grid { ox: 0, oy: 0, cw: 76, ch: 96, cols: 6, rows: 4, ..Default::default() };
        // Every cell has an icon but the 2 x 2 at the bottom left has one.
        let mut icons: HashSet<Cell> = g.all().collect();
        icons.remove(&(0, 2));
        icons.remove(&(1, 2));
        icons.remove(&(0, 3));
        assert_eq!(spot(&g, (2, 2), &icons, &HashSet::new(), false), Some(Span { c: 0, r: 2, w: 2, h: 2 }));
        // Another widget's cells weigh more than icons.
        let hard: HashSet<Cell> = [(0, 2)].into_iter().collect();
        let s = spot(&g, (2, 2), &icons, &hard, false).unwrap();
        assert!(!s.contains((0, 2)));
        assert_eq!(spot(&g, (7, 1), &icons, &hard, false), None, "wider than the grid");
    }

    #[test]
    fn auto_arrange_keeps_the_icons_columns_and_never_moves_icons() {
        let m = mon(0, 0, 1920, 1080, 96, true);
        let mons = [m];
        // 23 icons, 10 rows: columns 0..=2 are theirs.
        let desk = Desk { cell: (76, 96), icons: column_icons(&m, 23, 10), auto_arrange: true, shown: true, ..Desk::default() };
        let g = grids(&desk, &mons)[0].unwrap();
        let cells = icon_cells(&desk, &mons, 0, &g);
        let flow = flow_columns(&g, &cells, false);
        assert!(flow.contains(&(2, 9)) && !flow.contains(&(3, 0)), "the third column whole, the fourth free");
        // A tiny desktop: the widget would fit only over the flow; it keeps out.
        let small = Grid { cols: 6, ..g };
        let s = spot(&small, (3, 3), &HashSet::new(), &flow_columns(&small, &cells, false), false).unwrap();
        assert!(s.c >= 3, "{s:?}");
        let r = auto_place(&desk, &mons, 0, (304, 384), &FIT, &[]).unwrap();
        assert!(plan_pushes(&desk, &mons, r, &[r]).is_empty(), "auto-arrange: nothing moves");
    }

    #[test]
    fn review_07_places_keep_clear_of_auto_arranged_icons() {
        // 9 columns, auto-arrange filling columns 0..=6: a small widget
        // (2 x 2) fits at the last column pair, starting at odd column 7.
        let g = Grid { ox: 0, oy: 0, cw: 76, ch: 96, cols: 9, rows: 6, ..Default::default() };
        let hard: HashSet<Cell> = (0..=6).flat_map(|c| (0..6).map(move |r| (c, r))).collect();
        assert_eq!(slot_spot(&g, (2, 2), &HashSet::new(), &hard, false), Some(Span { c: 7, r: 0, w: 2, h: 2 }), "last pair is clear of icons");
        // Columns 0..=5 taken: the place at column 6 is free.
        let hard: HashSet<Cell> = (0..=5).flat_map(|c| (0..6).map(move |r| (c, r))).collect();
        let s = slot_spot(&g, (2, 2), &HashSet::new(), &hard, false).unwrap();
        assert_eq!((s.c, s.r), (7, 0));
        // Icons (not arranged) count too: the free place is preferred.
        let icons: HashSet<Cell> = [(6, 0), (6, 1)].into_iter().collect();
        let s = slot_spot(&g, (2, 2), &icons, &hard, false).unwrap();
        assert!(!s.cells().any(|x| icons.contains(&x) || hard.contains(&x)), "{s:?}");
    }

    #[test]
    fn icons_under_a_widget_go_to_the_nearest_free_cells_and_nothing_else_moves() {
        let g = Grid { ox: 0, oy: 0, cw: 76, ch: 96, cols: 8, rows: 5, ..Default::default() };
        let w = Span { c: 0, r: 0, w: 2, h: 2 };
        let icons = vec![(0, (0, 0)), (1, (1, 1)), (2, (2, 0)), (3, (5, 4))];
        let moved = pushes(&g, w, &icons, &HashSet::new());
        let ids: Vec<usize> = moved.iter().map(|(i, _)| *i).collect();
        assert_eq!(ids, vec![0, 1], "only the two under it");
        // (0,0) goes to the nearest free: (0,2) and (2,1)/(1,2) are as near as can be.
        for (_, to) in &moved {
            assert!(!w.contains(*to) && *to != (2, 0) && *to != (5, 4));
        }
        assert_ne!(moved[0].1, moved[1].1, "never both to one cell");
        assert_eq!(moved[0].1, (0, 2));
        assert_eq!(moved[1].1, (2, 1));
        // A cell reserved by another widget is not free.
        let other: HashSet<Cell> = [(0, 2), (1, 2), (2, 1)].into_iter().collect();
        let moved = pushes(&g, w, &icons, &other);
        assert!(moved.iter().all(|(_, to)| !other.contains(to)));
        // A full grid: icons stay.
        let all: HashSet<Cell> = g.all().collect();
        assert!(pushes(&g, w, &icons, &all).is_empty());
        assert_eq!(nearest_free(&g, (0, 0), &all), None);
    }

    #[test]
    fn pushes_land_on_the_screen_grid_of_the_widgets_display() {
        let a = mon(0, 0, 1920, 1080, 96, true);
        let b = mon(1920, 0, 2560, 1440, 144, false);
        let mons = [a, b];
        // Icons on both; one on the second display at its cell (0, 0).
        let mut icons = column_icons(&a, 3, 10);
        icons.push(pt(1920 + 2, 2));
        let desk = Desk { cell: (76, 96), icons, auto_arrange: false, shown: true, ..Desk::default() };
        let gs = grids(&desk, &mons);
        let gb = gs[1].unwrap();
        assert_eq!((gb.ox, gb.oy), (1922, 2));
        let w = gb.rect(Span { c: 0, r: 0, w: 3, h: 2 }, 6);
        let plan = plan_pushes(&desk, &mons, w, &[w]);
        assert_eq!(plan.len(), 1, "only the second display's icon");
        assert_eq!(plan[0].0, 3);
        let to = plan[0].1;
        assert!(to.x >= 1920 && gb.icon_cell(to).is_some_and(|c| !Span { c: 0, r: 0, w: 3, h: 2 }.contains(c)), "{to:?}");
        // Hidden icons: nothing to move.
        let hidden = Desk { shown: false, ..desk.clone() };
        assert!(plan_pushes(&hidden, &mons, w, &[w]).is_empty());
    }

    #[test]
    fn an_icon_off_the_grid_takes_every_cell_its_box_touches() {
        // "Align icons to grid" off: the review's icon at x 1480 (its centre
        // in column 19) also reaches into column 20.
        let m = mon(0, 0, 1920, 1080, 96, true);
        let mons = [m];
        let desk = Desk { cell: (76, 96), icons: vec![pt(2, 2), pt(2 + 24 * 76, 2), pt(2 + 23 * 76, 98), pt(1480, 2)], auto_arrange: false, shown: true, ..Desk::default() };
        let g = grids(&desk, &mons)[0].unwrap();
        assert_eq!((g.ox, g.oy), (2, 2));
        assert_eq!(g.icon_cell(pt(1480, 2)), Some((19, 0)));
        let boxes = icon_boxes(&desk, &g);
        assert!(boxes.contains(&(19, 0)) && boxes.contains(&(20, 0)) && !boxes.contains(&(21, 0)));
        // One a few px off in both directions: four cells.
        let off = Desk { icons: vec![pt(2 + 76 * 3 + 5, 2 + 96 - 4)], ..desk.clone() };
        assert_eq!(icon_boxes(&off, &g), [(3, 0), (4, 0), (3, 1), (4, 1)].into_iter().collect());
        // On the grid: one cell each.
        let on = Desk { icons: vec![pt(2, 2), pt(2 + 76, 2 + 96)], ..desk.clone() };
        assert_eq!(icon_boxes(&on, &g), [(0, 0), (1, 1)].into_iter().collect());
        // The widget over the top right: nothing goes where that box reaches.
        let w = RECT { left: 2 + 21 * 76 + 4, top: 6, right: 2 + 25 * 76 - 4, bottom: 2 + 4 * 96 - 4 };
        let plan = plan_pushes(&desk, &mons, w, &[w]);
        assert_eq!(plan.len(), 2);
        for (_, to) in &plan {
            assert!(to.x >= 1480 + 76 || to.y >= 2 + 96 || to.x + 76 <= 1480, "overlaps the icon at 1480: {to:?}");
        }
        assert!(plan.iter().all(|(_, to)| *to != pt(2 + 20 * 76, 2)));
        // Icons hidden: no box takes anything.
        assert!(icon_boxes(&Desk { shown: false, ..desk }, &g).is_empty());
    }

    #[test]
    fn a_snapped_window_is_never_under_its_minimum_after_the_inset() {
        // The review's case: 76 x 96 cells, a note's 140 x 90 minimum, 4 px in.
        let g = Grid { ox: 0, oy: 0, cw: 76, ch: 96, cols: 25, rows: 10, ..Default::default() };
        let r = g.rect(g.snap(RECT { left: 0, top: 0, right: 140, bottom: 90 }, (140, 90), 4), 4);
        assert!(r.right - r.left >= 140 && r.bottom - r.top >= 90, "{r:?}");
        // Every size, every inset up to 8 px.
        for inset in 0..=8 {
            for w in (140..400).step_by(7) {
                for h in (90..400).step_by(11) {
                    let s = g.snap(RECT { left: 30, top: 40, right: 30 + w, bottom: 40 + h }, (140, 90), inset);
                    let r = g.rect(s, inset);
                    assert!(r.right - r.left >= 140 && r.bottom - r.top >= 90, "{w}x{h} inset {inset}: {r:?}");
                }
            }
        }
        // A grid too small for the minimum: the whole grid, nothing more.
        let tiny = Grid { cols: 1, rows: 1, ..g };
        assert_eq!(tiny.span_for((300, 300), (140, 90), 4), (1, 1));
    }

    #[test]
    fn a_right_to_left_desktop_flows_from_the_right_and_widgets_keep_left() {
        let g = Grid { ox: 0, oy: 0, cw: 76, ch: 96, cols: 10, rows: 4, ..Default::default() };
        // Icons in the two rightmost columns.
        let icons: Vec<(usize, Cell)> = (0..6).map(|i| (i as usize, (9 - i / 4, i % 4))).collect();
        let flow = flow_columns(&g, &icons, true);
        assert!(flow.contains(&(8, 3)) && flow.contains(&(9, 0)) && !flow.contains(&(7, 0)));
        let under: HashSet<Cell> = icons.iter().map(|(_, c)| *c).collect();
        assert_eq!(spot(&g, (3, 2), &under, &flow, true), Some(Span { c: 0, r: 0, w: 3, h: 2 }), "the top left");
        // Left to right the same icons would flow from the left: all ten columns.
        assert_eq!(flow_columns(&g, &icons, false).len(), 40);
    }

    #[test]
    fn review_empty_display_uses_its_own_work_origin() {
        let a = mon(0, 0, 1920, 1080, 96, true);
        let b = mon(1920, 0, 1920, 1080, 96, false);
        let desk = Desk { cell: (76, 96), icons: column_icons(&a, 4, 10), auto_arrange: false, shown: true, ..Desk::default() };
        let gs = grids(&desk, &[a, b]);
        assert_eq!((gs[1].unwrap().ox, gs[1].unwrap().oy), (1920, 0));
        // A widget dropped across both belongs to the one it covers most.
        let r = RECT { left: 1800, top: 100, right: 2200, bottom: 400 };
        assert_eq!(monitor_of(r, &[a, b]), Some(1));
        let snapped = snap_rect(&desk, &[a, b], r, &FIT).unwrap();
        assert!(snapped.left >= 1920, "{snapped:?}");
        assert_eq!(monitor_of(RECT { left: -900, top: -900, right: -800, bottom: -800 }, &[a, b]), Some(0), "off screen: the primary");
    }

    #[test]
    fn review_aligned_origin_ignores_individual_icon_motion() {
        let m = mon(0, 40, 1920, 1080, 96, true);
        let mut desk = Desk { cell: (80, 100), align: true, icons: vec![pt(17, 61)], ..Default::default() };
        let before = grids(&desk, &[m]);
        desk.icons[0] = pt(39, 88);
        assert_eq!(grids(&desk, &[m]), before);
        assert_eq!((before[0].unwrap().ox, before[0].unwrap().oy), (m.work.left, m.work.top));
    }

    #[test]
    fn review_cached_origin_is_used_by_single_monitor_previews() {
        let mons = [mon(0, 0, 1920, 1080, 96, true), mon(1920, 0, 1920, 1080, 144, false)];
        let mut first = Desk { cell: (80, 100), icons: vec![pt(7, 11)], ..Default::default() };
        stabilize(&mut first, None, &mons);
        let before = grids(&first, &mons);
        let mut moved = Desk { icons: vec![pt(37, 49)], ..first.clone() };
        stabilize(&mut moved, Some(&first), &mons);
        assert_eq!(grids(&moved, &mons[..1]), before[..1]);
    }

    #[test]
    fn review_shell_spacing_is_one_physical_coordinate_space() {
        let mons = [mon(0, 0, 1920, 1080, 96, true), mon(1920, 0, 2560, 1440, 144, false)];
        let desk = Desk { cell: (80, 100), spacing_dpi: 96, align: true, ..Default::default() };
        let gs = grids(&desk, &mons);
        assert_eq!((gs[1].unwrap().cw, gs[1].unwrap().ch), (80, 100));
    }
}
