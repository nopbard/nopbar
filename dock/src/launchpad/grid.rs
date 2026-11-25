//! Where things go on the Launchpad (pure math, DIP): the grid of app
//! cells sized from the monitor's work area, paging, keyboard steps, hit
//! tests, and right-to-left mirroring.
//!
//! Only the cells of the page on screen (and its neighbours, for a swipe)
//! ever get visuals: the rest of the list is virtual (GameDevNotes:
//! "리스트 가상화 — 보이는 칸만 만든다").

/// App icon edge.
pub const ICON: f32 = 64.0;
/// Gap between an icon and its name.
pub const LABEL_GAP: f32 = 8.0;
/// Names: 13 DIP text (times the text size setting), at most two lines.
pub const LABEL_SIZE: f32 = crate::theme::ramp::BODY.size;
/// The names' box at least: two lines of Segoe UI or Malgun Gothic at the
/// largest text size (38.04 DIP as DirectWrite lays them out), so with
/// those the square below holds them at every size and its shape never
/// changes with the setting. A font with taller lines gets a taller box
/// (label_box, Grid::label_h), the same for every app.
pub const LABEL_H: f32 = 39.0;
/// Icon + gap + name, centered in its cell (with the smallest name box).
pub const CONTENT_H: f32 = ICON + LABEL_GAP + LABEL_H;
/// Room around the icon and its name inside an app's square (the
/// selection and hover shape, and what a click hits): the same on every
/// side, so the icon and its name sit evenly in it.
pub const ITEM_PAD: f32 = 10.0;
/// An app's square: icon, gap and two lines of name, padded evenly (with
/// the smallest name box; Grid::item_side).
pub const ITEM_SIDE: f32 = ITEM_PAD * 2.0 + CONTENT_H;
/// Above the grid: the search field.
pub const TOP_BAND: f32 = 112.0;
/// Below the grid: the page dots.
pub const BOTTOM_BAND: f32 = 96.0;
/// The smallest cell (more columns / rows only when they fit at this size;
/// both grow with a taller name box, so the square keeps its room).
pub const MIN_CELL_W: f32 = 150.0;
pub const MIN_CELL_H: f32 = 140.0;
/// The biggest cell (a huge screen spreads the grid out only so far).
pub const MAX_CELL_W: f32 = 240.0;
pub const MAX_CELL_H: f32 = 200.0;
pub const MAX_COLS: usize = 7;
pub const MAX_ROWS: usize = 5;

/// The search field (width at most; its height).
pub const FIELD_W: f32 = 300.0;
pub const FIELD_H: f32 = crate::theme::metrics::size::FIELD_L;
pub const FIELD_TOP: f32 = (TOP_BAND - FIELD_H) / 2.0;

/// Page dots: spacing, and their centre above the work area's bottom.
pub const DOT_GAP: f32 = 18.0;
pub const DOT: f32 = 8.0;
pub const DOTS_UP: f32 = 54.0;

/// The page dots keep this far from the work area's sides.
pub const DOTS_MARGIN: f32 = 28.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Rect {
    pub fn new(l: f32, t: f32, r: f32, b: f32) -> Self {
        Rect { l, t, r, b }
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
    pub fn w(&self) -> f32 {
        self.r - self.l
    }
    pub fn h(&self) -> f32 {
        self.b - self.t
    }
    pub fn cx(&self) -> f32 {
        (self.l + self.r) / 2.0
    }
    pub fn cy(&self) -> f32 {
        (self.t + self.b) / 2.0
    }
    pub fn offset(&self, dx: f32, dy: f32) -> Rect {
        Rect { l: self.l + dx, t: self.t + dy, r: self.r + dx, b: self.b + dy }
    }
    /// Do the two share any area (touching edges do not)? (The layout's
    /// checks, and what a band picks in edit mode: picks.rs.)
    pub fn overlaps(&self, o: &Rect) -> bool {
        self.l.max(o.l) < self.r.min(o.r) && self.t.max(o.t) < self.b.min(o.b)
    }
    /// Is `o` inside (a hair of rounding allowed)?
    #[cfg(test)]
    pub fn holds(&self, o: &Rect) -> bool {
        const EPS: f32 = 0.01;
        o.l >= self.l - EPS && o.t >= self.t - EPS && o.r <= self.r + EPS && o.b <= self.b + EPS
    }
}

/// The app grid, relative to the work area's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub left: f32,
    pub top: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// The names' box height (label_box): the same for every app. 0: the
    /// names are hidden (a work area too small for them: fit).
    pub label_h: f32,
    /// The icon's edge: ICON, less only when a square of ICON does not fit
    /// (fit); the padding and the gap shrink with it, evenly.
    pub icon_edge: f32,
}

/// Pure: the names' box for two lines `two_lines` DIP tall (measured with
/// DirectWrite in the chosen font at the text size): whole DIP, never
/// smaller than LABEL_H (Segoe UI's and Malgun Gothic's), LABEL_H if it
/// could not be measured.
pub fn label_box(two_lines: f32) -> f32 {
    if two_lines.is_finite() && two_lines > 0.0 {
        two_lines.ceil().max(LABEL_H)
    } else {
        LABEL_H
    }
}

/// A line of text about this many em tall (Segoe UI's 1.3301 as
/// DirectWrite lays it out): the guess when a font's lines cannot be had.
pub const LINE_EM: f32 = 1.34;

/// Pure: the room for the first `lines` of a text laid out in lines
/// `line_hs` DIP tall (DirectWrite's line metrics), at most `box_h`: what
/// its layout is cut to, so the lines it may have are all kept (a hair
/// over their sum: never lost to rounding) and none spills out of its box.
/// No line metrics (`line_hs` empty: they could not be read): `guess`, the
/// lines' height as the font's own metrics put it (lines_h, LINE_EM), in
/// the box; no guess either, the box. Never a room of next to nothing.
pub fn lines_room(line_hs: &[f32], lines: usize, box_h: f32, guess: f32) -> f32 {
    let lines_ok = !line_hs.is_empty() && line_hs.iter().all(|h| h.is_finite() && *h > 0.0);
    let want = if lines_ok {
        line_hs.iter().take(lines.max(1)).sum::<f32>()
    } else if guess.is_finite() && guess >= 1.0 {
        guess
    } else {
        box_h
    };
    (want + 0.5).min(box_h)
}

/// Below the grid at the least, on a short work area: the page dots' hit
/// band (DOTS_UP + 12 above the bottom), and a hair more. The grid's
/// region ends here.
pub const BOTTOM_ROOM: f32 = DOTS_UP + DOTS_HIT + 4.0;

/// The smallest icon shown with its name: a square that does not fit with
/// a whole icon is scaled down (icon, padding and gap alike) to this.
pub const MIN_ICON: f32 = 32.0;
/// The smallest square a work area is laid out for: a MIN_ICON icon, no
/// name, padded evenly.
pub const MIN_SQUARE: f32 = MIN_ICON * (ICON + ITEM_PAD * 2.0) / ICON;
/// The shortest layout: the top controls, a row of the smallest squares,
/// the dots. A work area shorter still (hardly a
/// real one) is laid out this tall: the bottom controls sink towards (at
/// the very least past) its edge instead of overlapping the rest.
pub const MIN_LAYOUT_H: f32 = TOP_BAND + MIN_SQUARE + BOTTOM_ROOM;
/// The page dots' hit band: this far above and below their centre.
pub const DOTS_HIT: f32 = 12.0;


/// What goes where on Launchpad, relative to the work area's top-left
/// corner: three regions laid out first, from the work area alone, that
/// never share a point. Everything shown or hit falls inside its own
/// region (the field in `top`, the apps in `grid`, the
/// page dots in `dots`), so nothing can be under anything else.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Regions {
    /// The work area's width, and the height laid out (the work area's,
    /// MIN_LAYOUT_H at the least).
    pub w: f32,
    pub h: f32,
    pub rtl: bool,
    /// The search field (Done in edit mode).
    pub top: Rect,
    /// The apps (the Library's tiles, the search results).
    pub grid: Rect,
    /// The page dots' hit band, across the bottom.
    pub dots: Rect,
}

