//! Shared card geometry. All coordinates at the placement boundary are screen pixels.
use super::grid::{self, Desk, Grid, Span};
use crate::monitors::Monitor;
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::RECT;

// Used to infer sizes of cards saved before the independent widget grid.
pub const INSET: f32 = 6.;
pub const RADIUS: f32 = 22.;
pub const PAD: f32 = 16.;

/// A widget's size in places ("자리", SLOT x SLOT icon cells): 1 to 4
/// across, 1 to 4 down (owner 0.1.95: Galaxy-like sizes by place).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Size {
    pub w: u8,
    pub h: u8,
}
impl Default for Size {
    fn default() -> Self {
        Self::SMALL
    }
}
/// The most places across or down.
pub const MAX_PLACES: u8 = 4;
impl Size {
    pub const SMALL: Self = Self { w: 1, h: 1 };
    pub const MEDIUM: Self = Self { w: 2, h: 1 };
    pub const LARGE: Self = Self { w: 2, h: 2 };
    /// The named sizes (menus, the gallery's pages).
    pub const ALL: [Self; 3] = [Self::SMALL, Self::MEDIUM, Self::LARGE];
    #[cfg(test)]
    pub const fn new(w: u8, h: u8) -> Self {
        Self { w, h }
    }
    /// Every size from 1 x 1 to 4 x 4.
    pub fn every() -> impl Iterator<Item = Self> {
        (1..=MAX_PLACES).flat_map(|h| (1..=MAX_PLACES).map(move |w| Self { w, h }))
    }
    /// Kept within 1..=MAX_PLACES both ways.
    pub fn clamped(self) -> Self {
        Self { w: self.w.clamp(1, MAX_PLACES), h: self.h.clamp(1, MAX_PLACES) }
    }
    pub fn cells(self) -> (i32, i32) {
        (self.w as i32 * SLOT, self.h as i32 * SLOT)
    }
    /// Only a fallback / preview.
    #[cfg(test)]
    pub fn dimensions(self) -> (f32, f32) {
        let (w, h) = self.cells();
        (w as f32 * 92. - 2. * INSET, h as f32 * 92. - 2. * INSET)
    }
    /// The battery rows a card this size lists at most.
    pub fn capacity(self) -> usize {
        4 * self.h as usize
    }
    pub fn label(self) -> String {
        match self {
            Self::SMALL => crate::i18n::t("battery.small").to_string(),
            Self::MEDIUM => crate::i18n::t("battery.medium").to_string(),
            Self::LARGE => crate::i18n::t("widgets.size.large").to_string(),
            s => format!("{}×{}", s.w, s.h),
        }
    }
    pub fn pixels(self, g: &Grid, scale: f32) -> (i32, i32) {
        let (w, h) = self.cells();
        let inset = if g.gap == 0 { (INSET * scale).round() as i32 } else { 0 };
        let rect = g.rect(Span { c: 0, r: 0, w, h }, inset);
        (rect.right - rect.left, rect.bottom - rect.top)
    }
    pub fn nearest(size: (i32, i32), g: &Grid, scale: f32) -> Self {
        Self::every()
            .min_by_key(|s| {
                let (w, h) = s.pixels(g, scale);
                (i64::from(w - size.0)).pow(2) + (i64::from(h - size.1)).pow(2)
            })
            .unwrap_or_default()
    }
}

/// Saved as {"w": 2, "h": 1}; read from that or from the old names
/// (Small 1 x 1, Medium 2 x 1, Large 2 x 2).
impl Serialize for Size {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Size", 2)?;
        st.serialize_field("w", &self.w)?;
        st.serialize_field("h", &self.h)?;
        st.end()
    }
}
impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Saved {
            Name(String),
            Places {
                #[serde(default = "one")]
                w: u8,
                #[serde(default = "one")]
                h: u8,
            },
        }
        fn one() -> u8 {
            1
        }
        Ok(match Saved::deserialize(d)? {
            Saved::Name(n) => match n.as_str() {
                "Medium" => Self::MEDIUM,
                "Large" => Self::LARGE,
                _ => Self::SMALL,
            },
            Saved::Places { w, h } => Self { w, h }.clamped(),
        })
    }
}

/// The smallest size each widget takes: a battery and a note 1 x 1, the
/// inbox 2 x 1 (its rows need the width for text and the time pill).
pub fn min_size(kind: super::edit::Kind) -> Size {
    match kind {
        super::edit::Kind::Inbox => Size::MEDIUM,
        _ => Size::SMALL,
    }
}

