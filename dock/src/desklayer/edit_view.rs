//! One painter and hit map for live editing and offscreen acceptance images.
use super::{
    card,
    edit::{Kind, Preview},
    widget::{self, Size},
};
use crate::{
    companion::chat::paint::{color, rect, rounded},
    gfx::Gfx,
    monitors::Monitor,
    theme::Theme,
};
use windows::{
    core::Result,
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct2D::{Common::*, *},
            DirectWrite::*,
        },
    },
};
use windows_numerics::{Matrix3x2, Vector2};
pub struct Scene<'a> {
    pub monitor: Monitor,
    pub frames: &'a [(isize, RECT)],
    pub ghost: Option<RECT>,
    /// The free places (widget::free_slots) while a card is dragged.
    pub slots: &'a [RECT],
    /// The card being dragged.
    pub lift: Option<Lifted<'a>>,
    pub gallery: bool,
    pub kind: Kind,
    pub size: Size,
    pub preview: &'a Preview,
    pub preview_hwnd: windows::Win32::Foundation::HWND,
    pub notes: &'a [(String, String)],
    pub selected: Option<&'a str>,
    /// The card being resized (long press, or picked in edit mode): its
    /// white frame, four handles and menu (screen px).
    pub chosen: Option<RECT>,
    /// Entered by a long press: only the chosen card's frame, no badges,
    /// no "+ widget" and "done".
    pub quiet: bool,
    /// The free places outlined while a card is chosen.
    pub places: &'a [RECT],
}
#[derive(Debug, PartialEq)]
pub enum Hit {
    None,
    Done,
    Gallery,
    Kind(Kind),
    Collection(i32),
    Page(i32),
    Add,
    Note(String),
    NewNote,
    Panel,
}
pub fn badge_center(r: RECT, m: Monitor) -> (f32, f32) {
    let dpi = m.scale();
    let (w, h) = dimensions(m);
    (
        ((r.left - m.work.left) as f32 / dpi).clamp(11., (w - 11.).max(11.)),
        ((r.top - m.work.top) as f32 / dpi).clamp(11., (h - 11.).max(11.)),
    )
}
fn dimensions(m: Monitor) -> (f32, f32) {
    (
        (m.work.right - m.work.left) as f32 / m.scale(),
        (m.work.bottom - m.work.top) as f32 / m.scale(),
    )
}
pub fn panel(m: Monitor) -> D2D_RECT_F {
    let (w, h) = dimensions(m);
    rect(
        (w - 404.).max(12.),
        24.,
        380_f32.min(w - 24.),
        (h - 104.).clamp(360., 720.),
    )
}
fn inside(r: D2D_RECT_F, p: (f32, f32)) -> bool {
    p.0 >= r.left && p.0 < r.right && p.1 >= r.top && p.1 < r.bottom
}
pub fn hit(
    m: Monitor,
    p: (f32, f32),
    gallery: bool,
    kind: Kind,
    notes: &[(String, String)],
) -> Hit {
    let (w, h) = dimensions(m);
    if inside(rect(w / 2. - 104., h - 64., 112., 36.), p) {
        return Hit::Gallery;
    }
    if inside(rect(w / 2. + 20., h - 64., 84., 36.), p) {
        return Hit::Done;
    }
    if gallery {
        let r = panel(m);
        let q = (p.0 - r.left, p.1 - r.top);
        let pw = r.right - r.left;
        let ph = r.bottom - r.top;
        if inside(r, p) {
            for (i, k) in Kind::ALL.into_iter().enumerate() {
                if inside(
                    rect(20. + i as f32 * (pw - 40.) / 3., 20., (pw - 48.) / 3., 32.),
                    q,
                ) {
                    return Hit::Kind(k);
                }
            }
            if inside(rect(20., ph - 104., 36., 32.), q) {
                return Hit::Page(-1);
            }
            if inside(rect(pw - 56., ph - 104., 36., 32.), q) {
                return Hit::Page(1);
            }
            if inside(rect(20., ph - 56., pw - 40., 36.), q) {
                return Hit::Add;
            }
            if kind == Kind::Note && !notes.is_empty() {
                if inside(rect(pw - 92., ph - 276., 32., 24.), q) {
                    return Hit::Collection(-1);
                }
                if inside(rect(pw - 56., ph - 276., 32., 24.), q) {
                    return Hit::Collection(1);
                }
                let base = ph - 244.;
                if inside(rect(20., base, pw - 40., 26.), q) {
                    return Hit::NewNote;
                }
                for (i, (id, _)) in notes.iter().take(3).enumerate() {
                    if inside(rect(20., base + 28. + i as f32 * 28., pw - 40., 26.), q) {
                        return Hit::Note(id.clone());
                    }
                }
            }
            return Hit::Panel;
        }
    }
    Hit::None
}
pub unsafe fn content(
    dc: &ID2D1DeviceContext,
    gfx: &Gfx,
    theme: &Theme,
    preview: &Preview,
    size: Size,
    dims: (f32, f32),
) -> Result<()> {
    match preview {
        Preview::Note(body, date) => card::note(dc, gfx, theme, body, date, dims.0, dims.1),
        Preview::Inbox(rows, visible, remaining) => {
            let n = crate::notes::widgets::rows_fit(dims.1);
            crate::notes::widgets::inbox_content(
                dc,
                gfx,
                theme,
                &crate::notes::widgets::Inbox {
                    rows,
                    visible: &visible[..visible.len().min(n)],
                    remaining: *remaining,
                    hover: None,
                    flash: None,
                },
                dims,
                0.,
            )
        }
        Preview::Battery(prefs, devices) => {
            let mut prefs = prefs.clone();
            prefs.size = size;
            crate::device_battery::paint::content_sized(
                dc,
                gfx,
                theme,
                &prefs,
                // All of them: the painter picks (lowest first) as the
                // card's own does.
                &devices.iter().collect::<Vec<_>>(),
                dims,
            )
        }
    }
}
unsafe fn label(
    dc: &ID2D1DeviceContext,
    gfx: &Gfx,
    theme: &Theme,
    s: &str,
    x: f32,
    y: f32,
    w: f32,
) -> Result<()> {
    card::text(
        dc,
        gfx,
        s,
        x,
        y,
        12.,
        DWRITE_FONT_WEIGHT_SEMI_BOLD,
        rgba(theme.ink()),
        w,
        22.,
    )
}
/// A dragged card shows a little larger.
pub const LIFT: f32 = 1.03;