/// Pure: the regions of a work area `w` x `h` DIP (`rtl`: the dots run
/// right to left).
pub fn regions(w: f32, h: f32, rtl: bool) -> Regions {
    let w = if w.is_finite() { w.max(0.0) } else { 0.0 };
    let h = if h.is_finite() { h.max(MIN_LAYOUT_H) } else { MIN_LAYOUT_H };
    let top = Rect::new(0.0, 0.0, w, TOP_BAND);
    let grid = Rect::new(0.0, TOP_BAND, w, h - BOTTOM_ROOM);
    let (l, r) = (DOTS_MARGIN, w - DOTS_MARGIN);
    let cy = h - DOTS_UP;
    let dots = Rect::new(l, cy - DOTS_HIT, r.max(l), cy + DOTS_HIT);
    Regions { w, h, rtl, top, grid, dots }
}

/// One of the regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Top,
    Grid,
    Dots,
}

impl Regions {
    /// The region at (x, y), if any (the margins between them: none).
    pub fn at(&self, x: f32, y: f32) -> Option<Area> {
        [(self.top, Area::Top), (self.grid, Area::Grid), (self.dots, Area::Dots)]
            .into_iter()
            .find_map(|(r, a)| r.contains(x, y).then_some(a))
    }

    /// The page dots' row for `pages` pages, page `page` shown. Centered on
    /// the work area as always, and closer together and smaller when it is
    /// too narrow (each dot as drawn inside its hit, every hit inside
    /// `dots`). Too many for DOT_MIN_PITCH apiece: a window of the dots
    /// around the page shown (DotsRow). `back`: the last page change went
    /// back (towards the first page).
    ///
    /// A window of three or more holds the page shown and both its
    /// neighbours, so a click on either goes on that way. A window of two
    /// cannot hold both: it holds the page shown and the next one the way
    /// the pages last went (`back`), turning at either end; a window of
    /// one holds only that next page (the page shown has no room). Either
    /// way clicking the dots one after the other goes from the first page
    /// to the last and back.
    pub fn dots_row(&self, pages: usize, page: usize, back: bool) -> DotsRow {
        let d = &self.dots;
        let pages = pages.max(1);
        let room = d.w().max(0.0);
        let fit = if room / pages as f32 >= DOT_MIN_PITCH { pages } else { (room / DOT_MIN_PITCH).floor() as usize };
        let shown = fit.min(pages);
        let page = page.min(pages - 1);
        let first = match shown {
            1 if pages > 1 => {
                if page + 1 == pages || (back && page > 0) {
                    page - 1
                } else {
                    page + 1
                }
            }
            2 => (if back { page.saturating_sub(1) } else { page }).min(pages - 2),
            _ => page.saturating_sub(shown / 2).min(pages - shown),
        };
        let pitch = if shown == 0 { 0.0 } else { DOT_GAP.min(room / shown as f32) };
        let half = shown as f32 * pitch / 2.0;
        let (lo, hi) = (d.l + half, d.r - half);
        let cx = if hi >= lo { (self.w / 2.0).clamp(lo, hi) } else { d.cx() };
        DotsRow { pages, page, first, shown, cx, pitch, dot: DOT * (pitch / DOT_GAP).min(1.0), t: d.t, b: d.b, rtl: self.rtl }
    }

    /// The dot at (x, y) of `pages` (page `page` shown, `back` as in
    /// dots_row), if any (none with one page).
    pub fn dot_at(&self, x: f32, y: f32, pages: usize, page: usize, back: bool) -> Option<usize> {
        if pages < 2 || !self.dots.contains(x, y) {
            return None;
        }
        self.dots_row(pages, page, back).at(x, y)
    }

    /// The search field.
    pub fn field(&self) -> Rect {
        let fw = FIELD_W.min(self.w - 48.0).max(120.0);
        let l = self.w / 2.0 - fw / 2.0;
        Rect::new(l, FIELD_TOP, l + fw, FIELD_TOP + FIELD_H)
    }
}

/// Pure: an app's square side with names `label_h` DIP tall (0: none) and
/// an icon `icon` DIP across: the padding and the gap in step with the
/// icon (ITEM_SIDE with the usual name box and a whole icon).
fn square(label_h: f32, icon: f32) -> f32 {
    let k = icon / ICON;
    let gap = if label_h > 0.0 { LABEL_GAP * k } else { 0.0 };
    ITEM_PAD * 2.0 * k + icon + gap + label_h
}

/// Pure: the names' box and the icon's edge for an app's square at most
/// `room` DIP across, names measured to a box of `label` (label_box). The
/// whole square when it fits; else the names give what they must, down to
/// one line (half the box); else the square scales down (icon, padding
/// and gap alike; whole DIP), the icon to MIN_ICON at the least; else the
/// names are hidden and the icon takes the square (ICON at most).
fn fit(room: f32, label: f32) -> (f32, f32) {
    let room = if room.is_finite() { room.max(0.0) } else { 0.0 };
    let bare = square(LABEL_H, ICON) - LABEL_H;
    if room >= bare + label {
        return (label, ICON);
    }
    let fits = (room - bare).floor();
    let one_line = (label / 2.0).ceil();
    if fits >= one_line {
        return (fits, ICON);
    }
    let k = (room - one_line) / bare;
    if k * ICON >= MIN_ICON {
        return (one_line, (k * ICON).floor());
    }
    (0.0, (room * ICON / square(0.0, ICON)).floor().clamp(0.0, ICON))
}

/// The grid for a work area of `w` x `h` DIP with names whose two lines are
/// `two_lines` DIP tall as measured (label_box makes their box): grid_in
/// the work area's regions.
pub fn grid(w: f32, h: f32, two_lines: f32) -> Grid {
    grid_in(&regions(w, h, false), two_lines)
}

/// The grid in the grid region of `r`: as many 150 x 140 cells as fit
/// beside a 10 % side margin and between the field and the dots, at most
/// 7 x 5 (1920 x 1080 at 100 % gets 7 x 5), centered. A taller name box
/// makes every app's square and the smallest cell that much bigger both
/// ways. The rows end above the dots' usual band (BOTTOM_BAND) when a
/// square fits there, else at the region's bottom. A region too small for
/// a square gets fewer rows and columns (one row, two columns), then names
/// cut to one line, then smaller squares, then no names (fit): never
/// anything outside the region.
pub fn grid_in(r: &Regions, two_lines: f32) -> Grid {
    let (w, area) = (r.w, r.grid);
    let label = label_box(two_lines);
    let bottom = if r.h - area.t - BOTTOM_BAND >= square(label, ICON) { r.h - BOTTOM_BAND } else { area.b };
    let avail_h = (bottom - area.t).max(0.0);
    let (mut label_h, mut icon_edge) = fit(avail_h, label);
    let side = (w * 0.1).max(24.0);
    let min_w = MIN_CELL_W + square(label_h, icon_edge) - ITEM_SIDE;
    let avail_w = (w - side * 2.0).max(min_w * 2.0).min(w);
    // Two columns at the least, inside the region: a square wider than
    // half of it is made smaller too.
    if square(label_h, icon_edge) > avail_w / 2.0 {
        (label_h, icon_edge) = fit((avail_w / 2.0).min(avail_h), label);
    }
    let item = square(label_h, icon_edge);
    let grow = item - ITEM_SIDE;
    let (min_w, min_h) = (MIN_CELL_W + grow, MIN_CELL_H + grow);
    let cols = ((avail_w / min_w).floor() as usize).clamp(2, MAX_COLS);
    let rows = ((avail_h / min_h).floor() as usize).clamp(1, MAX_ROWS);
    let cell_w = (avail_w / cols as f32).min(MAX_CELL_W.max(min_w));
    // (One row shorter than the smallest cell: the square, which fits.)
    let cell_h = (avail_h / rows as f32).min(MAX_CELL_H.max(min_h)).max(item);
    let tall = cell_h * rows as f32;
    let top = area.t + ((avail_h - tall) / 2.0).max(0.0);
    Grid { cols, rows, left: (w - cell_w * cols as f32) / 2.0, top, cell_w, cell_h, label_h, icon_edge }
}