pub fn intersects(a: RECT, b: RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}
pub fn card_rect(window: RECT, pad: i32) -> RECT {
    RECT {
        left: window.left + pad,
        top: window.top + pad,
        right: window.right - pad,
        bottom: window.bottom - pad,
    }
}
/// A size unit: 2 x 2 icon cells. Position advances one icon cell,
/// independently of size, including odd columns and rows.
pub const SLOT: i32 = 2;

/// Nearest fitting free place, deterministic ties: up then left. Never
/// shrink a size.
pub fn nearest_free(
    g: &Grid,
    from: RECT,
    size: (i32, i32),
    inset: i32,
    others: &[RECT],
) -> Option<RECT> {
    let (w, h) = size;
    if w < 1 || h < 1 || w > g.cols || h > g.rows {
        return None;
    }
    (0..=g.cols - w)
        .flat_map(|c| (0..=g.rows - h).map(move |r| Span { c, r, w, h }))
        .map(|s| g.rect(s, inset))
        .filter(|r| !others.iter().any(|o| intersects(*r, *o)))
        .min_by_key(|r| {
            (
                (i64::from(r.left) + i64::from(r.right) - i64::from(from.left) - i64::from(from.right)).pow(2)
                    + (i64::from(r.top) + i64::from(r.bottom) - i64::from(from.top) - i64::from(from.bottom)).pow(2),
                r.top,
                r.left,
            )
        })
}
/// None means full / unavailable: the caller keeps its committed position AND size.
pub fn snap(
    desk: &Desk,
    mons: &[Monitor],
    requested: RECT,
    size: Option<Size>,
    others: &[RECT],
) -> Option<RECT> {
    let mi = grid::monitor_of(requested, mons)?;
    let g = Grid::widget(&mons[mi], desk.cell)?;
    let scale = mons[mi].scale();
    let size = size.unwrap_or_else(|| {
        Size::nearest(
            (
                requested.right - requested.left,
                requested.bottom - requested.top,
            ),
            &g,
            scale,
        )
    });
    nearest_free(&g, requested, size.cells(), 0, &obstacles(desk, others))
}

/// Actual icon boxes block placement; empty rows in their columns stay free.
pub fn obstacles(desk: &Desk, others: &[RECT]) -> Vec<RECT> {
    let mut occupied = others.to_vec();
    if !super::icons::MOVE_ICONS || desk.auto_arrange || !desk.movable {
        occupied.extend(grid::icon_rects(desk));
    }
    occupied
}

/// Plan the entire change before reserving or moving any window.
pub fn realign(desk: &Desk, mons: &[Monitor], cards: &[(RECT, Size)], other: &[RECT]) -> Option<Vec<RECT>> {
    let mut occupied = other.to_vec();
    let mut result = Vec::with_capacity(cards.len());
    for &(old, size) in cards {
        let r = snap(desk, mons, old, Some(size), &occupied)?;
        occupied.push(r);
        result.push(r);
    }
    Some(result)
}

/// Half a cell of travel since the last target change, measured at the
/// card centre. Small reversals around a boundary keep the chosen target.
#[derive(Default)]
pub struct Target {
    kept: Option<(Grid, RECT, (i64, i64))>,
}
fn centre(r: RECT) -> (i64, i64) { (i64::from(r.left) + i64::from(r.right), i64::from(r.top) + i64::from(r.bottom)) }
impl Target {
    pub fn choose(&mut self, g: Grid, requested: RECT, candidate: Option<RECT>, others: &[RECT]) -> Option<RECT> {
        let p = centre(requested);
        if let Some((old_grid, old, at)) = self.kept {
            if old_grid == g && (p.0 - at.0).abs() < i64::from(g.cw) && (p.1 - at.1).abs() < i64::from(g.ch) && !others.iter().any(|r| intersects(old, *r)) {
                return Some(old);
            }
        }
        if let Some(r) = candidate {
            if self.kept.is_none_or(|(old_grid, old, _)| old_grid != g || old != r) { self.kept = Some((g, r, p)); }
        } else { self.kept = None; }
        candidate
    }
}

#[cfg(test)]
fn drag_target(history: &mut Target, desk: &Desk, mons: &[Monitor], at: RECT, size: Size, others: &[RECT]) -> Option<RECT> {
    let mi = grid::monitor_of(at, mons)?;
    let g = Grid::widget(&mons[mi], desk.cell)?;
    drag_target_in(history, g, at, size, &obstacles(desk, others))
}

pub(super) fn drag_target_in(history: &mut Target, g: Grid, at: RECT, size: Size, occupied: &[RECT]) -> Option<RECT> {
    history.choose(g, at, nearest_free(&g, at, size.cells(), 0, occupied), occupied)
}