/// A card as it lies, from what it shows: its glass over `under`, its
/// content (no hover buttons). The widgets' own painters draw the same
/// (note::frame, View::frame + its content); a dragged card, edit mode and
/// the comparisons draw this.
#[allow(clippy::too_many_arguments)]
pub unsafe fn face(dc: &ID2D1DeviceContext, gfx: &Gfx, theme: &Theme, preview: &Preview, size: Size, dims: (f32, f32), under: Option<&super::backdrop::Under>) -> Result<()> {
    card::body(dc, theme, dims.0, dims.1, under)?;
    let theme = card::theme_for(theme, under);
    content(dc, gfx, theme, preview, size, dims)
}

/// The card at (0, 0) lifted by `t` (1: held, 0: lying): 1 + 0.03 t
/// larger about its middle, its shadow from the lying one to the deep one.
/// The only difference from the card as it lies.
#[allow(clippy::too_many_arguments)]
pub unsafe fn raised(dc: &ID2D1DeviceContext, gfx: &Gfx, theme: &Theme, preview: &Preview, size: Size, dims: (f32, f32), under: Option<&super::backdrop::Under>, t: f32) -> Result<()> {
    use crate::companion::render::{scale, translate};
    let mut base = Matrix3x2::identity();
    dc.GetTransform(&mut base);
    let k = 1. + (LIFT - 1.) * t.clamp(0., 1.);
    let (w, h) = dims;
    dc.SetTransform(&(translate(-w / 2., -h / 2.) * scale(k, k) * translate(w / 2., h / 2.) * base));
    let glass = card::glass_for(theme, under);
    let light = glass.map_or(theme.is_light, |g| g.light);
    crate::gfx::soft_shadows(dc, rect(0., 0., w, h), card::RADIUS, &card::lift_shadow(light, glass.is_some(), t))?;
    face(dc, gfx, theme, preview, size, dims, under)?;
    dc.SetTransform(&base);
    Ok(())
}