impl Grid {
    /// The padding around an app's icon and name in its square (ITEM_PAD,
    /// in step with the icon).
    pub fn pad(&self) -> f32 {
        ITEM_PAD * self.icon_edge / ICON
    }

    /// Between the icon and its name (none without names).
    pub fn gap(&self) -> f32 {
        if self.label_h > 0.0 { LABEL_GAP * self.icon_edge / ICON } else { 0.0 }
    }

    /// Icon + gap + name, centered in a cell.
    pub fn content_h(&self) -> f32 {
        self.icon_edge + self.gap() + self.label_h
    }

    /// An app's square: icon, gap and two lines of name, padded evenly.
    pub fn item_side(&self) -> f32 {
        square(self.label_h, self.icon_edge)
    }

    pub fn per_page(&self) -> usize {
        (self.cols * self.rows).max(1)
    }

    /// Column on screen of slot `slot` (right to left mirrors it).
    fn col(&self, slot: usize, rtl: bool) -> usize {
        let c = slot % self.cols;
        if rtl { self.cols - 1 - c } else { c }
    }

    /// The whole cell of slot `slot` (0 = first of the page).
    pub fn cell(&self, slot: usize, rtl: bool) -> Rect {
        let row = (slot / self.cols).min(self.rows.saturating_sub(1));
        let l = self.left + self.col(slot, rtl) as f32 * self.cell_w;
        let t = self.top + row as f32 * self.cell_h;
        Rect::new(l, t, l + self.cell_w, t + self.cell_h)
    }

    /// The icon of slot `slot`.
    pub fn icon(&self, slot: usize, rtl: bool) -> Rect {
        let c = self.cell(slot, rtl);
        let (t, e) = (c.t + (c.h() - self.content_h()).max(0.0) / 2.0, self.icon_edge);
        Rect::new(c.cx() - e / 2.0, t, c.cx() + e / 2.0, t + e)
    }

    /// The name under the icon of slot `slot` (no height: names hidden).
    pub fn label(&self, slot: usize, rtl: bool) -> Rect {
        let i = self.icon(slot, rtl);
        let (w, t) = (self.label_w(), i.b + self.gap());
        Rect::new(i.cx() - w / 2.0, t, i.cx() + w / 2.0, t + self.label_h)
    }

    /// Width of a name.
    pub fn label_w(&self) -> f32 {
        // Names stay inside the app's square.
        let most = self.item_side() - 12.0;
        (self.cell_w - 16.0).min(most).max(self.icon_edge.min(most))
    }

    /// What a click on slot `slot` hits: the icon and its name, a little
    /// around them (the gaps between apps are "empty": a click there closes).
    pub fn item(&self, slot: usize, rtl: bool) -> Rect {
        let i = self.icon(slot, rtl);
        let side = self.item_side();
        let t = i.t - self.pad();
        Rect::new(i.cx() - side / 2.0, t, i.cx() + side / 2.0, t + side)
    }

    /// The slot whose item is at (x, y), if any.
    pub fn slot_at(&self, x: f32, y: f32, rtl: bool) -> Option<usize> {
        (0..self.per_page()).find(|&s| self.item(s, rtl).contains(x, y))
    }
}

/// Which page app `i` is on.
pub fn page_of(i: usize, per_page: usize) -> usize {
    i / per_page.max(1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

/// Arrow key on app `sel` of `n`: left/right step through the list (across
/// rows and pages; mirrored right to left), up/down move a row within the
/// page. Returns the new app, None at an edge (nothing to move to).
pub fn step(sel: usize, n: usize, g: &Grid, key: Arrow, rtl: bool) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let sel = sel.min(n - 1);
    let pp = g.per_page();
    let slot = sel % pp;
    let page_start = sel - slot;
    let forward = match key {
        Arrow::Right => !rtl,
        Arrow::Left => rtl,
        Arrow::Down | Arrow::Up => {
            let row = slot / g.cols;
            let to_row = if key == Arrow::Down { row + 1 } else { row.checked_sub(1)? };
            if to_row >= g.rows {
                return None;
            }
            // Down into a short last row: its last app.
            let to = (page_start + to_row * g.cols + slot % g.cols).min(n - 1);
            return (to >= page_start + to_row * g.cols).then_some(to);
        }
    };
    if forward {
        (sel + 1 < n).then_some(sel + 1)
    } else {
        sel.checked_sub(1)
    }
}

/// Page dots closer than this (a hit narrower, a dot smaller) are too
/// fiddly: past that, a window of them (Regions::dots_row).
pub const DOT_MIN_PITCH: f32 = 12.0;

/// The page dots as laid out (Regions::dots_row): pages `first` ..
/// `first + shown` of `pages` (all of them unless they do not fit; the
/// page shown among them unless only one fits), around `cx`, `pitch`
/// apart, `dot` DIP across, hits `t`..`b` tall. A dot at a window's edge
/// with more pages beyond it shows smaller (the current page's never does;
/// a window of one, the next page's alone, always does).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DotsRow {
    pub pages: usize,
    pub page: usize,
    pub first: usize,
    pub shown: usize,
    pub cx: f32,
    pub pitch: f32,
    pub dot: f32,
    pub t: f32,
    pub b: f32,
    pub rtl: bool,
}

/// A window's edge dot with more pages beyond it: this much of a dot.
pub const DOT_MORE: f32 = 0.6;

impl DotsRow {
    /// Is page `p`'s dot shown?
    pub fn shows(&self, p: usize) -> bool {
        p >= self.first && p < self.first + self.shown
    }

    /// The centre x of page `p`'s dot (None: not shown).
    pub fn x(&self, p: usize) -> Option<f32> {
        if !self.shows(p) {
            return None;
        }
        let j = p - self.first;
        let k = if self.rtl { self.shown - 1 - j } else { j };
        Some(self.cx + (k as f32 - (self.shown as f32 - 1.0) / 2.0) * self.pitch)
    }

    /// Page `p`'s dot's diameter as drawn (0: not shown).
    pub fn size(&self, p: usize) -> f32 {
        if !self.shows(p) {
            return 0.0;
        }
        let more = (p == self.first && p > 0) || (p + 1 == self.first + self.shown && p + 1 < self.pages);
        if more && p != self.page { self.dot * DOT_MORE } else { self.dot }
    }

    /// What a click on page `p`'s dot hits (None: not shown).
    pub fn hit(&self, p: usize) -> Option<Rect> {
        let x = self.x(p)?;
        Some(Rect::new(x - self.pitch / 2.0, self.t, x + self.pitch / 2.0, self.b))
    }

    /// The box page `p`'s dot is drawn in (None: not shown).
    pub fn shape(&self, p: usize) -> Option<Rect> {
        let (x, r, cy) = (self.x(p)?, self.size(p) / 2.0, (self.t + self.b) / 2.0);
        Some(Rect::new(x - r, cy - r, x + r, cy + r))
    }

    /// The row as drawn: every hit, side by side (empty with no dots).
    pub fn span(&self) -> Rect {
        let half = self.shown as f32 * self.pitch / 2.0;
        Rect::new(self.cx - half, self.t, self.cx + half, self.b)
    }

    /// The pages whose dots are shown.
    pub fn shown_pages(&self) -> std::ops::Range<usize> {
        self.first..self.first + self.shown
    }

    /// The dot at (x, y), if any.
    pub fn at(&self, x: f32, y: f32) -> Option<usize> {
        self.shown_pages().find(|&p| self.hit(p).is_some_and(|h| h.contains(x, y)))
    }
}