/// Resolve release using the last displayed target.
pub fn landing(target: Option<RECT>, origin: RECT) -> RECT {
    target.unwrap_or(origin)
}

#[cfg(test)]
mod release_review_tests {
    use super::*;
    #[test]
    fn review_release_is_the_last_displayed_hysteresis_target() {
        let r = RECT { left: 0, top: 0, right: 800, bottom: 600 };
        let mons = [Monitor { handle: 0, rect: r, work: r, primary: true, dpi: 96 }];
        let desk = Desk { cell: (80, 100), ..Default::default() };
        let origin = RECT { left: 0, top: 0, right: 160, bottom: 200 };
        let mut history = Target::default();
        for x in [0, 41, 39] {
            let at = RECT { left: x, right: x + 160, ..origin };
            let preview = drag_target(&mut history, &desk, &mons, at, Size::SMALL, &[]);
            assert_eq!(landing(preview, origin), preview.unwrap());
        }
    }
}

pub fn drop_log(desk: &Desk, mons: &[Monitor], at: RECT, size: Size, chosen: Option<RECT>) {
    let Some(mi) = grid::monitor_of(at, mons) else { return };
    let Some(g) = Grid::widget(&mons[mi], desk.cell) else { return };
    let wanted = nearest_free(&g, at, size.cells(), 0, &[]);
    let reason = if chosen.is_none() { "full" } else if wanted == chosen { "centre" } else { "nearest-free" };
    crate::log!("widget-drop: origin=({}, {}) cell={}x{} cols={} rows={} selected={:?} reason={}", g.ox, g.oy, g.cw, g.ch, g.cols, g.rows, chosen.and_then(|r| g.cell_at(r.left, r.top)), reason);
}
/// Pure: the free places of grid `g` (one place each, its card rect with
/// `inset`): where the dots show while a card is dragged.
pub fn free_slots(g: &Grid, inset: i32, others: &[RECT]) -> Vec<RECT> {
    let mut out = Vec::new();
    for r in 0..=g.rows - SLOT {
        for c in 0..=g.cols - SLOT {
            let rect = g.rect(Span { c, r, w: SLOT, h: SLOT }, inset);
            if !others.iter().any(|o| intersects(rect, *o)) {
                out.push(rect);
            }
        }
    }
    out
}
pub fn desk(_scale: f32) -> Desk {
    // Until Explorer replies, use one system grid for every monitor too.
    super::icons::cached().unwrap_or_else(|| super::spacing::fallback(1.))
}
pub fn auto(desk: &Desk, mons: &[Monitor], mi: usize, size: Size, others: &[RECT]) -> Option<RECT> {
    grid::auto_slot(desk, mons, mi, size.cells(), others)
}