/// A card being dragged (or settling into its place).
#[derive(Clone, Copy)]
pub struct Lifted<'a> {
    pub preview: &'a Preview,
    /// Where it is (screen px): its size there (`size` at that display).
    pub at: RECT,
    /// Its size in places, fixed when it was picked up (never read again
    /// from its px).
    pub size: Size,
    /// The scale it is drawn at: the display it is mostly on (one card,
    /// one scale, on every display it reaches).
    pub dpi: f32,
    /// How far it is lifted (1 held, down to 0 as it settles).
    pub t: f32,
}

/// Card `l` drawn on monitor `m`'s surface (its transform at `m`'s scale,
/// its origin `m`'s work area): its part there, at the card's own scale,
/// its glass `under` (`m`'s picture: backdrop::under_on). A card across
/// two displays is drawn on each, the parts meeting px for px.
pub unsafe fn dragged(dc: &ID2D1DeviceContext, gfx: &Gfx, theme: &Theme, m: Monitor, l: &Lifted<'_>, under: Option<&super::backdrop::Under>) -> Result<()> {
    use crate::companion::render::{scale, translate};
    let mut base = Matrix3x2::identity();
    dc.GetTransform(&mut base);
    let c = local(m, l.at);
    let dims = ((l.at.right - l.at.left) as f32 / l.dpi, (l.at.bottom - l.at.top) as f32 / l.dpi);
    let k = l.dpi / m.scale();
    dc.SetTransform(&(scale(k, k) * translate(c.left, c.top) * base));
    let drawn = raised(dc, gfx, theme, l.preview, l.size, dims, under, l.t);
    dc.SetTransform(&base);
    drawn
}

/// While a card is dragged (edit mode or not), on monitor `m`: where it
/// will land (`ghost`: white 16 %, a 1.5 DIP white 60 % rim, corners 22)
/// and a 6 DIP dot (white 35 %) in the middle of every free place. Nothing
/// otherwise.
pub unsafe fn marks(dc: &ID2D1DeviceContext, m: Monitor, ghost: Option<RECT>, slots: &[RECT]) -> Result<()> {
    let dpi = m.scale();
    let local = |r: RECT| rect((r.left - m.work.left) as f32 / dpi, (r.top - m.work.top) as f32 / dpi, (r.right - r.left) as f32 / dpi, (r.bottom - r.top) as f32 / dpi);
    let dot = dc.CreateSolidColorBrush(&color(1., 0.35), None)?;
    for r in slots {
        if ghost.is_some_and(|g| widget::intersects(g, *r)) {
            continue;
        }
        let r = local(*r);
        let (cx, cy) = ((r.left + r.right) / 2., (r.top + r.bottom) / 2.);
        dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: 3., radiusY: 3. }, &dot);
    }
    if let Some(g) = ghost {
        let r = local(g);
        rounded(dc, r, card::RADIUS, color(1., 0.16))?;
        let b = dc.CreateSolidColorBrush(&color(1., 0.60), None)?;
        dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 0.75, r.top + 0.75, r.right - r.left - 1.5, r.bottom - r.top - 1.5), radiusX: card::RADIUS - 0.75, radiusY: card::RADIUS - 0.75 }, &b, 1.5, None);
    }
    Ok(())
}


/// Pure: card `r` (screen px) in monitor `m`'s DIP.
pub fn local(m: Monitor, r: RECT) -> D2D_RECT_F {
    let dpi = m.scale();
    rect((r.left - m.work.left) as f32 / dpi, (r.top - m.work.top) as f32 / dpi, (r.right - r.left) as f32 / dpi, (r.bottom - r.top) as f32 / dpi)
}

/// A handle's radius (DIP): 14 across (mock5.html .h).
pub const HANDLE: f32 = 7.;
/// How near a press must be to a handle to take it (DIP).
const HANDLE_REACH: f32 = 14.;

/// Pure: the handles in the middle of card `c`'s four sides (DIP).
pub fn handles(c: D2D_RECT_F) -> [(widget::Edge, (f32, f32)); 4] {
    let (cx, cy) = ((c.left + c.right) / 2., (c.top + c.bottom) / 2.);
    [
        (widget::Edge::Top, (cx, c.top)),
        (widget::Edge::Right, (c.right, cy)),
        (widget::Edge::Bottom, (cx, c.bottom)),
        (widget::Edge::Left, (c.left, cy)),
    ]
}