/// Where a finished swipe lands: a drag of `dx` DIP (positive = to the
/// right) on a page `width` wide, ending at `speed` DIP/s. Far enough or
/// fast enough turns the page; right to left, a drag to the right goes on.
pub fn swipe_target(page: usize, pages: usize, dx: f32, speed: f32, width: f32, rtl: bool) -> usize {
    let far = dx.abs() > width * 0.12 || (dx.abs() > 24.0 && speed.abs() > 600.0 && speed.signum() == dx.signum());
    if !far {
        return page;
    }
    let next = (dx < 0.0) != rtl;
    if next { (page + 1).min(pages.saturating_sub(1)) } else { page.saturating_sub(1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_box_follows_the_fonts_lines() {
        // Segoe UI / Malgun Gothic: two lines at Small, Default and Large
        // (DirectWrite: 1.3301 em a line) all get the smallest box.
        for scale in crate::fonts::TEXT_SCALES {
            assert_eq!(label_box(LABEL_SIZE * scale * 1.3301 * 2.0), LABEL_H, "{scale}");
        }
        // A font with tall lines (Javanese Text: 2.2705 em) gets them all.
        assert_eq!(label_box(64.94), 65.0);
        // Not measured: the smallest.
        for bad in [0.0, -3.0, f32::NAN, f32::INFINITY] {
            assert_eq!(label_box(bad), LABEL_H);
        }
        // The square holds the icon and two lines, padded evenly.
        assert_eq!(ITEM_SIDE, ITEM_PAD * 2.0 + ICON + LABEL_GAP + LABEL_H);
        assert!(ITEM_SIDE <= MIN_CELL_H, "the square fits the smallest cell");
    }

    #[test]
    fn a_layout_keeps_its_lines_and_its_box() {
        // Two lines of a tall font are kept whole (no early "…").
        assert!(lines_room(&[32.47, 32.47, 32.47], 2, 65.0, 0.0) >= 64.94);
        // A third line is not.
        assert!(lines_room(&[32.47, 32.47, 32.47], 2, 1000.0, 0.0) < 64.94 + 32.0);
        // Never past the box (the name's surface): a line that cannot fit
        // goes to "…" instead of being cut through.
        assert_eq!(lines_room(&[30.0, 30.0], 2, 39.0, 0.0), 39.0);
        // One line, or fewer lines than allowed.
        assert_eq!(lines_room(&[19.0, 19.0], 1, 100.0, 0.0), 19.5);
        assert_eq!(lines_room(&[19.0], 2, 100.0, 0.0), 19.5);
    }

    #[test]
    fn lines_not_measured_get_the_fonts_guess_or_the_box() {
        // GetLineMetrics failed (no lines): the guess for two lines of the
        // font, in the box; never the half DIP an empty sum gave.
        assert_eq!(lines_room(&[], 2, 65.0, 64.94), 65.0);
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(near(lines_room(&[], 2, 39.0, 34.6), 35.1));
        assert!(near(lines_room(&[], 2, f32::INFINITY, 34.6), 35.1), "no box: the guess");
        // No guess either: the box.
        for bad in [0.0, -1.0, 0.4, f32::NAN, f32::INFINITY] {
            assert_eq!(lines_room(&[], 2, 39.0, bad), 39.0, "{bad}");
        }
        // Metrics that make no sense count as none.
        assert!(near(lines_room(&[f32::NAN, 19.0], 2, 39.0, 34.6), 35.1));
        assert_eq!(lines_room(&[0.0], 1, 39.0, 17.0), 17.5);
        // Whatever comes, a usable room.
        for (hs, guess) in [(&[][..], 0.0), (&[][..], 17.0), (&[0.0][..], f32::NAN), (&[19.0][..], 0.0)] {
            let room = lines_room(hs, 2, 39.0, guess);
            assert!((17.0..=39.0).contains(&room), "{hs:?} {guess}: {room}");
        }
    }

    #[test]
    fn a_taller_name_box_grows_every_square_alike() {
        for (w, h) in [(1920.0, 1048.0), (1366.0, 666.0), (2560.0, 1408.0), (1024.0, 560.0), (1080.0, 1888.0)] {
            // The usual box: the grid as it always was.
            let g = grid(w, h, LABEL_H);
            assert_eq!((g.label_h, g.item_side(), g.content_h()), (LABEL_H, ITEM_SIDE, CONTENT_H));
            for label in [LABEL_H, 65.0, 90.0] {
                let g = grid(w, h, label);
                let grow = label - LABEL_H;
                assert_eq!(g.item_side(), ITEM_SIDE + grow, "{w}x{h} {label}");
                for slot in 0..g.per_page() {
                    let (c, i, it, l) = (g.cell(slot, false), g.icon(slot, false), g.item(slot, false), g.label(slot, false));
                    assert!((it.w() - it.h()).abs() < 0.01, "{w}x{h} {label}: square");
                    assert!((i.t - it.t - ITEM_PAD).abs() < 0.01, "{w}x{h} {label}: pad above the icon");
                    assert!((it.b - l.b - ITEM_PAD).abs() < 0.01, "{w}x{h} {label}: pad below the name");
                    assert!((l.h() - label).abs() < 0.01, "{w}x{h} {label}: the whole box");
                    assert!(l.l >= it.l && l.r <= it.r, "{w}x{h} {label}: the name stays inside");
                    // Neighbours never overlap: the square is inside its cell.
                    assert!(it.w() <= c.w() + 0.01 && it.h() <= c.h() + 0.01, "{w}x{h} {label}: {it:?} in {c:?}");
                    assert!(it.l >= c.l - 0.01 && it.r <= c.r + 0.01, "{w}x{h} {label}");
                }
                if label > LABEL_H {
                    let base = grid(w, h, LABEL_H);
                    assert!(g.cols <= base.cols && g.rows <= base.rows, "{w}x{h} {label}: fewer, bigger cells");
                }
            }
        }
    }

    /// Every square of `g` (a work area `w` x `h`) in the grid region: clear
    /// of the field, the banner and the dots' hit band, inside its cell and
    /// the work area.
    fn clear_of_the_bottom(g: &Grid, w: f32, h: f32, what: &str) {
        for rtl in [false, true] {
            let r = regions(w, h, rtl);
            for slot in 0..g.per_page() {
                let (c, it, l) = (g.cell(slot, rtl), g.item(slot, rtl), g.label(slot, rtl));
                assert!((it.w() - it.h()).abs() < 0.01, "{what}: square");
                assert!(c.holds(&it), "{what}: {it:?} in {c:?}");
                assert!(r.grid.holds(&it), "{what}: {it:?} in the grid's region {:?}", r.grid);
                assert!(it.t >= -0.01 && it.l >= -0.01 && it.r <= w + 0.01, "{what}: inside the work area: {it:?}");
                assert!(!it.overlaps(&r.dots) && it.b <= r.dots.t + 0.01, "{what}: over the dots: {it:?}");
                assert!(!it.overlaps(&r.field()), "{what}: under the field: {it:?}");
                assert!(it.holds(&l), "{what}: the name inside its square");
            }
        }
    }

    /// Names whole, cut to one line at the least, or (the square scaled
    /// down) one line over an icon of MIN_ICON at the least, or hidden.
    fn names_as_they_may_be(g: &Grid, two: f32, what: &str) {
        let (whole, one) = (label_box(two), (label_box(two) / 2.0).ceil());
        if g.icon_edge >= ICON {
            assert!(g.label_h == 0.0 || (g.label_h <= whole && g.label_h >= one), "{what}: {g:?}");
        } else if g.label_h > 0.0 {
            assert_eq!(g.label_h, one, "{what}: {g:?}");
            assert!(g.icon_edge >= MIN_ICON && g.icon_edge == g.icon_edge.floor(), "{what}: {g:?}");
        }
        assert!(g.icon_edge > 0.0 && g.icon_edge <= ICON, "{what}: {g:?}");
    }

    /// The page dots of `pages` (page `page` shown) in regions `r`, as
    /// drawn and as hit: every dot inside its own hit and clear of its
    /// neighbours, every hit inside the dots' region, a click anywhere on a
    /// dot finding that dot; the page shown among those shown (a window of
    /// one: a neighbour of it, smaller), a window only when they cannot all
    /// have DOT_MIN_PITCH. Either way the pages last went.
    fn dots_as_drawn(r: &Regions, pages: usize, page: usize, what: &str) {
        for back in [false, true] {
            dots_as_drawn_going(r, pages, page, back, what);
        }
    }

    fn dots_as_drawn_going(r: &Regions, pages: usize, page: usize, back: bool, what: &str) {
        let row = r.dots_row(pages, page, back);
        let what = format!("{what}: {pages} pages on {page} back={back}: {row:?}");
        assert!(row.shown <= pages, "{what}");
        assert!(row.shown == 0 || r.dots.holds(&row.span()), "{what}");
        if row.shown == 1 && pages > 1 {
            assert!(!row.shows(page) && row.first.abs_diff(page) == 1, "{what}: a neighbour of the page shown");
            assert!(row.size(row.first) < row.dot, "{what}: not the page shown: smaller");
        } else if row.shown > 0 {
            assert!(row.shows(page), "{what}: the page shown");
        }
        if row.shown > 0 {
            assert!(row.pitch >= DOT_MIN_PITCH - 0.01 && row.pitch <= DOT_GAP, "{what}");
            assert!(row.shown == pages || r.dots.w() / (pages as f32) < DOT_MIN_PITCH, "{what}: a window only when they cannot all fit");
        }
        let mut last: Option<Rect> = None;
        for p in 0..pages {
            match (row.hit(p), row.shape(p)) {
                (Some(h), Some(d)) => {
                    assert!(r.dots.holds(&h), "{what}: hit {p} {h:?}");
                    assert!(h.holds(&d) && r.dots.holds(&d), "{what}: dot {p} {d:?} in {h:?}");
                    assert!(d.w() > 0.0 && d.w() <= DOT + 0.01 && d.w() <= h.w(), "{what}: dot {p}");
                    // A click on the dot's edges or middle finds it.
                    for x in [d.l + 0.01, d.cx(), d.r - 0.01] {
                        assert_eq!(r.dot_at(x, d.cy(), pages, page, back), Some(p), "{what}: dot {p} at {x}");
                    }
                    if let Some(q) = last {
                        let (a, b) = if r.rtl { (d, q) } else { (q, d) };
                        assert!(a.r <= b.l + 0.01, "{what}: dots {p} and its neighbour overlap: {q:?} {d:?}");
                    }
                    last = Some(d);
                }
                (None, None) => assert!(!row.shows(p), "{what}: {p}"),
                other => panic!("{what}: {p}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_short_work_area_keeps_the_grid_off_the_dots() {
        // The first review's case: 640 x 300 DIP, Javanese Text at Large
        // (a 65 DIP name box).
        let g = grid(640.0, 300.0, 64.94);
        assert_eq!(g.rows, 1);
        clear_of_the_bottom(&g, 640.0, 300.0, "640x300 Javanese Large");
        assert_eq!(g.slot_at(440.0, 300.0 - DOTS_UP, false), None, "the dots' band is no app");
        // Names cut to one line there (a box of at least half the two).
        assert!(g.label_h >= 33.0 && g.label_h < 65.0, "{}", g.label_h);
        // The usual names on the same screen: the whole box, the band down
        // to BOTTOM_ROOM.
        let g = grid(640.0, 300.0, LABEL_H);
        clear_of_the_bottom(&g, 640.0, 300.0, "640x300");
        assert!(g.top + g.cell_h * g.rows as f32 <= 300.0 - BOTTOM_ROOM + 0.01);
        // Every short work area, every name box: fewer rows, never past the
        // dots (from about the shortest a row of squares can take).
        for w in [400.0, 640.0, 1024.0, 1920.0] {
            for h in (220..=900).step_by(7) {
                let h = h as f32;
                for two in [LABEL_H, 64.94, 89.0] {
                    let g = grid(w, h, two);
                    clear_of_the_bottom(&g, w, h, &format!("{w}x{h} {two}"));
                    names_as_they_may_be(&g, two, &format!("{w}x{h} {two}"));
                    // Where the usual band holds a square, it is as it was:
                    // the whole name box, the grid between field and dots.
                    if h - TOP_BAND - BOTTOM_BAND >= ITEM_SIDE + label_box(two) - LABEL_H {
                        assert_eq!(g.label_h, label_box(two), "{w}x{h} {two}");
                        assert!(g.top >= TOP_BAND - 0.01 && g.top + g.cell_h * g.rows as f32 <= h - BOTTOM_BAND + 0.01, "{w}x{h} {two}");
                    }
                }
            }
        }
    }

    #[test]
    fn an_apps_shape_is_a_square_with_even_padding() {
        let g = grid(1920.0, 1048.0, LABEL_H);
        let (i, it, l) = (g.icon(0, false), g.item(0, false), g.label(0, false));
        assert!((it.w() - it.h()).abs() < 0.01, "square");
        assert!((i.t - it.t - ITEM_PAD).abs() < 0.01, "pad above the icon");
        assert!((it.b - l.b - ITEM_PAD).abs() < 0.01, "pad below the name");
        assert!(l.l >= it.l && l.r <= it.r, "the name stays inside");
        assert!((i.cx() - it.cx()).abs() < 0.01, "centered");
    }

    #[test]
    fn grid_sizes() {
        // 1080p at 100 %, top bar 32 px (taskbar hidden): 7 x 5.
        let g = grid(1920.0, 1048.0, LABEL_H);
        assert_eq!((g.cols, g.rows), (7, 5));
        // The dock reserving the bottom too: still 7 x 5.
        assert_eq!((grid(1920.0, 968.0, LABEL_H).cols, grid(1920.0, 968.0, LABEL_H).rows), (7, 5));
        // 4K at 150 % (2560 x 1440 DIP): the same grid, cells capped.
        let g = grid(2560.0, 1408.0, LABEL_H);
        assert_eq!((g.cols, g.rows), (7, 5));
        assert!(g.cell_w <= MAX_CELL_W && g.cell_h <= MAX_CELL_H);
        // 1366 x 768 at 100 %, dock reserved.
        let g = grid(1366.0, 666.0, LABEL_H);
        assert_eq!((g.cols, g.rows), (7, 3));
        // 1280 x 800 at 125 % (1024 x 640 DIP) with the top bar and dock.
        let g = grid(1024.0, 560.0, LABEL_H);
        assert_eq!((g.cols, g.rows), (5, 2));
        // Portrait 1080 x 1920 at 100 %.
        let g = grid(1080.0, 1888.0, LABEL_H);
        assert_eq!((g.cols, g.rows), (5, 5));
        // Tiny: never less than 2 x 1 (smaller squares: maybe more columns).
        let g = grid(300.0, 200.0, LABEL_H);
        assert!(g.cols >= 2 && g.rows == 1, "{g:?}");
    }

    /// The grid as it was laid out before the regions (0.1.25), for the
    /// sizes where it fit: what the new one must give to the pixel.
    fn grid_0_1_25(w: f32, h: f32, two_lines: f32) -> Grid {
        let label = label_box(two_lines);
        let item = |label: f32| ITEM_SIDE + (label - LABEL_H);
        let room = |below: f32| h - TOP_BAND - below;
        let (bottom, label_h) = if room(BOTTOM_BAND) >= item(label) {
            (h - BOTTOM_BAND, label)
        } else if room(BOTTOM_ROOM) >= item(label) {
            (h - BOTTOM_ROOM, label)
        } else {
            let fits = (room(BOTTOM_ROOM) - (ITEM_SIDE - LABEL_H)).floor();
            (h - BOTTOM_ROOM, fits.clamp((label / 2.0).ceil(), label))
        };
        let grow = label_h - LABEL_H;
        let (min_w, min_h) = (MIN_CELL_W + grow, MIN_CELL_H + grow);
        let item = ITEM_SIDE + grow;
        let side = (w * 0.1).max(24.0);
        let avail_w = (w - side * 2.0).max(min_w * 2.0);
        let avail_h = (bottom - TOP_BAND).max(0.0);
        let cols = ((avail_w / min_w).floor() as usize).clamp(2, MAX_COLS);
        let rows = ((avail_h / min_h).floor() as usize).clamp(1, MAX_ROWS);
        let cell_w = (avail_w / cols as f32).min(MAX_CELL_W.max(min_w));
        let cell_h = (avail_h / rows as f32).min(MAX_CELL_H.max(min_h)).max(item);
        let tall = cell_h * rows as f32;
        let top = if tall <= avail_h { TOP_BAND + (avail_h - tall) / 2.0 } else { (bottom - tall).max(0.0) };
        Grid { cols, rows, left: (w - cell_w * cols as f32) / 2.0, top, cell_w, cell_h, label_h, icon_edge: ICON }
    }

    #[test]
    fn a_usual_screen_is_laid_out_as_before_to_the_pixel() {
        let mut same = 0;
        for w in [400.0, 640.0, 800.0, 1024.0, 1280.0, 1366.0, 1440.0, 1536.0, 1920.0, 2560.0, 3840.0] {
            for h in (300..=2200).step_by(13) {
                let h = h as f32;
                for two in [0.0, 34.6, LABEL_H, 54.9, 64.94, 89.0] {
                    let old = grid_0_1_25(w, h, two);
                    // Where it fit (not over the field, not past the side).
                    if old.top < TOP_BAND || old.left < 0.0 {
                        continue;
                    }
                    let g = grid(w, h, two);
                    let what = format!("{w}x{h} {two}");
                    // Bit for bit: every number, so every rect.
                    assert_eq!(g, old, "{what}");
                    for rtl in [false, true] {
                        for slot in 0..g.per_page() {
                            assert_eq!(g.icon(slot, rtl), old.icon(slot, rtl), "{what}");
                            assert_eq!(g.label(slot, rtl), old.label(slot, rtl), "{what}");
                            assert_eq!(g.item(slot, rtl), old.item(slot, rtl), "{what}");
                        }
                        assert_eq!(g.label_w(), (old.cell_w - 16.0).clamp(ICON, ITEM_SIDE + old.label_h - LABEL_H - 12.0), "{what}");
                        // The dots centered DOT_GAP apart.
                        let r = regions(w, h, rtl);
                        let room = w / 2.0 - DOTS_MARGIN;
                        for pages in (2..=12).filter(|&p| p as f32 * DOT_GAP / 2.0 <= room) {
                            let row = r.dots_row(pages, 0, false);
                            assert_eq!((row.cx, row.pitch, row.dot, row.shown), (w / 2.0, DOT_GAP, DOT, pages), "{what} {pages}");
                        }
                        let f = r.field();
                        let fw = FIELD_W.min(w - 48.0).max(120.0);
                        assert_eq!((f.l, f.t, f.r, f.b), (w / 2.0 - fw / 2.0, FIELD_TOP, w / 2.0 - fw / 2.0 + fw, FIELD_TOP + FIELD_H), "{what}");
                    }
                    same += 1;
                }
            }
        }
        // (Most of the sweep is a usual screen.)
        assert!(same > 1000, "{same}");
        // The common screens, the usual names, all of them among those.
        for (w, h) in [(1920.0, 1048.0), (1920.0, 968.0), (1366.0, 666.0), (2560.0, 1408.0), (1024.0, 560.0), (1080.0, 1888.0)] {
            assert_eq!(grid(w, h, LABEL_H), grid_0_1_25(w, h, LABEL_H), "{w}x{h}");
        }
    }

    #[test]
    fn regions_never_overlap_and_everything_stays_in_its_own() {
        let mut heights: Vec<f32> = (150..=1500).step_by(11).map(|h| h as f32).collect();
        heights.extend([0.0, 182.0, 200.0, 220.0, 223.0, 224.0, 225.0, 250.0, 300.0]);
        for w in [320.0, 360.0, 400.0, 480.0, 640.0, 800.0, 1024.0, 1280.0, 1366.0, 1920.0, 2560.0, 3840.0] {
            for &h in &heights {
                for rtl in [false, true] {
                    let r = regions(w, h, rtl);
                    let what = format!("{w}x{h} rtl={rtl}");
                    let all = [r.top, r.grid, r.dots];
                    // No two share a point.
                    for i in 0..all.len() {
                        for j in i + 1..all.len() {
                            assert!(!all[i].overlaps(&all[j]), "{what}: {:?} and {:?}", all[i], all[j]);
                        }
                    }
                    // All in the area laid out (the work area, unless it is
                    // shorter than the shortest layout).
                    assert_eq!(r.h, h.max(MIN_LAYOUT_H));
                    let area = Rect::new(0.0, 0.0, w, r.h);
                    for x in all {
                        assert!(area.holds(&x), "{what}: {x:?}");
                    }
                    // The top controls in the top.
                    assert!(r.top.holds(&r.field()), "{what}: the field");
                    // Every dot as drawn in its hit, every hit in theirs,
                    // each found where it is.
                    for pages in [2, 3, 5, 10, 25, 60] {
                        for page in [0, pages / 2, pages - 1] {
                            dots_as_drawn(&r, pages, page, &what);
                        }
                    }
                    // Every app, every name box, in the grid's.
                    for two in [0.0, 34.6, LABEL_H, 54.9, 64.94, 89.0, 120.0, f32::NAN] {
                        let g = grid_in(&r, two);
                        let what = format!("{what} {two}");
                        assert_eq!(g, grid(w, h, two), "{what}: the same grid either way");
                        names_as_they_may_be(&g, two, &what);
                        let mut items = Vec::new();
                        for slot in 0..g.per_page() {
                            let (c, i, it, l) = (g.cell(slot, rtl), g.icon(slot, rtl), g.item(slot, rtl), g.label(slot, rtl));
                            assert!(r.grid.holds(&c), "{what}: cell {c:?} in {:?}", r.grid);
                            assert!(c.holds(&it) && it.holds(&i) && it.holds(&l), "{what}: {c:?} {it:?} {i:?} {l:?}");
                            assert!((it.w() - it.h()).abs() < 0.01, "{what}: square");
                            // Even padding: above the icon, below the name,
                            // beside the icon.
                            let below = if g.label_h > 0.0 { it.b - l.b } else { it.b - i.b };
                            assert!((i.t - it.t - g.pad()).abs() < 0.01 && (below - g.pad()).abs() < 0.01, "{what}: {it:?}");
                            assert!(((i.l - it.l) - (it.r - i.r)).abs() < 0.01, "{what}: centered");
                            assert_eq!(r.at(it.cx(), it.cy()), Some(Area::Grid), "{what}");
                            assert_eq!(g.slot_at(it.cx(), it.cy(), rtl), Some(slot), "{what}");
                            items.push(it);
                        }
                        for a in 0..items.len() {
                            for b in a + 1..items.len() {
                                assert!(!items[a].overlaps(&items[b]), "{what}: apps {a} and {b}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn too_short_for_a_row_scales_the_squares_down_then_hides_the_names() {
        // The latest review's case: 640 x 220 DIP, Javanese Text at Large.
        // A row of whole squares rose over the field: (288, 35)-(352, 99)
        // under the field's (170, 36)-(470, 72).
        let r = regions(640.0, 220.0, false);
        let g = grid_in(&r, 64.94);
        let field = r.field();
        for slot in 0..g.per_page() {
            let it = g.item(slot, false);
            assert!(!it.overlaps(&field) && it.t >= TOP_BAND, "{it:?}");
        }
        assert_eq!(g.slot_at(320.0, 50.0, false), None, "the field's point is no app");
        assert_eq!(r.at(320.0, 50.0), Some(Area::Top));
        // 42 DIP for a row: no name fits beside an icon of MIN_ICON, so
        // the names go and the icon takes the square.
        assert_eq!((g.label_h, g.icon_edge), (0.0, 32.0));
        // 640 x 250: one line of name over a smaller icon.
        let g = grid(640.0, 250.0, LABEL_H);
        assert_eq!(g.label_h, 20.0);
        assert!(g.icon_edge >= MIN_ICON && g.icon_edge < ICON, "{g:?}");
        assert!(g.item_side() <= 250.0 - BOTTOM_ROOM - TOP_BAND + 0.01);
        // Each step only when the one before cannot fit, and the squares
        // never grow as the room shrinks (but when the names go).
        let mut last = f32::INFINITY;
        let mut named = true;
        for h in (224..=500).rev() {
            let g = grid(1024.0, h as f32, LABEL_H);
            if g.label_h == 0.0 && named {
                named = false;
                last = f32::INFINITY;
            }
            assert!(named || g.label_h == 0.0, "{h}: names come back on a shorter screen");
            assert!(g.item_side() <= last + 0.01, "{h}: {g:?}");
            last = g.item_side();
        }
        // Shorter than the shortest layout: laid out as that, the bottom
        // controls just past the edge, never over the rest.
        for h in [0.0, 100.0, 182.0, 223.0] {
            let r = regions(1024.0, h, false);
            assert_eq!(r.h, MIN_LAYOUT_H);
            assert_eq!(grid(1024.0, h, LABEL_H), grid(1024.0, MIN_LAYOUT_H, LABEL_H));
        }
    }

    #[test]
    fn the_dots_run_across_the_bottom() {
        // Nothing beside them: the band runs from margin to margin, the
        // dots centered on the work area.
        for rtl in [false, true] {
            let r = regions(640.0, 800.0, rtl);
            assert_eq!((r.dots.l, r.dots.r), (DOTS_MARGIN, 640.0 - DOTS_MARGIN));
            for pages in [2, 6, 20] {
                let row = r.dots_row(pages, 0, false);
                assert_eq!((row.cx, row.pitch, row.shown), (320.0, DOT_GAP, pages), "{pages}");
            }
        }
        // Too many for the room: closer together and smaller, still inside.
        let r = regions(121.0, 800.0, false);
        let row = r.dots_row(5, 0, false);
        assert!(row.pitch < DOT_GAP && row.pitch * 5.0 <= r.dots.w() + 0.01 && row.dot < DOT, "{row:?}");
        assert_eq!(row.shown, 5);
    }

    /// Clicks the page dots of `pages` in `r` from page `from` (`back`: the
    /// way the pages last went) towards page `to`, as a click finds them
    /// (in the middle of the dot drawn), the way kept as go_page keeps it:
    /// each time the next page's dot that way if shown, else the dot
    /// shown nearest `to`. The pages gone through (stopping when no dot
    /// leads anywhere or after 2 x `pages` clicks).
    fn click_dots(r: &Regions, pages: usize, from: usize, back: bool, to: usize) -> Vec<usize> {
        let (mut page, mut back, mut path) = (from, back, vec![from]);
        while page != to && path.len() <= 2 * pages {
            let row = r.dots_row(pages, page, back);
            let next = if to > page { page + 1 } else { page - 1 };
            let q = if row.shows(next) { Some(next) } else { row.shown_pages().filter(|&q| q != page).min_by_key(|&q| q.abs_diff(to)) };
            let Some(q) = q else { break };
            let d = row.shape(q).expect("shown");
            assert_eq!(r.dot_at(d.cx(), d.cy(), pages, page, back), Some(q), "{row:?}: dot {q}");
            back = q < page;
            page = q;
            path.push(q);
        }
        path
    }

    #[test]
    fn clicking_the_dots_one_after_another_goes_to_either_end() {
        // The fix-lp5 review's case: 4 pages, room for two dots (80 DIP:
        // 24 between the margins). The window was [0, 1] on page 1 too: no
        // dot for 2 or 3.
        for rtl in [false, true] {
            let r = regions(80.0, 800.0, rtl);
            assert_eq!(r.dots_row(4, 0, false).shown, 2);
            assert_eq!(click_dots(&r, 4, 0, false, 3), [0, 1, 2, 3]);
            assert_eq!(click_dots(&r, 4, 3, false, 0), [3, 2, 1, 0]);
            // On page 1 going on: 1 and 2 (2 at 1's right left to right).
            let row = r.dots_row(4, 1, false);
            assert_eq!(row.shown_pages(), 1..3);
            assert_eq!(row.x(2).unwrap() > row.x(1).unwrap(), !rtl);
            assert!(row.size(2) < row.dot, "more pages that way");
            assert_eq!(r.dots_row(4, 2, true).shown_pages(), 1..3, "going back: 1 and 2");
        }
        // Every room from one dot to all of them, both ways: from the first
        // page to the last a page at a time and back; from any page, either
        // way the pages last went, either end (turning once at the most).
        for pages in [2, 3, 4, 7] {
            let mut rooms = std::collections::BTreeSet::new();
            for w in 60..=400 {
                for rtl in [false, true] {
                    let r = regions(w as f32, 800.0, rtl);
                    let n = r.dots_row(pages, 0, false).shown;
                    if n == 0 {
                        continue;
                    }
                    rooms.insert(n);
                    let what = format!("{w} rtl={rtl} {pages} pages, {n} shown");
                    let on: Vec<usize> = (0..pages).collect();
                    let back: Vec<usize> = (0..pages).rev().collect();
                    for b in [false, true] {
                        assert_eq!(click_dots(&r, pages, 0, b, pages - 1), on, "{what}");
                        assert_eq!(click_dots(&r, pages, pages - 1, b, 0), back, "{what}");
                        for from in 0..pages {
                            for to in [0, pages - 1] {
                                let path = click_dots(&r, pages, from, b, to);
                                assert_eq!(path.last(), Some(&to), "{what}: from {from} back={b}: {path:?}");
                            }
                            // The hint: the page shown whole, a window's
                            // edge dot with more pages beyond it smaller,
                            // every other dot whole.
                            let row = r.dots_row(pages, from, b);
                            for p in row.shown_pages() {
                                let edge = (p == row.first && p > 0) || (p + 1 == row.first + row.shown && p + 1 < pages);
                                let small = row.size(p) < row.dot;
                                assert_eq!(small, edge && p != from, "{what}: on {from} back={b}: dot {p}");
                            }
                            // Later pages to the right (left right to left).
                            for p in row.shown_pages().skip(1) {
                                assert_eq!(row.x(p).unwrap() > row.x(p - 1).unwrap(), !rtl, "{what}");
                            }
                        }
                    }
                }
            }
            assert_eq!(rooms, (1..=pages).collect(), "every room: {pages} pages");
        }
    }

    #[test]
    fn narrow_dots_shrink_to_their_region_then_show_a_window() {
        // The review's case: 4 pages, the dots' region x 28..43 (71 DIP
        // wide): 3.75 DIP apart, the 8 DIP dots overlapped and the end ones
        // were drawn outside it (at (27, 746): a click that closed).
        for rtl in [false, true] {
            let r = regions(71.0, 800.0, rtl);
            assert_eq!((r.dots.l, r.dots.r), (28.0, 43.0));
            for page in 0..4 {
                dots_as_drawn(&r, 4, page, &format!("71x800 rtl={rtl}"));
                // Room for one dot: the next page the way the pages went
                // (turning at either end), so a click goes on.
                for back in [false, true] {
                    let row = r.dots_row(4, page, back);
                    assert_eq!(row.shown, 1);
                    let next = match page {
                        0 => 1,
                        3 => 2,
                        p if back => p - 1,
                        p => p + 1,
                    };
                    assert_eq!(row.first, next, "on {page} back={back}");
                }
            }
            let (x, y) = (27.0, 746.0);
            assert_eq!(r.dot_at(x, y, 4, 0, false), None);
            assert!(!r.dots_row(4, 0, false).span().contains(x, y), "nothing drawn there");
        }
        // 121 DIP: five fit, smaller and closer; six do not: a window.
        let r = regions(121.0, 800.0, false);
        let room = r.dots.w();
        assert!(room >= 5.0 * DOT_MIN_PITCH && room < 6.0 * DOT_MIN_PITCH, "{room}");
        let row = r.dots_row(5, 4, false);
        assert_eq!((row.first, row.shown), (0, 5));
        assert!(row.dot < DOT && (row.dot / row.pitch - DOT / DOT_GAP).abs() < 0.001, "in step: {row:?}");
        let row = r.dots_row(6, 0, false);
        assert_eq!((row.first, row.shown), (0, 5));
        assert!(row.size(4) < row.dot, "more pages that way: the last shows smaller");
        assert_eq!(row.size(0), row.dot);
        // A window around the page shown: 320 DIP, 40 pages.
        for rtl in [false, true] {
            let r = regions(320.0, 800.0, rtl);
            let n = (r.dots.w() / DOT_MIN_PITCH).floor() as usize;
            assert!(n < 40);
            let row = r.dots_row(40, 20, false);
            assert_eq!((row.first, row.shown), (20 - n / 2, n));
            assert!(row.size(row.first) < row.dot && row.size(row.first + n - 1) < row.dot, "more both ways");
            assert_eq!(row.size(20), row.dot);
            assert_eq!(r.dots_row(40, 0, false).first, 0);
            assert_eq!(r.dots_row(40, 39, false).first, 40 - n);
            assert_eq!(r.dots_row(40, 39, false).size(39), row.dot, "the last page: no more that way");
            // A click on the window's edge dot goes to that page.
            let e = row.shape(row.first).unwrap();
            assert_eq!(r.dot_at(e.cx(), e.cy(), 40, 20, false), Some(row.first));
            // Right to left the window's first page is at its right.
            let (a, b) = (row.x(row.first).unwrap(), row.x(row.first + 1).unwrap());
            assert_eq!(a > b, rtl);
        }
        // Every narrow width, every count, every page shown, both ways.
        for w in (60..=1100).step_by(13) {
            for rtl in [false, true] {
                let r = regions(w as f32, 700.0, rtl);
                for pages in [2, 3, 4, 7, 12, 30, 99] {
                    for page in [0, 1, pages / 2, pages - 2, pages - 1] {
                        dots_as_drawn(&r, pages, page, &format!("{w} rtl={rtl}"));
                    }
                }
            }
        }
    }

    #[test]
    fn grid_is_centered_and_inside() {
        for (w, h) in [(1920.0, 1048.0), (1366.0, 666.0), (2560.0, 1408.0), (1024.0, 560.0), (1080.0, 1888.0)] {
            let g = grid(w, h, LABEL_H);
            let right = g.left + g.cell_w * g.cols as f32;
            assert!((g.left - (w - right)).abs() < 0.01, "{w}x{h}: centered");
            assert!(g.top >= TOP_BAND - 0.01, "{w}x{h}: below the field");
            assert!(g.top + g.cell_h * g.rows as f32 <= h - BOTTOM_BAND + 0.01, "{w}x{h}: above the dots");
            assert!(g.cell_h >= CONTENT_H, "{w}x{h}: an icon and two lines fit");
        }
    }

    #[test]
    fn cells_and_mirroring() {
        let g = grid(1920.0, 1048.0, LABEL_H);
        let a = g.cell(0, false);
        let b = g.cell(g.cols - 1, true);
        assert_eq!(a, b, "right to left, the row's last app takes the left end");
        assert!(g.cell(0, true).l > g.cell(1, true).l, "and the first the right end");
        assert_eq!(g.cell(g.cols, false).t, a.t + g.cell_h, "the next row");
        let i = g.icon(1, false);
        assert_eq!((i.w(), i.h()), (ICON, ICON));
        assert_eq!(g.slot_at(i.cx(), i.cy(), false), Some(1));
        assert_eq!(g.slot_at(i.cx(), i.cy(), true), Some(g.cols - 2));
        let l = g.label(0, false);
        assert!(l.b > i.b && l.w() <= g.cell_w);
        // Outside every item (the work area's corner): nothing.
        assert_eq!(g.slot_at(0.0, 0.0, false), None);
    }

    #[test]
    fn paging() {
        let g = grid(1920.0, 1048.0, LABEL_H);
        assert_eq!(g.per_page(), 35);
        assert_eq!(page_of(34, 35), 0);
        assert_eq!(page_of(35, 35), 1);
        assert_eq!(page_of(133, 35), 3);
    }

    #[test]
    fn arrow_steps() {
        let g = Grid { cols: 7, rows: 5, left: 0.0, top: 0.0, cell_w: 100.0, cell_h: 100.0, label_h: LABEL_H, icon_edge: ICON };
        // Right / left go through the list, across pages.
        assert_eq!(step(0, 100, &g, Arrow::Right, false), Some(1));
        assert_eq!(step(6, 100, &g, Arrow::Right, false), Some(7), "next row");
        assert_eq!(step(34, 100, &g, Arrow::Right, false), Some(35), "next page");
        assert_eq!(step(35, 100, &g, Arrow::Left, false), Some(34), "previous page");
        assert_eq!(step(0, 100, &g, Arrow::Left, false), None);
        assert_eq!(step(99, 100, &g, Arrow::Right, false), None);
        // Right to left: the arrows swap.
        assert_eq!(step(5, 100, &g, Arrow::Left, true), Some(6));
        assert_eq!(step(5, 100, &g, Arrow::Right, true), Some(4));
        // Up / down stay on the page.
        assert_eq!(step(3, 100, &g, Arrow::Down, false), Some(10));
        assert_eq!(step(10, 100, &g, Arrow::Up, false), Some(3));
        assert_eq!(step(3, 100, &g, Arrow::Up, false), None);
        assert_eq!(step(31, 100, &g, Arrow::Down, false), None, "bottom row");
        // Into a short last row: its last app.
        assert_eq!(step(5, 9, &g, Arrow::Down, false), Some(8));
        assert_eq!(step(0, 3, &g, Arrow::Down, false), None, "no second row");
        assert_eq!(step(0, 0, &g, Arrow::Right, false), None);
    }

    #[test]
    fn dots_centered() {
        let r = regions(1000.0, 800.0, false);
        assert_eq!(r.dots_row(1, 0, false).x(0), Some(500.0));
        assert_eq!(r.dots_row(2, 0, false).x(0), Some(500.0 - DOT_GAP / 2.0));
        assert_eq!(regions(1000.0, 800.0, true).dots_row(2, 0, false).x(0), Some(500.0 + DOT_GAP / 2.0));
        // On a usual screen the dots are centered on it, DOT_GAP apart.
        for rtl in [false, true] {
            let r = regions(1000.0, 800.0, rtl);
            let row = r.dots_row(4, 0, false);
            assert_eq!((row.cx, row.pitch, row.dot, row.first, row.shown), (500.0, DOT_GAP, DOT, 0, 4));
            let y = 800.0 - DOTS_UP;
            let x2 = row.x(2).unwrap();
            assert_eq!(r.dot_at(x2, y, 4, 0, false), Some(2));
            assert_eq!(r.dot_at(x2, y + DOTS_HIT + 1.0, 4, 0, false), None, "below the band");
            assert_eq!(r.dot_at(10.0, y, 4, 0, false), None);
            assert_eq!(r.dot_at(500.0, y, 1, 0, false), None, "one page: no dots");
            // All shown: every dot whole.
            assert!((0..4).all(|p| row.size(p) == DOT));
        }
    }

    #[test]
    fn swipes() {
        let w = 1920.0;
        assert_eq!(swipe_target(0, 4, -300.0, 0.0, w, false), 1, "dragged left: next page");
        assert_eq!(swipe_target(1, 4, 300.0, 0.0, w, false), 0);
        assert_eq!(swipe_target(1, 4, -50.0, 0.0, w, false), 1, "too short");
        assert_eq!(swipe_target(1, 4, -50.0, -900.0, w, false), 2, "a flick");
        assert_eq!(swipe_target(3, 4, -900.0, 0.0, w, false), 3, "no page after the last");
        assert_eq!(swipe_target(0, 4, 300.0, 0.0, w, false), 0, "none before the first");
        assert_eq!(swipe_target(0, 4, 300.0, 0.0, w, true), 1, "right to left: dragging right goes on");
    }
}