/// Shared popup components, called outside a runtime's RefCell borrow.
pub fn menu(
    theme: &crate::theme::Theme,
    at: windows::Win32::Foundation::POINT,
    scale: f32,
    extra: Vec<crate::popup::Item>,
) -> Option<u32> {
    use crate::popup::{self, Item};
    let gfx = std::rc::Rc::new(crate::gfx::Gfx::new().ok()?);
    let mut items = vec![Item::new(0, crate::i18n::t("battery.size")).literal().submenu(
        Size::ALL
            .into_iter()
            .enumerate()
            .map(|(i, s)| Item::new(10 + i as u32, s.label()).literal())
            .collect(),
    )];
    items.push(Item::new(50,crate::i18n::t("widgets.edit")).literal());
    items.extend(extra);
    match popup::track(popup::Request {
        gfx: &gfx,
        theme,
        items,
        at,
        align: popup::Align::TopLeft,
        scale,
        titles: Default::default(),
        on_slide: None,
        drag_ids: 0..0,
        on_space: None,
        on_pick: None,
        hangs: false,
    }) {
        popup::Outcome::Picked(50) => {super::edit::enter();None},
        popup::Outcome::Picked(id) => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod icon_grid_tests {
    use super::*;
    use windows::Win32::Foundation::POINT;
    fn mon(x: i32, dpi: u32) -> Monitor {
        let r = RECT { left: x, top: -100, right: x + 1440, bottom: 900 };
        Monitor { handle: 0, rect: r, work: r, primary: x == 0, dpi }
    }
    #[test]
    fn every_cell_including_last_odd_column_and_exact_sizes() {
        for cols in [5, 6, 9, 10] { for ox in [-1600, 0, 1440] { for dpi in [1., 1.5] {
            let g = Grid { ox, oy: -100, cw: (80. * dpi) as i32, ch: (100. * dpi) as i32, cols, rows: 8, ..Default::default() };
            for size in Size::every().filter(|s| s.cells().0 <= cols) {
                let (w, h) = size.cells();
                let i = (INSET * dpi).round() as i32;
                assert_eq!(size.pixels(&g, dpi), (w * g.cw - 2 * i, h * g.ch - 2 * i));
                let last = g.rect(Span { c: cols - w, r: 0, w, h }, 0);
                assert_eq!(nearest_free(&g, last, (w, h), 0, &[]), Some(last));
                assert_eq!(last.right, g.ox + g.cols * g.cw);
            }
            assert_eq!(free_slots(&g, 0, &[]).len(), ((cols - 1) * 7) as usize);
        } } }
    }
    #[test]
    fn resize_keeps_odd_start_and_the_opposite_edge() {
        let g = Grid { ox: -900, oy: 0, cw: 80, ch: 100, cols: 9, rows: 9, ..Default::default() };
        let s = Span { c: 5, r: 3, w: 2, h: 2 };
        assert_eq!(resized(&g, s, Edge::Right, 3, Size::SMALL, 0, &[]).0, Span { w: 4, ..s });
        let left = resized(&g, s, Edge::Left, -1, Size::SMALL, 0, &[]).0;
        assert_eq!(left, Span { c: 3, w: 4, ..s });
        assert_eq!(g.rect(left, 0).right, g.rect(s, 0).right);
    }
    #[test]
    fn boundary_reversals_need_half_a_cell_since_last_change() {
        let g = Grid { ox: 0, oy: 0, cw: 80, ch: 100, cols: 9, rows: 6, ..Default::default() };
        let a = g.rect(Span { c: 0, r: 0, w: 2, h: 2 }, 0);
        let b = g.rect(Span { c: 1, r: 0, w: 2, h: 2 }, 0);
        let shifted = |x| RECT { left: x, right: x + 160, ..a };
        let mut history = Target::default();
        assert_eq!(history.choose(g, a, Some(a), &[]), Some(a));
        assert_eq!(history.choose(g, shifted(41), Some(b), &[]), Some(b));
        for x in [39, 41, 38, 42, 40] { assert_eq!(history.choose(g, shifted(x), Some(if x <= 40 {a} else {b}), &[]), Some(b)); }
        assert_eq!(history.choose(g, shifted(0), Some(a), &[]), Some(a));
        assert_eq!(history.choose(g, shifted(1), Some(b), &[a]), Some(b), "an occupied target is released immediately");
    }
    #[test]
    fn drop_uses_centre_and_actual_icons_not_whole_flow_column() {
        let m = mon(0, 96);
        let desk = Desk { cell: (80, 100), icons: vec![POINT { x: 1280, y: -100 }], shown: true, auto_arrange: true, ..Default::default() };
        let at = RECT { left: 1200, top: 200, right: 1440, bottom: 400 };
        let got = snap(&desk, &[m], at, Some(Size::SMALL), &[]).unwrap();
        let widget_grid = Grid::widget(&m, desk.cell).unwrap();
        assert_eq!(got, widget_grid.rect(Span { c: 15, r: 3, w: 2, h: 2 }, 0));
        let g = grid::grids(&desk, &[m])[0].unwrap();
        let wide = RECT { left: 0, top: -100, right: 400, bottom: 100 };
        assert_eq!(nearest_free(&g, wide, (2, 2), 0, &[]).unwrap().left, 80);
    }
    #[test]
    fn manual_icons_block_drop_and_a_full_desktop_rejects_it() {
        let m = mon(0, 96);
        let mut desk = Desk { cell: (80, 100), shown: true, movable: true, auto_arrange: false, ..Default::default() };
        let g = grid::grids(&desk, &[m])[0].unwrap();
        let at = g.rect(Span { c: 0, r: 0, w: 2, h: 2 }, 6);
        desk.icons.push(g.cell_pos((0, 0)));
        let before = desk.icons.clone();
        let got = snap(&desk, &[m], at, Some(Size::SMALL), &[]).unwrap();
        assert!(!intersects(got, g.rect(Span { c: 0, r: 0, w: 1, h: 1 }, 0)));
        assert_eq!(desk.icons, before);
        desk.icons = (0..g.rows).flat_map(|r| (0..g.cols).map(move |c| g.cell_pos((c, r)))).collect();
        assert!(snap(&desk, &[m], at, Some(Size::SMALL), &[]).is_none());
        assert!(auto(&desk, &[m], 0, Size::SMALL, &[]).is_none());
    }
    #[test]
    fn registry_to_actual_grid_realigns_batch_on_negative_and_positive_displays() {
        let mons = [mon(-1440, 96), mon(0, 144)];
        let registry = Desk { cell: (76, 96), spacing_dpi: 96, ..Default::default() };
        let actual = Desk { cell: (80, 100), spacing_dpi: 96, icons: vec![POINT { x: -1436, y: -94 }, POINT { x: 6, y: -91 }], ..Default::default() };
        let gs = grid::grids(&registry, &mons);
        let old: Vec<_> = gs.iter().map(|g| (g.unwrap().rect(Span { c: 5, r: 1, w: 2, h: 2 }, 0), Size::SMALL)).collect();
        let aligned = realign(&actual, &mons, &old, &[]).unwrap();
        for (r, m) in aligned.iter().zip(mons) {
            let g = Grid::widget(&m, actual.cell).unwrap();
            let span = g.touched(*r).unwrap();
            assert_eq!((span.w, span.h), Size::SMALL.cells());
            assert_eq!(*r, g.rect(span, 0));
        }
        assert_eq!(realign(&actual, &mons, &aligned.iter().map(|r| (*r, Size::SMALL)).collect::<Vec<_>>(), &[]), Some(aligned));
    }

    #[test]
    #[ignore = "fake desktop icon alignment PNGs, no desktop capture"]
    fn harmony_icon_grid_panels() {
        use crate::{gfx::promo_kit as kit, companion::render, theme::Theme};
        let stage = kit::stage();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/icon-grid");
        for (cw, ch) in [(76, 96), (80, 100)] { for dpi in [1., 1.5] { for light in [false, true] {
            let g = Grid { ox: (12. * dpi) as i32, oy: (12. * dpi) as i32, cw, ch, cols: 9, rows: 5, ..Default::default() };
            let (w, h) = (g.ox * 2 + g.cw * g.cols, g.oy * 2 + g.ch * g.rows);
            let cv = kit::Canvas::new(w as f32, h as f32);
            let mut theme = Theme::graphite(); theme.is_light = light;
            unsafe {
                cv.dc.Clear(Some(&super::super::card::rgba(super::super::card::surface(light))));
                cv.dc.SetTransform(&windows_numerics::Matrix3x2::identity());
                let ink = super::super::card::rgba(super::super::card::ink(&theme, 3));
                let mut faint = ink; faint.a = 0.2;
                let brush = cv.dc.CreateSolidColorBrush(&faint, None).unwrap();
                for c in 0..=g.cols { let x = (g.ox+c*g.cw) as f32; cv.dc.DrawLine(windows_numerics::Vector2 {X:x,Y:g.oy as f32}, windows_numerics::Vector2 {X:x,Y:(g.oy+g.ch*5) as f32}, &brush, 1., None); }
                for r in 0..=g.rows { let y = (g.oy+r*g.ch) as f32; cv.dc.DrawLine(windows_numerics::Vector2 {X:g.ox as f32,Y:y}, windows_numerics::Vector2 {X:(g.ox+g.cw*9) as f32,Y:y}, &brush, 1., None); }
                for (c, r) in [(0,0),(0,1),(0,2),(1,0),(1,3),(2,4),(5,1),(6,4)] {
                    let (x,y)=((g.ox+c*g.cw) as f32,(g.oy+r*g.ch) as f32);
                    crate::companion::chat::paint::rounded(&cv.dc, crate::companion::chat::paint::rect(x+cw as f32/2.-20.,y+16.,40.,32.),4.,ink).unwrap();
                    super::super::card::line(&cv.dc,&stage.gfx,"폴더",x+8.,y+54.,super::super::card::Line::new(12.,400,20.),super::super::card::ink(&theme,2),cw as f32-16.).unwrap();
                }
                for s in [Span {c:3,r:0,w:2,h:2},Span {c:3,r:3,w:2,h:2},Span {c:7,r:0,w:2,h:2},Span {c:7,r:3,w:2,h:2}] {
                    let r=g.rect(s,(INSET*dpi).round() as i32); cv.dc.SetTransform(&(render::translate(r.left as f32/dpi,r.top as f32/dpi)*render::scale(dpi,dpi)));
                    super::super::card::plate(&cv.dc,&theme,(r.right-r.left) as f32/dpi,(r.bottom-r.top) as f32/dpi,false,None).unwrap();
                    super::super::card::line(&cv.dc,&stage.gfx,"메모",16.,16.,super::super::card::Line::new(16.,600,24.),super::super::card::ink(&theme,1),110.).unwrap();
                }
            }
            let (px,w,h)=cv.finish(); kit::save_png(&px,w,h,&dir.join(format!("icons-{cw}-{ch}-{}-{light}.png",(dpi*100.) as u32)));
        } } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grid() -> Grid {
        Grid {
            ox: -1000,
            oy: 20,
            cw: 76,
            ch: 96,
            cols: 8,
            rows: 8,
            ..Default::default()
        }
    }
    #[test]
    fn three_sizes_use_exact_cells_at_each_dpi() {
        for scale in [1., 1.5, 2.] {
            for (size, cells) in Size::ALL.into_iter().zip([(2, 2), (4, 2), (4, 4)]) {
                assert_eq!(size.cells(), cells);
                assert_eq!(
                    size.pixels(&grid(), scale),
                    (
                        cells.0 * 76 - 2 * (INSET * scale).round() as i32,
                        cells.1 * 96 - 2 * (INSET * scale).round() as i32
                    )
                );
            }
        }
    }
    #[test]
    fn old_sizes_migrate_and_old_battery_names_deserialize() {
        assert_eq!(Size::nearest((330, 400), &grid(), 1.), Size::LARGE);
        assert_eq!(Size::nearest((260, 180), &grid(), 1.), Size::MEDIUM);
        assert_eq!(
            serde_json::from_str::<Size>("\"Small\"").unwrap(),
            Size::SMALL
        );
        assert_eq!(
            serde_json::from_str::<Size>("\"Medium\"").unwrap(),
            Size::MEDIUM
        );
    }
    #[test]
    fn occupied_card_not_shadow_and_nearest_free_block() {
        let g = grid();
        let a = g.rect(
            Span {
                c: 0,
                r: 0,
                w: 2,
                h: 2,
            },
            8,
        );
        let b = nearest_free(&g, a, (2, 2), 8, &[a]).unwrap();
        assert!(!intersects(a, b));
        assert_eq!(b.left, a.left + 152);
        assert_eq!(
            card_rect(
                RECT {
                    left: 0,
                    top: 0,
                    right: 200,
                    bottom: 220
                },
                32
            ),
            RECT {
                left: 32,
                top: 32,
                right: 168,
                bottom: 188
            }
        );
        assert_eq!(
            nearest_free(
                &g,
                a,
                (2, 2),
                8,
                &[g.rect(
                    Span {
                        c: 0,
                        r: 0,
                        w: 8,
                        h: 8
                    },
                    0
                )]
            ),
            None
        );
    }
    #[test]
    fn sequential_startup_repair_keeps_first_and_moves_later_cards() {
        let g = grid();
        let a = g.rect(
            Span {
                c: 0,
                r: 0,
                w: 2,
                h: 2,
            },
            8,
        );
        let mut occupied = vec![];
        for _ in 0..16 {
            let r = nearest_free(&g, a, (2, 2), 8, &occupied).unwrap();
            assert!(!occupied.iter().any(|o| intersects(r, *o)));
            occupied.push(r);
        }
        assert_eq!(occupied[0], a);
        assert!(nearest_free(&g, a, (2, 2), 8, &occupied).is_none());
    }
}


/// Preserve a stored placement when no free block exists.
pub fn placed_or_stored(placed:Option<RECT>,stored:Option<RECT>)->Option<RECT>{placed.or(stored)}
#[cfg(test)] mod slot_tests {
 use super::*;
 fn grid()->Grid{Grid {ox:-1000,oy:20,cw:76,ch:96,cols:9,rows:7, ..Default::default() }}
 /// Positions advance one cell; size still takes whole two-cell units.
 #[test] fn cards_snap_cell_by_cell(){
  let g=grid();
  let a=g.rect(Span{c:2,r:2,w:2,h:2},8);
  let shift=|r:RECT,dx:i32,dy:i32|RECT{left:r.left+dx,top:r.top+dy,right:r.right+dx,bottom:r.bottom+dy};
  assert_eq!(nearest_free(&g,shift(a,30,0),(2,2),8,&[]),Some(a),"less than half a cell: same position");
  assert_eq!(nearest_free(&g,shift(a,80,0),(2,2),8,&[]),Some(g.rect(Span{c:3,r:2,w:2,h:2},8)));
  for c in 0..g.cols { for r in 0..g.rows {
   let from=g.rect(Span{c,r,w:4,h:2},8);
   if let Some(to)=nearest_free(&g,from,(4,2),8,&[]) {
    let s=g.touched(to).unwrap();assert_eq!((s.c,s.r),(c.min(g.cols-4),r.min(g.rows-2)),"{c},{r} -> {s:?}");
   }
  }}
 }
 /// Old saved places on odd cells move to the nearest free place at
 /// start, never onto another card.
 #[test] fn odd_saved_places_move_to_the_nearest_free_place(){
  let g=grid();
  let first=g.rect(Span{c:0,r:0,w:2,h:2},8);
  let odd=g.rect(Span{c:1,r:1,w:2,h:2},8);
  let to=nearest_free(&g,odd,(2,2),8,&[first]).unwrap();
  let s=g.touched(to).unwrap();
  assert!(!intersects(to,first),"{s:?}");
  assert_eq!(nearest_free(&g,odd,(2,2),8,&[]),Some(odd));
  assert!((s.c-1).abs()<=1&&(s.r-1).abs()<=1,"nearest: {s:?}");
 }
 /// The dots: every free place, not the taken ones; odd columns at the
 /// edge make no half place.
 #[test] fn free_places_are_whole_places_not_taken(){
  let g=grid();
  assert_eq!(free_slots(&g,8,&[]).len(),8*6);
  let taken=g.rect(Span{c:0,r:0,w:4,h:2},8);
  let free=free_slots(&g,8,&[taken]);
  assert_eq!(free.len(),8*6-8);
  assert!(free.iter().all(|r|!intersects(*r,taken)));
 }
}
#[cfg(test)] mod review_tests {
 use super::*;
 #[test] fn review_08_full_grid_keeps_stored_battery_visible(){
  let old=RECT{left:20,top:30,right:160,bottom:210};
  assert_eq!(placed_or_stored(None,Some(old)),Some(old));assert_eq!(placed_or_stored(None,None),None);
  let next=RECT{left:200,..old};assert_eq!(placed_or_stored(Some(next),Some(old)),Some(next));
 }
}