/// Pure: the handle of card `c` under `p` (DIP).
pub fn handle_at(c: D2D_RECT_F, p: (f32, f32)) -> Option<widget::Edge> {
    handles(c).into_iter().find(|(_, (x, y))| (p.0 - x).powi(2) + (p.1 - y).powi(2) <= HANDLE_REACH * HANDLE_REACH).map(|(e, _)| e)
}

/// The chosen card's menu items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Size,
    Settings,
    Remove,
}
impl Item {
    pub const ALL: [Self; 3] = [Self::Size, Self::Settings, Self::Remove];
    pub fn label(self) -> &'static str {
        match self {
            Self::Size => crate::i18n::t("battery.size"),
            Self::Settings => crate::i18n::t("preset.Settings"),
            Self::Remove => crate::i18n::t("preset.Delete"),
        }
    }
}
const MENU_TEXT: card::Line = card::Line::new(12.5, 600, 18.);
const MENU_PAD: f32 = 6.;
const MENU_ITEM_H: f32 = 30.;
const MENU_GAP: f32 = 12.;

/// Pure but for the text's width: the chosen card `c`'s menu (its box and
/// items, DIP) under it, or over it when the display ends below.
pub fn menu(gfx: &Gfx, m: Monitor, c: D2D_RECT_F) -> (D2D_RECT_F, Vec<(Item, D2D_RECT_F)>) {
    let (w, h) = dimensions(m);
    let widths: Vec<f32> = Item::ALL.iter().map(|i| card::width(gfx, i.label(), MENU_TEXT).ceil() + 24.).collect();
    let bw = widths.iter().sum::<f32>() + 4. * (Item::ALL.len() - 1) as f32 + 2. * MENU_PAD;
    let bh = MENU_ITEM_H + 2. * MENU_PAD;
    let below = c.bottom + MENU_GAP;
    let top = if below + bh <= h - 8. { below } else { (c.top - MENU_GAP - bh).max(8.) };
    let left = c.left.clamp(8., (w - bw - 8.).max(8.));
    let mut x = left + MENU_PAD;
    let items = Item::ALL
        .iter()
        .zip(&widths)
        .map(|(i, iw)| {
            let r = rect(x, top + MENU_PAD, *iw, MENU_ITEM_H);
            x += iw + 4.;
            (*i, r)
        })
        .collect();
    (rect(left, top, bw, bh), items)
}

/// The free places outlined while a card is chosen (mock5.html .slot):
/// a 1.5 DIP white 35 % rim inside each, corners 22.
pub unsafe fn outlines(dc: &ID2D1DeviceContext, m: Monitor, places: &[RECT], ghost: Option<RECT>) -> Result<()> {
    let b = dc.CreateSolidColorBrush(&color(1., 0.35), None)?;
    for p in places {
        if ghost.is_some_and(|g| widget::intersects(g, *p)) {
            continue;
        }
        let r = local(m, *p);
        dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 0.75, r.top + 0.75, r.right - r.left - 1.5, r.bottom - r.top - 1.5), radiusX: card::RADIUS - 0.75, radiusY: card::RADIUS - 0.75 }, &b, 1.5, None);
    }
    Ok(())
}

/// The chosen card `r` (screen px): a 2 DIP white frame round it (corners
/// 24), a white handle with a shadow in the middle of each side, and its
/// menu (size, settings, remove) under it (mock5.html, long press).
pub unsafe fn chosen(dc: &ID2D1DeviceContext, gfx: &Gfx, theme: &Theme, m: Monitor, r: RECT) -> Result<()> {
    let c = local(m, r);
    let white = dc.CreateSolidColorBrush(&color(1., 0.95), None)?;
    dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(c.left - 1., c.top - 1., c.right - c.left + 2., c.bottom - c.top + 2.), radiusX: 23., radiusY: 23. }, &white, 2., None);
    let shadow = [(0., 1., 4., crate::theme::Rgba { r: 0, g: 0, b: 0, a: 102 })];
    for (_, (x, y)) in handles(c) {
        let dot = rect(x - HANDLE, y - HANDLE, 2. * HANDLE, 2. * HANDLE);
        crate::gfx::soft_shadows(dc, dot, HANDLE, &shadow)?;
        rounded(dc, dot, HANDLE, color(1., 1.))?;
    }
    let (b, items) = menu(gfx, m, c);
    crate::gfx::soft_shadows(dc, b, 14., &[(0., 8., 24., crate::theme::Rgba { r: 0, g: 0, b: 0, a: 89 })])?;
    let (face, ink) = if theme.is_light {
        (D2D1_COLOR_F { r: 0.97, g: 0.97, b: 0.97, a: 0.94 }, crate::theme::Rgba { r: 0x11, g: 0x11, b: 0x11, a: 255 })
    } else {
        (D2D1_COLOR_F { r: 30. / 255., g: 30. / 255., b: 32. / 255., a: 0.9 }, crate::theme::Rgba { r: 255, g: 255, b: 255, a: 255 })
    };
    rounded(dc, b, 14., face)?;
    for (i, r) in items {
        let tw = card::width(gfx, i.label(), MENU_TEXT);
        card::line(dc, gfx, i.label(), r.left + (r.right - r.left - tw) / 2., r.top + (MENU_ITEM_H - MENU_TEXT.height) / 2., MENU_TEXT, ink, tw + 2.)?;
    }
    Ok(())
}

pub unsafe fn paint(
    dc: &ID2D1DeviceContext,
    gfx: &Gfx,
    theme: &Theme,
    s: &Scene<'_>,
) -> Result<()> {
    let mut base = Matrix3x2::identity();
    dc.GetTransform(&mut base);
    let m = s.monitor;
    let dpi = m.scale();
    let (w, h) = dimensions(m);
    // Minimal alpha makes blank desktop clicks dismiss the editor. Never an app overlay.
    let ink = if theme.is_light { 0. } else { 1. };
    let wash = dc.CreateSolidColorBrush(&color(ink, 1. / 255.), None)?;
    dc.FillRectangle(&rect(0., 0., w, h), &wash);
    // Clear the wash over cards; their own live surface remains visible underneath.
    let clear = dc.CreateSolidColorBrush(&color(0., 1. / 255.), None)?;
    for (_, r) in s.frames {
        let (bx, by) = badge_center(*r, m);
        let r = rect(
            (r.left - m.work.left) as f32 / dpi,
            (r.top - m.work.top) as f32 / dpi,
            (r.right - r.left) as f32 / dpi,
            (r.bottom - r.top) as f32 / dpi,
        );
        dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_COPY);
        dc.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: r,
                radiusX: 22.,
                radiusY: 22.,
            },
            &clear,
        );
        dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
        if s.quiet {
            continue;
        }
        rounded(
            dc,
            rect(bx - 11., by - 11., 22., 22.),
            11.,
            rgba(card::surface(theme.is_light)),
        )?;
        let b = dc.CreateSolidColorBrush(&rgba(theme.ink()), None)?;
        dc.DrawLine(
            Vector2 { X: bx - 4., Y: by },
            Vector2 { X: bx + 4., Y: by },
            &b,
            1.5,
            None,
        );
    }
    if s.chosen.is_some() {
        outlines(dc, m, s.places, s.ghost)?;
    }
    marks(dc, m, s.ghost, if s.ghost.is_some() { s.slots } else { &[] })?;
    if let Some(c) = s.chosen {
        if s.lift.is_none() {
            chosen(dc, gfx, theme, m, c)?;
        }
    }
    if let Some(l) = &s.lift {
        dragged(dc, gfx, theme, m, l, super::backdrop::under_on(l.at, m).as_ref())?;
    }
    if s.gallery {
        paint_panel(dc, gfx, theme, s, true)?;
    }
    if s.quiet {
        return Ok(());
    }
    crate::notes::widgets::pill(
        dc,
        gfx,
        crate::i18n::t("widgets.add_short"),
        rect(w / 2. - 104., h - 64., 112., 36.),
        false,
        theme.is_light,
    )?;
    crate::notes::widgets::pill(
        dc,
        gfx,
        crate::i18n::t("launchpad.edit.done"),
        rect(w / 2. + 20., h - 64., 84., 36.),
        true,
        theme.is_light,
    )?;
    Ok(())
}

fn rgba(c: crate::theme::Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: c.a as f32 / 255.,
    }
}