/// One of a card's four sides (its handle in the middle of it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}
/// Pure: the places a side dragged `d` px moved (half a place rounds on).
pub fn steps(d: i32, place: i32) -> i32 {
    if place <= 0 {
        return 0;
    }
    let p = place as f32;
    (d as f32 / p).round() as i32
}

/// Pure: the card on `span` (cells of grid `g`, on whole places) with side
/// `edge` dragged `steps` places (screen direction: + right / down): it
/// grows or shrinks place by place, from `min` to MAX_PLACES each way,
/// inside the grid, and stops before a place another card (`others`,
/// card rects; `inset` round a card) takes: it never pushes one away.
/// Its size in places too.
pub fn resized(g: &Grid, span: Span, edge: Edge, steps: i32, min: Size, inset: i32, others: &[RECT]) -> (Span, Size) {
    let (pc, pr, pw, ph) = (span.c, span.r, (span.w / SLOT).max(1), (span.h / SLOT).max(1));
    let (cols, rows) = (g.cols, g.rows);
    let max = MAX_PLACES as i32;
    // How far the side may go (in places, + grows), within size and grid.
    let (grow, room_lo, room_hi) = match edge {
        Edge::Right => (steps, min.w as i32 - pw, (max - pw).min((cols - pc - pw * SLOT) / SLOT)),
        Edge::Left => (-steps, min.w as i32 - pw, (max - pw).min(pc / SLOT)),
        Edge::Bottom => (steps, min.h as i32 - ph, (max - ph).min((rows - pr - ph * SLOT) / SLOT)),
        Edge::Top => (-steps, min.h as i32 - ph, (max - ph).min(pr / SLOT)),
    };
    let want = grow.clamp(room_lo.min(0), room_hi.max(0));
    let at = |k: i32| -> (i32, i32, i32, i32) {
        match edge {
            Edge::Right => (pc, pr, pw + k, ph),
            Edge::Left => (pc - k * SLOT, pr, pw + k, ph),
            Edge::Bottom => (pc, pr, pw, ph + k),
            Edge::Top => (pc, pr - k * SLOT, pw, ph + k),
        }
    };
    let span_of = |(c, r, w, h): (i32, i32, i32, i32)| Span { c, r, w: w * SLOT, h: h * SLOT };
    let mut k = want.min(0);
    // Growing: one place at a time, up to the first that is taken.
    for next in 1..=want.max(0) {
        let rect = g.rect(span_of(at(next)), inset);
        if others.iter().any(|o| intersects(rect, *o)) {
            break;
        }
        k = next;
    }
    let p = at(k);
    (span_of(p), Size { w: p.2 as u8, h: p.3 as u8 })
}