pub unsafe fn paint_panel(
    dc: &ID2D1DeviceContext,
    gfx: &Gfx,
    theme: &Theme,
    s: &Scene<'_>,
    backdrop: bool,
) -> Result<()> {
    let mut base = Matrix3x2::identity();
    dc.GetTransform(&mut base);
    let r = panel(s.monitor);
    let pw = r.right - r.left;
    let ph = r.bottom - r.top;
    let ink = if theme.is_light { 0. } else { 1. };
    dc.SetTransform(&(crate::companion::render::translate(r.left, r.top) * base));
    if backdrop {
        crate::gfx::soft_shadows(
            dc,
            rect(0., 0., pw, ph),
            20.,
            &[(
                0.,
                12.,
                28.,
                crate::theme::Rgba {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 75,
                },
            )],
        )?;
        rounded(
            dc,
            rect(0., 0., pw, ph),
            20.,
            rgba(card::surface(theme.is_light)),
        )?;
        let edge = dc.CreateSolidColorBrush(&color(ink, 0.16), None)?;
        dc.DrawRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: rect(0.5, 0.5, pw - 1., ph - 1.),
                radiusX: 20.,
                radiusY: 20.,
            },
            &edge,
            1.,
            None,
        );
    }
    for (i, k) in Kind::ALL.into_iter().enumerate() {
        crate::notes::widgets::pill(
            dc,
            gfx,
            k.label(),
            rect(20. + i as f32 * (pw - 40.) / 3., 20., (pw - 48.) / 3., 32.),
            k == s.kind,
            theme.is_light,
        )?;
    }
    let desk = super::widget::desk(s.monitor.scale());
    let Some(grid) = super::grid::Grid::widget(&s.monitor, desk.cell) else { dc.SetTransform(&base); return Ok(()); };
    let p = s.size.pixels(&grid, s.monitor.scale());
    let dims = (p.0 as f32 / s.monitor.scale(), p.1 as f32 / s.monitor.scale());
    let collection = s.kind == Kind::Note && !s.notes.is_empty();
    let room = (ph - if collection { 366. } else { 194. }).max(96.);
    let scale = ((pw - 64.) / dims.0).min(room / dims.1).min(1.);
    let x = (pw - dims.0 * scale) / 2.;
    let y = 76. + (room - dims.1 * scale) / 2.;
    dc.SetTransform(
        &(crate::companion::render::scale(scale, scale)
            * crate::companion::render::translate(r.left + x, r.top + y)
            * base),
    );
    // The preview over the wallpaper where it shows (screen px).
    let (dpi, work) = (s.monitor.scale(), s.monitor.work);
    let left = work.left + ((r.left + x) * dpi).round() as i32;
    let top = work.top + ((r.top + y) * dpi).round() as i32;
    let at = RECT { left, top, right: left + (dims.0 * scale * dpi).round() as i32, bottom: top + (dims.1 * scale * dpi).round() as i32 };
    let under = super::backdrop::under_for(s.preview_hwnd, at);
    card::plate(dc, theme, dims.0, dims.1, false, under.as_ref())?;
    let card_theme = card::theme_for(theme, under.as_ref());
    content(dc, gfx, card_theme, s.preview, s.size, dims)?;
    dc.SetTransform(&(crate::companion::render::translate(r.left, r.top) * base));
    if collection {
        label(dc, gfx, theme, crate::i18n::t("widgets.saved_notes"), 20., ph - 270., pw - 120.)?;
        crate::notes::widgets::pill(
            dc,
            gfx,
            "‹",
            rect(pw - 92., ph - 276., 32., 24.),
            false,
            theme.is_light,
        )?;
        crate::notes::widgets::pill(
            dc,
            gfx,
            "›",
            rect(pw - 56., ph - 276., 32., 24.),
            false,
            theme.is_light,
        )?;
        let y = ph - 244.;
        crate::notes::widgets::pill(
            dc,
            gfx,
            crate::i18n::t("notes.new"),
            rect(20., y, pw - 40., 26.),
            s.selected.is_none(),
            theme.is_light,
        )?;
        for (i, (id, title)) in s.notes.iter().take(3).enumerate() {
            crate::notes::widgets::pill(
                dc,
                gfx,
                title,
                rect(20., y + 28. + i as f32 * 28., pw - 40., 26.),
                s.selected == Some(id),
                theme.is_light,
            )?;
        }
    }
    label(
        dc,
        gfx,
        theme,
        &s.size.label(),
        pw / 2. - 20.,
        ph - 132.,
        80.,
    )?;
    crate::notes::widgets::pill(
        dc,
        gfx,
        "‹",
        rect(20., ph - 104., 36., 32.),
        false,
        theme.is_light,
    )?;
    crate::notes::widgets::pill(
        dc,
        gfx,
        "›",
        rect(pw - 56., ph - 104., 36., 32.),
        false,
        theme.is_light,
    )?;
    for (i, size) in Size::ALL.into_iter().enumerate() {
        rounded(
            dc,
            rect(pw / 2. - 19. + i as f32 * 16., ph - 91., 6., 6.),
            3.,
            color(ink, if size == s.size { 0.9 } else { 0.22 }),
        )?;
    }
    crate::notes::widgets::pill(
        dc,
        gfx,
        crate::i18n::t("widgets.add"),
        rect(20., ph - 56., pw - 40., 36.),
        true,
        theme.is_light,
    )?;

    dc.SetTransform(&base);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "offscreen translated widget editor, no desktop access"]
    fn localized_widget_editor() {
        use crate::gfx::promo_kit as kit;
        let stage = kit::stage();
        let root = std::env::temp_dir().join("topbar-one-shots/widgets");
        std::fs::create_dir_all(&root).unwrap();
        for lang in ["ko", "en"] {
            let _lang = crate::i18n::test_lock(lang);
            for light in [false, true] {
                for kind in Kind::ALL {
                    for size in Size::ALL {
                        let theme = kit::theme(light);
                        let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), 1280., 900., 1.);
                        let area = RECT { left: 0, top: 0, right: 1280, bottom: 900 };
                        let preview = match kind {
                            Kind::Battery => Preview::Battery(Default::default(), Vec::new()),
                            Kind::Note => Preview::Note(String::new(), String::new()),
                            Kind::Inbox => Preview::Inbox(Vec::new(), Vec::new(), 0),
                        };
                        let scene = Scene {
                            monitor: Monitor { handle: 0, rect: area, work: area, primary: true, dpi: 96 },
                            frames: &[], ghost: None, slots: &[], lift: None, gallery: true,
                            kind, size, preview: &preview, preview_hwnd: Default::default(), notes: &[], selected: None, chosen: None, quiet: false, places: &[],
                        };
                        unsafe {
                            cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                            paint(&cv.dc, &stage.gfx, &theme, &scene).unwrap();
                        }
                        let (px, w, h) = cv.finish();
                        kit::save_png(&px, w, h, &root.join(format!("{lang}-{}-{kind:?}-{size:?}.png", if light { "light" } else { "dark" })));
                    }
                }
            }
        }
    }
    #[test]
    fn gallery_hit_targets_follow_monitor_dpi_and_collection_pages() {
        for dpi in [96, 144] {
            let scale = dpi as f32 / 96.;
            let m = Monitor {
                handle: 0,
                rect: RECT {
                    left: -1920,
                    top: 0,
                    right: 0,
                    bottom: 1080,
                },
                work: RECT {
                    left: -1920,
                    top: 0,
                    right: -1920 + (1280. * scale) as i32,
                    bottom: (900. * scale) as i32,
                },
                primary: false,
                dpi,
            };
            let notes = vec![("id".into(), "보관한 메모".into())];
            let r = panel(m);
            let ph = r.bottom - r.top;
            let pw = r.right - r.left;
            assert_eq!(
                hit(m, (640. - 90., 850.), false, Kind::Battery, &[]),
                Hit::Gallery
            );
            assert_eq!(hit(m, (670., 850.), false, Kind::Battery, &[]), Hit::Done);
            assert_eq!(
                hit(
                    m,
                    (r.left + 30., r.top + ph - 40.),
                    true,
                    Kind::Note,
                    &notes
                ),
                Hit::Add
            );
            assert_eq!(
                hit(
                    m,
                    (r.left + pw - 40., r.top + ph - 90.),
                    true,
                    Kind::Note,
                    &notes
                ),
                Hit::Page(1)
            );
            assert_eq!(
                hit(
                    m,
                    (r.left + pw - 40., r.top + ph - 266.),
                    true,
                    Kind::Note,
                    &notes
                ),
                Hit::Collection(1)
            );
            assert_eq!(
                hit(
                    m,
                    (r.left + 30., r.top + ph - 210.),
                    true,
                    Kind::Note,
                    &notes
                ),
                Hit::Note("id".into())
            );
            assert_eq!(hit(m, (10., 10.), true, Kind::Note, &notes), Hit::None);
        }
    }
}