#[cfg(test)]
mod place_tests {
    use super::*;
    fn grid() -> Grid {
        Grid { ox: 0, oy: 0, cw: 76, ch: 96, cols: 16, rows: 10, ..Default::default() }
    }
    /// Small 1 x 1, Medium 2 x 1, Large 2 x 2: the old names read as places;
    /// new sizes are saved as places and read back.
    #[test]
    fn old_sizes_read_as_places() {
        for (old, new) in [("\"Small\"", Size::SMALL), ("\"Medium\"", Size::MEDIUM), ("\"Large\"", Size::LARGE)] {
            assert_eq!(serde_json::from_str::<Size>(old).unwrap(), new);
        }
        assert_eq!((Size::SMALL.w, Size::SMALL.h), (1, 1));
        assert_eq!((Size::MEDIUM.w, Size::MEDIUM.h), (2, 1));
        assert_eq!((Size::LARGE.w, Size::LARGE.h), (2, 2));
        let s = Size::new(3, 4);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"w":3,"h":4}"#);
        assert_eq!(serde_json::from_str::<Size>(&json).unwrap(), s);
        // Out of range: kept to 1..4; a missing side is one place.
        assert_eq!(serde_json::from_str::<Size>(r#"{"w":9,"h":0}"#).unwrap(), Size::new(4, 1));
        assert_eq!(serde_json::from_str::<Size>(r#"{"w":2}"#).unwrap(), Size::new(2, 1));
        // An entry that holds one (the notes' index) reads an old one too.
        #[derive(serde::Deserialize)]
        struct E {
            #[serde(default)]
            size: Option<Size>,
        }
        assert_eq!(serde_json::from_str::<E>(r#"{"size":"Large"}"#).unwrap().size, Some(Size::LARGE));
        assert_eq!(serde_json::from_str::<E>("{}").unwrap().size, None);
        assert_eq!(Size::every().count(), 16);
        assert_eq!(Size::new(3, 2).cells(), (6, 4));
    }
    /// A side dragged moves place by place, within 1..4 and the kind's
    /// least, inside the grid; another card stops it, never pushed.
    #[test]
    fn a_side_resizes_place_by_place() {
        let g = grid();
        let one = Span { c: 2, r: 2, w: 2, h: 2 };
        let place = 2 * 76;
        // Half a place rounds on; less does nothing.
        assert_eq!(steps(place / 2, place), 1);
        assert_eq!(steps(place / 2 - 10, place), 0);
        assert_eq!(steps(-place * 3, place), -3);
        // Right two places: 3 x 1.
        assert_eq!(resized(&g, one, Edge::Right, 2, Size::SMALL, 8, &[]), (Span { c: 2, r: 2, w: 6, h: 2 }, Size::new(3, 1)));
        // Left: the right side stays; the grid's edge stops it (one place left).
        assert_eq!(resized(&g, one, Edge::Left, -3, Size::SMALL, 8, &[]), (Span { c: 0, r: 2, w: 4, h: 2 }, Size::new(2, 1)));
        // Top and bottom.
        assert_eq!(resized(&g, one, Edge::Top, -1, Size::SMALL, 8, &[]).1, Size::new(1, 2));
        assert_eq!(resized(&g, one, Edge::Bottom, 9, Size::SMALL, 8, &[]).1, Size::new(1, 4), "four at most");
        // Never under the least: an inbox stays 2 x 1.
        let inbox = Span { c: 0, r: 0, w: 4, h: 2 };
        assert_eq!(resized(&g, inbox, Edge::Right, -1, Size::MEDIUM, 8, &[]).1, Size::MEDIUM);
        assert_eq!(resized(&g, one, Edge::Right, -2, Size::SMALL, 8, &[]).1, Size::SMALL);
        // Shrinks back.
        let big = Span { c: 0, r: 0, w: 8, h: 8 };
        assert_eq!(resized(&g, big, Edge::Bottom, -2, Size::SMALL, 8, &[]), (Span { c: 0, r: 0, w: 8, h: 4 }, Size::new(4, 2)));
        assert_eq!(resized(&g, big, Edge::Left, 1, Size::SMALL, 8, &[]), (Span { c: 2, r: 0, w: 6, h: 8 }, Size::new(3, 4)));
        // Blocked: a card two places to the right stops it after one.
        let other = g.rect(Span { c: 6, r: 2, w: 2, h: 2 }, 8);
        assert_eq!(resized(&g, one, Edge::Right, 3, Size::SMALL, 8, &[other]).1, Size::new(2, 1));
        // A card right beside it: no growth at all, nothing moved.
        let beside = g.rect(Span { c: 4, r: 2, w: 2, h: 2 }, 8);
        assert_eq!(resized(&g, one, Edge::Right, 2, Size::SMALL, 8, &[beside]), (one, Size::SMALL));
        // A card in another row does not stop it.
        let below = g.rect(Span { c: 4, r: 4, w: 2, h: 2 }, 8);
        assert_eq!(resized(&g, one, Edge::Right, 1, Size::SMALL, 8, &[below]).1, Size::new(2, 1));
    }
}
