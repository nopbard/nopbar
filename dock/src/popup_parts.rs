//! What a menu level shows beside its rows' picture (design system v2.1,
//! 6.2, 6.3 and 2.5): which fill lies where (the pointer's, the keyboard's
//! cursor, a press, the row whose submenu is up), how each comes and goes,
//! and the visuals that are things of their own: the fills, the bands a
//! long menu's rows fade out under, a slider's well and knob, a failure's
//! button, the progress ring. The rules are pure functions; popup.rs asks
//! them and drives these visuals.

use windows::core::{Interface, Result, HSTRING};
use windows::Foundation::TimeSpan;
use windows::UI::Color;
use windows::UI::Composition::{
    AnimationIterationBehavior, CompositionColorBrush, CompositionDrawingSurface, CompositionRoundedRectangleGeometry, CompositionSpriteShape, CompositionStretch, CompositionSurfaceBrush, Compositor,
    ContainerVisual, ShapeVisual, SpriteVisual, Visual,
};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1RenderTarget, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES};
use windows_numerics::{Vector2, Vector3};

use super::{grid_col, grid_mark, nav_span, Item, NavPart, EVENT_GAP, NAV_BTN, ROW_INSET};
use crate::gfx::Gfx;
use crate::motion::{self, Curve};
use crate::theme::metrics::{size, space};
use crate::theme::{Level, Rgba, Role, State, Theme};

/// Something of a level that answers the pointer or holds the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A whole row.
    Row(usize),
    /// A part of a month header (its row).
    Nav(usize, NavPart),
    /// A day of a week row (its row, its column).
    Cell(usize, usize),
    /// A failure row's button (its row): raised, it answers by itself.
    Button(usize),
}

impl Target {
    pub fn row(self) -> usize {
        match self {
            Target::Row(r) | Target::Nav(r, _) | Target::Cell(r, _) | Target::Button(r) => r,
        }
    }
    /// Lit by a fill laid on the surface (a button is not: it is raised).
    fn inked(self) -> bool {
        !matches!(self, Target::Button(_))
    }
}

/// Why the one instant fill of a level is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// The button is down on it.
    Pressed,
    /// Its submenu is up: it keeps the pressed fill.
    Open,
    /// The keyboard's place: the selected fill, no ring.
    Cursor,
}

impl Tone {
    pub fn state(self) -> State {
        match self {
            Tone::Pressed => State::Pressed,
            Tone::Open => State::Open,
            Tone::Cursor => State::Cursor,
        }
    }
}

/// What a level's fills show: the pointer's (fading) and the instant one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Glows {
    pub hover: Option<Target>,
    pub mark: Option<(Target, Tone)>,
}

/// Pure: the fills of a level (6.2). There is one emphasis: the pointer's
/// hover or the keyboard's cursor, whichever moved last (`by_key`). A
/// press wins over the row whose submenu is `open`, that over the cursor;
/// what the instant fill lies on has no hover fill under it (one fill, at
/// its own strength).
pub fn glows(pointer: Option<Target>, cursor: Option<Target>, by_key: bool, pressed: Option<Target>, open: Option<usize>) -> Glows {
    let mark = pressed
        .filter(|t| t.inked())
        .map(|t| (t, Tone::Pressed))
        .or(open.map(|r| (Target::Row(r), Tone::Open)))
        .or(cursor.filter(|_| by_key).map(|t| (t, Tone::Cursor)));
    let hover = pointer.filter(|t| !by_key && t.inked()).filter(|t| mark.map(|m| m.0) != Some(*t));
    Glows { hover, mark }
}

/// How a fill changes: at once, or over a fade (ms at speed 1, its curve).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Go {
    Now,
    Fade(i64, Curve),
}

/// Pure: how the pointer's fill comes on (6.3): over FADE_IN_MS; at once
/// in high contrast (`hc`: a fading outline passes through colors that
/// break its contrast, 12.3).
pub fn hover_on(hc: bool) -> Go {
    if hc {
        Go::Now
    } else {
        Go::Fade(motion::FADE_IN_MS, motion::EASE_OUT)
    }
}

/// Pure: how the pointer's fill goes: over FADE_OUT_MS when the pointer
/// left; at once when the keyboard took over (`by_key`) or the instant
/// fill now lies there (`covered`: a press), and in high contrast.
pub fn hover_off(by_key: bool, covered: bool, hc: bool) -> Go {
    if by_key || covered || hc {
        Go::Now
    } else {
        Go::Fade(motion::FADE_OUT_MS, motion::EASE_IN)
    }
}

/// Pure: how the instant fill goes when nothing takes its place: the
/// cursor at once (a key leaves no trail), a press let go over FADE_IN_MS
/// (back to the hover under it), an open row over FADE_OUT_MS as its
/// submenu starts to close.
pub fn mark_off(tone: Tone, hc: bool) -> Go {
    match tone {
        _ if hc => Go::Now,
        Tone::Cursor => Go::Now,
        Tone::Pressed => Go::Fade(motion::FADE_IN_MS, motion::EASE_OUT),
        Tone::Open => Go::Fade(motion::FADE_OUT_MS, motion::EASE_IN),
    }
}

/// Pure: where `t` is lit, (x, y, w, h, corner) DIP in the rows' own
/// space, in a level `width` wide whose row fills have corners `row_r`: a
/// row from vessel line to vessel line (an agenda row: its card), a month
/// header's title its span, its buttons and a week's days circles on their
/// columns. None for what is not there.
pub fn target_box(t: Target, items: &[Item], rows: &[(f32, f32)], width: f32, rtl: bool, row_r: f32) -> Option<(f32, f32, f32, f32, f32)> {
    let r = t.row();
    let (it, &(y, h)) = (items.get(r)?, rows.get(r)?);
    Some(match t {
        Target::Row(_) | Target::Button(_) => {
            let (y, h) = if it.event.is_some() { (y + EVENT_GAP / 2.0, h - EVENT_GAP) } else { (y, h) };
            (ROW_INSET, y, width - 2.0 * ROW_INSET, h, row_r)
        }
        Target::Nav(_, NavPart::Title) => {
            let (x0, w) = nav_span(NavPart::Title, width, rtl);
            let x1 = (x0 + w).min(width - ROW_INSET);
            let x0 = x0.max(ROW_INSET);
            (x0, y, x1 - x0, h, row_r)
        }
        Target::Nav(_, part) => {
            let (x0, w) = nav_span(part, width, rtl);
            let side = NAV_BTN.min(w).min(h);
            (x0 + (w - side) / 2.0, y + (h - side) / 2.0, side, side, side / 2.0)
        }
        Target::Cell(_, k) => {
            let n = it.cells.len();
            if k >= n {
                return None;
            }
            let col = grid_col(width, n);
            let side = grid_mark(col).0.min(h);
            let cx = space::S4 + col * (k as f32 + 0.5);
            let cx = if rtl { width - cx } else { cx };
            (cx - side / 2.0, y + (h - side) / 2.0, side, side, side / 2.0)
        }
    })
}

/// Pure: the runs of `time` (UTF-16 start, length) that are its day
/// period ("AM", "오전", "a.m."): letters, and the dots inside them. The
/// clock draws them small beside its digits; a 24 hour time has none.
pub fn period_runs(time: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let (mut at, mut open): (u32, Option<u32>) = (0, None);
    let mut prev_letter = false;
    for c in time.chars() {
        let n = c.len_utf16() as u32;
        let part = c.is_alphabetic() || (c == '.' && prev_letter);
        match (part, open) {
            (true, None) => open = Some(at),
            (false, Some(start)) => {
                out.push((start, at - start));
                open = None;
            }
            _ => {}
        }
        prev_letter = c.is_alphabetic() || (c == '.' && prev_letter);
        at += n;
    }
    if let Some(start) = open {
        out.push((start, at - start));
    }
    out
}

fn color(c: Rgba) -> Color {
    Color { A: c.a, R: c.r, G: c.g, B: c.b }
}

fn d2d(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

/// `v`'s opacity set at once (what animates it stops).
pub fn set_now(v: &impl Interface, opacity: f32) {
    if let Ok(v) = v.cast::<Visual>() {
        let _ = v.StopAnimation(&HSTRING::from("Opacity"));
        let _ = v.SetOpacity(opacity);
    }
}

/// `v`'s opacity to `to` as `go` says (a fade: motion::fade_to, so reduced
/// motion and the speed setting are its own); `from_zero`: it comes in at
/// a new place, from nothing.
pub fn go_to(c: &Compositor, v: &impl Interface, to: f32, go: Go, from_zero: bool) {
    match go {
        Go::Now => set_now(v, to),
        Go::Fade(ms, curve) => {
            let from = if from_zero { motion::Start::At(0.0) } else { motion::Start::Current };
            if motion::fade_from(c, v, from, to, ms, curve).is_err() {
                set_now(v, to);
            }
        }
    }
}

/// One fill of a level: a color in a rounded clip, placed on a target.
/// High contrast's hover has no fill: an outline inside its edge instead.
pub struct Glow {
    pub v: SpriteVisual,
    geom: CompositionRoundedRectangleGeometry,
    brush: CompositionColorBrush,
    outline: Option<(CompositionRoundedRectangleGeometry, f32)>,
}

impl Glow {
    /// A fill in `fill`, with an outline (color, DIP) inside its edge if
    /// given; clear until it is lit.
    pub fn new(c: &Compositor, fill: Rgba, outline: Option<(Rgba, f32)>) -> Result<Glow> {
        let v = c.CreateSpriteVisual()?;
        let brush = c.CreateColorBrushWithColor(color(fill))?;
        v.SetBrush(&brush)?;
        let geom = c.CreateRoundedRectangleGeometry()?;
        v.SetClip(&c.CreateGeometricClipWithGeometry(&geom)?)?;
        let mut line = None;
        if let Some((col, w)) = outline {
            let shape_v = c.CreateShapeVisual()?;
            shape_v.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
            let og = c.CreateRoundedRectangleGeometry()?;
            og.SetOffset(Vector2 { X: w / 2.0, Y: w / 2.0 })?;
            let shape = c.CreateSpriteShapeWithGeometry(&og)?;
            shape.SetStrokeBrush(&c.CreateColorBrushWithColor(color(Rgba { a: 255, ..col }))?)?;
            shape.SetStrokeThickness(w)?;
            shape_v.Shapes()?.Append(&shape)?;
            v.Children()?.InsertAtTop(&shape_v)?;
            line = Some((og, w));
        }
        v.SetOpacity(0.0)?;
        Ok(Glow { v, geom, brush, outline: line })
    }

    /// Puts it on (x, y, w, h, corner).
    pub fn place(&self, (x, y, w, h, r): (f32, f32, f32, f32, f32)) {
        let size = Vector2 { X: w.max(0.0), Y: h.max(0.0) };
        let _ = self.v.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
        let _ = self.v.SetSize(size);
        let _ = self.geom.SetSize(size);
        let _ = self.geom.SetCornerRadius(Vector2 { X: r, Y: r });
        if let Some((g, t)) = &self.outline {
            let _ = g.SetSize(Vector2 { X: (w - t).max(0.0), Y: (h - t).max(0.0) });
            let k = (r - t / 2.0).max(0.0);
            let _ = g.SetCornerRadius(Vector2 { X: k, Y: k });
        }
    }

    /// Its fill, at once.
    pub fn fill(&self, c: Rgba) {
        let _ = self.v.SetBrush(&self.brush);
        let _ = self.brush.SetColor(color(c));
    }
}

/// A vertical gradient of alpha (0 at one end, 1 at the other) on a
/// surface `w` x `h` px: the mask of a scroll edge's band (2.5). `up`: the
/// top band (clear at its top).
fn band_mask(gfx: &Gfx, w: u32, h: u32, up: bool) -> Result<windows::UI::Composition::CompositionSurfaceBrush> {
    let surface = gfx.draw(w.max(1), h.max(1), |dc, ox, oy| unsafe {
        let at = |a: f32| D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a };
        let (a0, a1) = if up { (0.0, 1.0) } else { (1.0, 0.0) };
        let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: at(a0) }, D2D1_GRADIENT_STOP { position: 1.0, color: at(a1) }];
        let rt: ID2D1RenderTarget = dc.cast()?;
        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: ox, Y: oy }, endPoint: Vector2 { X: ox, Y: oy + h as f32 } };
        let brush = rt.CreateLinearGradientBrush(&props, None, &coll)?;
        dc.FillRectangle(&D2D_RECT_F { left: ox, top: oy, right: ox + w as f32, bottom: oy + h as f32 }, &brush);
        Ok(())
    })?;
    let b = gfx.compositor.CreateSurfaceBrushWithSurface(&surface)?;
    b.SetStretch(CompositionStretch::Fill)?;
    Ok(b)
}

/// The scroll edges of a menu taller than its room (2.5): its rows are
/// shown through three slices of one picture of them (visual surfaces
/// over the same rows: nothing is drawn twice). The middle shows them as
/// they are. Each band shows them once, under an alpha mask of two
/// layers: a gradient, 0 at the menu's edge to 1 a band in, and over it a
/// cover of full alpha. The cover is there while the menu is at that end
/// (the mask is whole: the rows as they are) and fades away when rows are
/// cut off there, which leaves the gradient: a band of alpha on the
/// content, not a band of color over it. The rows are composited once
/// either way, so a see-through fill under a band that is off (search's
/// state fills) is as strong there as in the middle (2026-11-11 review: a
/// masked copy under a plain one made it stronger).
/// High contrast has no band: a 1 DIP line in the text color at a cut
/// edge instead.
pub struct Edges {
    /// What goes into the menu's plate.
    pub stage: ContainerVisual,
    /// Holds the rows (never in the plate's own tree): its child is the
    /// scaled viewport the scroller lives in.
    _source: ContainerVisual,
    /// Per band (top, bottom), what turns it: the mask's cover (opacity 1:
    /// no band), or in high contrast the cut line (opacity 1: cut).
    turn: [Visual; 2],
    /// The bands' masks (the sources of their visual surfaces).
    _masks: Vec<ContainerVisual>,
    hc: bool,
    shown: (bool, bool),
}

impl Edges {
    /// For rows in `viewport` (DIP, `width` x `height`, bands `band`) at
    /// `s` px per DIP. `viewport` must not have a parent yet.
    pub fn new(gfx: &Gfx, theme: &Theme, viewport: &ContainerVisual, (width, height, band): (f32, f32, f32), s: f32, bands: (bool, bool)) -> Result<Edges> {
        let c = &gfx.compositor;
        let (w_px, h_px, b_px) = ((width * s).ceil(), (height * s).ceil(), (band * s).round().max(1.0));
        // The source in px: a surface draws its visual 1:1, so the DPI
        // scale has to be inside it for sharp text.
        let source = c.CreateContainerVisual()?;
        source.SetSize(Vector2 { X: w_px, Y: h_px })?;
        viewport.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
        source.Children()?.InsertAtTop(viewport)?;
        let slice = |y_px: f32, hh_px: f32| -> Result<windows::UI::Composition::CompositionSurfaceBrush> {
            let vs = c.CreateVisualSurface()?;
            vs.SetSourceVisual(&source)?;
            vs.SetSourceOffset(Vector2 { X: 0.0, Y: y_px })?;
            vs.SetSourceSize(Vector2 { X: w_px, Y: hh_px })?;
            let b = c.CreateSurfaceBrushWithSurface(&vs)?;
            b.SetStretch(CompositionStretch::Fill)?;
            Ok(b)
        };
        let stage = c.CreateContainerVisual()?;
        stage.SetSize(Vector2 { X: width, Y: height })?;
        let sprite = |y: f32, h: f32| -> Result<SpriteVisual> {
            let v = c.CreateSpriteVisual()?;
            v.SetOffset(Vector3 { X: 0.0, Y: y, Z: 0.0 })?;
            v.SetSize(Vector2 { X: width, Y: h })?;
            stage.Children()?.InsertAtTop(&v)?;
            Ok(v)
        };
        // Bands on whole pixels, so the slices meet without a seam.
        let band = b_px / s;
        let mid = sprite(band, height - 2.0 * band)?;
        mid.SetBrush(&slice(b_px, h_px - 2.0 * b_px)?)?;
        let hc = theme.high_contrast && theme.hc.is_some();
        let (mut turn, mut masks) = (Vec::new(), Vec::new());
        for (up, y, y_px) in [(true, 0.0, 0.0), (false, height - band, h_px - b_px)] {
            let p = sprite(y, band)?;
            if let Some(h) = theme.hc.filter(|_| hc) {
                p.SetBrush(&slice(y_px, b_px)?)?;
                // The cut edge's line, shown with the cut.
                let line = c.CreateSpriteVisual()?;
                line.SetBrush(&c.CreateColorBrushWithColor(color(Rgba { a: 255, ..h.text }))?)?;
                line.SetSize(Vector2 { X: width, Y: 1.0 })?;
                line.SetOffset(Vector3 { X: 0.0, Y: if up { 0.0 } else { band - 1.0 }, Z: 0.0 })?;
                line.SetComment(&HSTRING::from("nb-cut"))?;
                p.Children()?.InsertAtTop(&line)?;
                turn.push(line.cast::<Visual>()?);
                continue;
            }
            // The mask: the gradient and the cover over it, MASK_W x b_px
            // px, stretched over the band.
            let m = c.CreateContainerVisual()?;
            let size = Vector2 { X: MASK_W, Y: b_px };
            m.SetSize(size)?;
            let grad = c.CreateSpriteVisual()?;
            grad.SetSize(size)?;
            grad.SetBrush(&band_mask(gfx, MASK_W as u32, b_px as u32, up)?)?;
            m.Children()?.InsertAtTop(&grad)?;
            let cover = c.CreateSpriteVisual()?;
            cover.SetSize(size)?;
            cover.SetBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: 255, G: 255, B: 255 })?)?;
            m.Children()?.InsertAtTop(&cover)?;
            let vs = c.CreateVisualSurface()?;
            vs.SetSourceVisual(&m)?;
            vs.SetSourceSize(size)?;
            let mb = c.CreateSurfaceBrushWithSurface(&vs)?;
            mb.SetStretch(CompositionStretch::Fill)?;
            let mask = c.CreateMaskBrush()?;
            mask.SetSource(&slice(y_px, b_px)?)?;
            mask.SetMask(&mb)?;
            p.SetBrush(&mask)?;
            turn.push(cover.cast::<Visual>()?);
            masks.push(m);
        }
        let turn: [Visual; 2] = [turn[0].clone(), turn[1].clone()];
        let mut e = Edges { stage, _source: source, turn, _masks: masks, hc, shown: (!bands.0, !bands.1) };
        e.jump(bands, hc);
        Ok(e)
    }

    /// Pure: `turn`'s opacity with its band `on` (the cover goes with the
    /// band; high contrast's line comes with it).
    fn level(hc: bool, on: bool) -> f32 {
        if on == hc {
            1.0
        } else {
            0.0
        }
    }

    /// The bands as `bands` (top, bottom) says, at once (a picture's own
    /// band beside them comes and goes with its paint: search). The look
    /// the edges were made in decides how (`_hc` is that look).
    pub fn jump(&mut self, bands: (bool, bool), _hc: bool) {
        for (i, on) in [bands.0, bands.1].into_iter().enumerate() {
            set_now(&self.turn[i], Self::level(self.hc, on));
        }
        self.shown = bands;
    }

    /// The bands that show now: `bands` (top, bottom). One comes on over
    /// FADE_IN_MS and goes over FADE_OUT_MS (2.5); high contrast's line at
    /// once.
    pub fn set(&mut self, c: &Compositor, bands: (bool, bool), _hc: bool) {
        let was = [self.shown.0, self.shown.1];
        for (i, on) in [bands.0, bands.1].into_iter().enumerate() {
            if was[i] == on {
                continue;
            }
            let to = Self::level(self.hc, on);
            if self.hc {
                set_now(&self.turn[i], to);
                continue;
            }
            // The band shows as the mask's cover goes.
            let go = if on { Go::Fade(motion::FADE_IN_MS, motion::EASE_OUT) } else { Go::Fade(motion::FADE_OUT_MS, motion::EASE_IN) };
            go_to(c, &self.turn[i], to, go, false);
        }
        self.shown = bands;
    }

    /// Test: per band (top, bottom), the opacity of what turns it (the
    /// cover, or high contrast's line), and how many visuals show the rows
    /// (one per slice: nothing composited twice).
    #[cfg(test)]
    pub fn probe(&self) -> ([f32; 2], usize) {
        let n = self.stage.Children().and_then(|k| k.Count()).unwrap_or(0) as usize;
        ([self.turn[0].Opacity().unwrap_or(-1.0), self.turn[1].Opacity().unwrap_or(-1.0)], n)
    }
}

/// A band mask's width (px): it changes only down the band and is
/// stretched across it.
const MASK_W: f32 = 4.0;

/// A slider's well: the track sunk into the menu (gfx::parts::well: the
/// track fill and its upper wall's shade), `w` x TRACK_H DIP at `s`.
pub fn slider_well(gfx: &Gfx, theme: &Theme, w: f32, s: f32) -> Result<SpriteVisual> {
    let h = size::TRACK_H;
    let (w_px, h_px) = ((w * s).ceil().max(1.0), (h * s).ceil().max(1.0));
    let surface = gfx.draw(w_px as u32, h_px as u32, |dc, ox, oy| unsafe {
        let r = D2D_RECT_F { left: ox, top: oy, right: ox + w * s, bottom: oy + h * s };
        crate::gfx::parts::well(dc, r, crate::theme::capsule(h) * s, theme, s)
    })?;
    let c = &gfx.compositor;
    let v = c.CreateSpriteVisual()?;
    let b = c.CreateSurfaceBrushWithSurface(&surface)?;
    b.SetStretch(CompositionStretch::Fill)?;
    v.SetBrush(&b)?;
    v.SetSize(Vector2 { X: w, Y: h })?;
    Ok(v)
}

/// Room around a slider's knob for its E1 shadow and for growing when
/// held (DIP, each side).
pub const KNOB_M: f32 = space::S4;

/// Pure: the side of the knob's visual (the knob and KNOB_M around it).
pub fn knob_box() -> f32 {
    size::KNOB + KNOB_M * 2.0
}

/// A slider's knob (gfx::parts::knob: its fill, the contact shadow, E1),
/// knob_box() DIP square at `s`, scaling about its middle.
pub fn slider_knob(gfx: &Gfx, theme: &Theme, s: f32) -> Result<SpriteVisual> {
    let side = (knob_box() * s).ceil();
    let surface = gfx.draw(side as u32, side as u32, |dc, ox, oy| unsafe {
        let c = knob_box() * s / 2.0;
        crate::gfx::parts::knob(dc, ox + c, oy + c, size::KNOB * s / 2.0, theme, false, s)
    })?;
    let c = &gfx.compositor;
    let v = c.CreateSpriteVisual()?;
    let b = c.CreateSurfaceBrushWithSurface(&surface)?;
    b.SetStretch(CompositionStretch::Fill)?;
    v.SetBrush(&b)?;
    v.SetSize(Vector2 { X: knob_box(), Y: knob_box() })?;
    v.SetCenterPoint(Vector3 { X: knob_box() / 2.0, Y: knob_box() / 2.0, Z: 0.0 })?;
    Ok(v)
}

/// A failure row's button (11.4): raised (the card fills: lighter under
/// the pointer, darker and a little smaller pressed), CONTROL_H tall, its
/// corners the control radius, the contact shadow round it (2.6: a card
/// fill on a plate of nearly the same color keeps its outline). Its text
/// and its outline follow its state as a row's do (theme::text_of,
/// outline_of; high contrast, 12.3: HighlightText on the Highlight fill
/// pressed, a 2 DIP Highlight line under the pointer) and the state of the
/// row it is on: on a row with the keyboard's cursor (high contrast: the
/// Highlight fill) it answers on that (theme::fill_on, outline_on,
/// text_on).
pub struct Button {
    pub v: ContainerVisual,
    brush: CompositionColorBrush,
    /// Flat v2's card light round its fill (a button is raised: a card).
    card: crate::wash::CardLight,
    /// Its text in each color a state asks for, drawn once each, and the
    /// brush that shows one of them.
    labels: Vec<(Rgba, CompositionDrawingSurface)>,
    text: CompositionSurfaceBrush,
    /// Its outline (high contrast only).
    line: Option<Line>,
    size: (f32, f32),
    radius: f32,
    state: State,
    /// The state of the row under it.
    ground: State,
}

/// A button's outline, inside its edge: its color and width per state.
struct Line {
    v: ShapeVisual,
    shape: CompositionSpriteShape,
    geo: CompositionRoundedRectangleGeometry,
    brush: CompositionColorBrush,
}

/// A button's side padding and height.
pub const BUTTON_PAD: f32 = size::BUTTON_PAD_X;
pub const BUTTON_H: f32 = size::CONTROL_H;

/// The states a button shows (6.2): it is never selected or open.
const BUTTON_STATES: [State; 3] = [State::Rest, State::Hover, State::Pressed];
/// What its row can be doing under it: nothing, or holding the keyboard's
/// cursor (a failure row answers nothing else: only its button takes the
/// pointer).
const BUTTON_GROUNDS: [State; 2] = [State::Rest, State::Cursor];

/// Pure: the state of a failure row under its button from the level's
/// instant fill: the row's own (its cursor), else at rest.
pub fn ground_of(row: usize, mark: Option<(Target, Tone)>) -> State {
    match mark {
        Some((Target::Row(r), tone)) if r == row => tone.state(),
        _ => State::Rest,
    }
}

impl Button {
    /// `label(ink)`: its text drawn in `ink` (w, h DIP); the button is
    /// that and BUTTON_PAD either side wide.
    pub fn new(gfx: &Gfx, theme: &Theme, label: &dyn Fn(Rgba) -> Result<CompositionDrawingSurface>, (lw, lh): (f32, f32)) -> Result<Button> {
        let c = &gfx.compositor;
        let (w, h) = ((lw + 2.0 * BUTTON_PAD).ceil(), BUTTON_H);
        let r = crate::theme::concentric(theme.radii().menu, size::MENU_PAD).min(h / 2.0);
        let v = c.CreateContainerVisual()?;
        v.SetSize(Vector2 { X: w, Y: h })?;
        let contact = theme.contact();
        if contact.a > 0 {
            let k = crate::theme::CONTACT_W;
            let g = c.CreateRoundedRectangleGeometry()?;
            g.SetSize(Vector2 { X: w + k, Y: h + k })?;
            g.SetOffset(Vector2 { X: 1.0 - k / 2.0, Y: 1.0 - k / 2.0 })?;
            g.SetCornerRadius(Vector2 { X: r + k / 2.0, Y: r + k / 2.0 })?;
            let shape = c.CreateSpriteShapeWithGeometry(&g)?;
            shape.SetStrokeBrush(&c.CreateColorBrushWithColor(color(contact))?)?;
            shape.SetStrokeThickness(k)?;
            let ring = c.CreateShapeVisual()?;
            ring.Shapes()?.Append(&shape)?;
            ring.SetSize(Vector2 { X: w + 2.0, Y: h + 2.0 })?;
            ring.SetOffset(Vector3 { X: -1.0, Y: -1.0, Z: 0.0 })?;
            v.Children()?.InsertAtTop(&ring)?;
        }
        let fill = c.CreateSpriteVisual()?;
        let brush = c.CreateColorBrushWithColor(color(theme.fill_on(Role::Raised, State::Rest, State::Rest)))?;
        fill.SetBrush(&brush)?;
        fill.SetSize(Vector2 { X: w, Y: h })?;
        let clip = c.CreateRoundedRectangleGeometry()?;
        clip.SetSize(Vector2 { X: w, Y: h })?;
        clip.SetCornerRadius(Vector2 { X: r, Y: r })?;
        fill.SetClip(&c.CreateGeometricClipWithGeometry(&clip)?)?;
        let line = if BUTTON_STATES.iter().any(|&st| BUTTON_GROUNDS.iter().any(|&g| theme.outline_on(Role::Raised, st, g).is_some())) {
            let geo = c.CreateRoundedRectangleGeometry()?;
            let shape = c.CreateSpriteShapeWithGeometry(&geo)?;
            let brush = c.CreateColorBrush()?;
            shape.SetStrokeBrush(&brush)?;
            let lv = c.CreateShapeVisual()?;
            lv.Shapes()?.Append(&shape)?;
            lv.SetSize(Vector2 { X: w, Y: h })?;
            fill.Children()?.InsertAtTop(&lv)?;
            Some(Line { v: lv, shape, geo, brush })
        } else {
            None
        };
        v.Children()?.InsertAtTop(&fill)?;
        let mut card = crate::wash::CardLight::new(c, &v.Children()?, &fill, &brush.cast()?)?;
        card.place(crate::wash::Shape::new(0.0, 0.0, w, h, r));
        card.show(c, theme, State::Rest, 0, motion::EASE_OUT);
        // Its text in every color its states use on every row state (one
        // outside high contrast).
        let mut labels: Vec<(Rgba, CompositionDrawingSurface)> = Vec::new();
        for st in BUTTON_STATES {
            for g in BUTTON_GROUNDS {
                let ink = theme.text_on(Level::Primary, st, g);
                if !labels.iter().any(|l| l.0 == ink) {
                    labels.push((ink, label(ink)?));
                }
            }
        }
        let text = c.CreateSpriteVisual()?;
        let tb = c.CreateSurfaceBrush()?;
        tb.SetStretch(CompositionStretch::Fill)?;
        text.SetBrush(&tb)?;
        text.SetSize(Vector2 { X: lw, Y: lh })?;
        text.SetOffset(Vector3 { X: ((w - lw) / 2.0).round(), Y: ((h - lh) / 2.0).round(), Z: 0.0 })?;
        v.Children()?.InsertAtTop(&text)?;
        let b = Button { v, brush, card, labels, text: tb, line, size: (w, h), radius: r, state: State::Rest, ground: State::Rest };
        b.show(theme)?;
        Ok(b)
    }

    pub fn size(&self) -> (f32, f32) {
        self.size
    }

    /// Its text and outline for its state on its row's, at once (drawn,
    /// not faded; in high contrast nothing fades anyway).
    fn show(&self, theme: &Theme) -> Result<()> {
        let (state, ground) = (self.state, self.ground);
        let ink = theme.text_on(Level::Primary, state, ground);
        if let Some((_, s)) = self.labels.iter().find(|l| l.0 == ink) {
            self.text.SetSurface(s)?;
        }
        let Some(line) = &self.line else { return Ok(()) };
        let (w, h) = self.size;
        match theme.outline_on(Role::Raised, state, ground) {
            Some((col, t)) => {
                let r = (self.radius - t / 2.0).max(0.0);
                line.geo.SetSize(Vector2 { X: w - t, Y: h - t })?;
                line.geo.SetOffset(Vector2 { X: t / 2.0, Y: t / 2.0 })?;
                line.geo.SetCornerRadius(Vector2 { X: r, Y: r })?;
                line.shape.SetStrokeThickness(t)?;
                line.brush.SetColor(color(col))?;
                line.v.SetIsVisible(true)
            }
            None => line.v.SetIsVisible(false),
        }
    }

    /// Its card light now (tests): the drop's and the light's colors.
    #[cfg(test)]
    pub fn card_shows(&self) -> (Rgba, Rgba) {
        self.card.shows()
    }

    /// What it shows now, read off its visuals: its text's color and its
    /// outline (color, DIP) if one shows.
    #[cfg(test)]
    pub fn shows(&self) -> (Option<Rgba>, Option<(Rgba, f32)>) {
        use windows::UI::Composition::ICompositionSurface;
        let now = self.text.Surface().ok();
        let ink = self.labels.iter().find(|(_, s)| s.cast::<ICompositionSurface>().ok().is_some_and(|s| now.as_ref() == Some(&s))).map(|l| l.0);
        let line = self.line.as_ref().filter(|l| l.v.IsVisible().unwrap_or(false)).map(|l| {
            let c = l.brush.Color().unwrap();
            (Rgba { r: c.R, g: c.G, b: c.B, a: c.A }, l.shape.StrokeThickness().unwrap())
        });
        (ink, line)
    }

    /// Its state (6.2, 6.3) on its row's `ground`: the fill at once when
    /// pressed, over FADE_IN_MS coming up under the pointer or back from a
    /// press, over FADE_OUT_MS when the pointer leaves; pressed it shrinks
    /// by press_scale on SNAP. Its text and outline change with it. The
    /// row's cursor came or went (a key): at once.
    pub fn set(&mut self, c: &Compositor, theme: &Theme, state: State, ground: State) {
        if state == self.state && ground == self.ground {
            return;
        }
        let was = std::mem::replace(&mut self.state, state);
        let moved = std::mem::replace(&mut self.ground, ground) != ground;
        let _ = self.show(theme);
        let to = theme.fill_on(Role::Raised, state, ground);
        let ms = match (was, state) {
            _ if moved => 0,
            (_, State::Pressed) => 0,
            (_, State::Rest) => motion::FADE_OUT_MS,
            _ => motion::FADE_IN_MS,
        };
        let curve = if state == State::Rest { motion::EASE_IN } else { motion::EASE_OUT };
        color_to(c, &self.brush, to, if theme.high_contrast { 0 } else { ms }, curve);
        self.card.show(c, theme, state, ms, curve);
        if (was == State::Pressed) != (state == State::Pressed) {
            let _ = motion::press(c, &self.v, self.size, state == State::Pressed);
        }
    }
}

/// `brush`'s color to `to` over `ms` (at speed 1) on `curve`, as
/// motion::fade_now plays a fade (reduced motion: REDUCED_MS at most,
/// straight); at once for 0.
pub fn color_to(c: &Compositor, brush: &CompositionColorBrush, to: Rgba, ms: i64, curve: Curve) {
    let prop = HSTRING::from("Color");
    let (ms, curve) = motion::fade_now(ms, curve);
    let run = || -> Result<()> {
        let a = c.CreateColorKeyFrameAnimation()?;
        a.InsertKeyFrameWithEasingFunction(1.0, color(to), &motion::easing(c, curve)?)?;
        a.SetDuration(TimeSpan { Duration: ms.max(1) * 10_000 })?;
        brush.StartAnimation(&prop, &a)
    };
    if ms <= 0 || run().is_err() {
        let _ = brush.StopAnimation(&prop);
        let _ = brush.SetColor(color(to));
    }
}

/// The progress ring (11.3): 16 DIP, a 270 degree arc in the secondary
/// text color, one turn in SPINNER_TURN_MS on a straight line for as long
/// as it shows. With reduced motion it does not turn (12.2).
pub fn spinner(gfx: &Gfx, theme: &Theme, s: f32) -> Result<SpriteVisual> {
    let side = size::SPINNER;
    let px = (side * s).ceil().max(1.0);
    let surface = gfx.draw(px as u32, px as u32, |dc, ox, oy| unsafe { crate::gfx::parts::spinner(dc, ox + side * s / 2.0, oy + side * s / 2.0, 0.0, theme, s) })?;
    let c = &gfx.compositor;
    let v = c.CreateSpriteVisual()?;
    let b = c.CreateSurfaceBrushWithSurface(&surface)?;
    b.SetStretch(CompositionStretch::Fill)?;
    v.SetBrush(&b)?;
    v.SetSize(Vector2 { X: side, Y: side })?;
    v.SetCenterPoint(Vector3 { X: side / 2.0, Y: side / 2.0, Z: 0.0 })?;
    turn(c, &v, motion::full())?;
    Ok(v)
}

/// Turns `spinner`'s ring (`on`: one turn in SPINNER_TURN_MS, for ever) or
/// stops it where it starts (reduced motion switched on while it shows:
/// 12.2, nothing keeps moving).
pub fn turn(c: &windows::UI::Composition::Compositor, v: &SpriteVisual, on: bool) -> Result<()> {
    let prop = HSTRING::from("RotationAngleInDegrees");
    if !on {
        let _ = v.StopAnimation(&prop);
        return v.SetRotationAngleInDegrees(0.0);
    }
    let a = c.CreateScalarKeyFrameAnimation()?;
    a.InsertKeyFrame(0.0, 0.0)?;
    a.InsertKeyFrameWithEasingFunction(1.0, 360.0, &c.CreateLinearEasingFunction()?)?;
    a.SetDuration(TimeSpan { Duration: crate::gfx::parts::SPINNER_TURN_MS as i64 * 10_000 })?;
    a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
    v.StartAnimation(&prop, &a)
}

/// The d2d color of `c` (the rows' paint).
pub fn ink(c: Rgba) -> D2D1_COLOR_F {
    d2d(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panels' confirm review (2026-11-11, P2): a band that is off
    /// showed the rows twice (a masked copy under the plain one). The rows
    /// are shown once per slice now, through one mask whose cover turns
    /// the band: off = the cover (the rows as they are), on = no cover
    /// (the gradient alone); a menu's fade on and off ends there too. High
    /// contrast: no mask, the cut line comes and goes. (The pixels:
    /// chrome_harmony's harmony_edges_composite_once.)
    #[test]
    fn a_band_shows_the_rows_once() {
        assert_eq!((Edges::level(false, false), Edges::level(false, true)), (1.0, 0.0));
        assert_eq!((Edges::level(true, false), Edges::level(true, true)), (0.0, 1.0));
        let stage = crate::gfx::promo_kit::stage();
        let c = &stage.gfx.compositor;
        let mut hc = crate::gfx::promo_kit::theme(false);
        hc.hc = Some(crate::syslook::HighContrast {
            window: Rgba { r: 0, g: 0, b: 0, a: 255 },
            text: Rgba { r: 255, g: 255, b: 255, a: 255 },
            highlight: Rgba { r: 26, g: 235, b: 255, a: 255 },
            highlight_text: Rgba { r: 0, g: 0, b: 0, a: 255 },
            gray_text: crate::syslook::GRAY_TEXT,
        });
        hc.high_contrast = true;
        for (theme, look) in [(crate::gfx::promo_kit::theme(true), false), (crate::gfx::promo_kit::theme(false), false), (hc, true)] {
            let view = c.CreateContainerVisual().unwrap();
            view.SetSize(Vector2 { X: 200.0, Y: 120.0 }).unwrap();
            let mut e = Edges::new(&stage.gfx, &theme, &view, (200.0, 120.0, size::SCROLL_FADE), 1.5, (false, true)).expect("the edges");
            let (off, on) = (Edges::level(look, false), Edges::level(look, true));
            assert_eq!(e.probe(), ([off, on], 3), "{look}: three slices, the bottom band on");
            e.jump((true, false), theme.high_contrast);
            assert_eq!(e.probe(), ([on, off], 3));
            // The menus' set: a fade (the property says where it ends
            // only once it is over), high contrast at once.
            e.set(c, (false, false), theme.high_contrast);
            if look {
                assert_eq!(e.probe(), ([off, off], 3));
            }
            e.jump((false, false), theme.high_contrast);
            assert_eq!(e.probe(), ([off, off], 3), "{look}: both off: the rows as they are");
        }
    }

    #[test]
    fn a_level_has_one_emphasis() {
        let (a, b) = (Target::Row(1), Target::Row(4));
        // The pointer alone: its hover.
        assert_eq!(glows(Some(a), None, false, None, None), Glows { hover: Some(a), mark: None });
        // An arrow key: the cursor's fill, and the pointer's goes (it is
        // still over its row).
        assert_eq!(glows(Some(a), Some(b), true, None, None), Glows { hover: None, mark: Some((b, Tone::Cursor)) });
        // The pointer moves again: the cursor goes.
        assert_eq!(glows(Some(a), Some(a), false, None, None), Glows { hover: Some(a), mark: None });
        // A press: the pressed fill alone on that row (not laid over the
        // hover: one fill at its own strength).
        assert_eq!(glows(Some(a), Some(a), false, Some(a), None), Glows { hover: None, mark: Some((a, Tone::Pressed)) });
        // A submenu up: its row keeps the pressed fill while the pointer
        // is in the submenu (no pointer here), or on another row here.
        assert_eq!(glows(None, Some(a), false, None, Some(1)), Glows { hover: None, mark: Some((a, Tone::Open)) });
        assert_eq!(glows(Some(b), Some(b), false, None, Some(1)), Glows { hover: Some(b), mark: Some((a, Tone::Open)) });
        // A press wins over the open row, that over the cursor.
        assert_eq!(glows(Some(b), Some(b), false, Some(b), Some(1)).mark, Some((b, Tone::Pressed)));
        assert_eq!(glows(None, Some(b), true, None, Some(1)).mark, Some((a, Tone::Open)));
        // A failure's button is raised: no ink is laid on its row.
        assert_eq!(glows(Some(Target::Button(2)), None, false, Some(Target::Button(2)), None), Glows::default());
        // The fills' states (theme::fill_of): open is the pressed fill.
        assert_eq!((Tone::Pressed.state(), Tone::Open.state(), Tone::Cursor.state()), (State::Pressed, State::Open, State::Cursor));
    }

    #[test]
    fn the_pointer_fades_and_the_keyboard_does_not() {
        use crate::motion::{EASE_IN, EASE_OUT, FADE_IN_MS, FADE_OUT_MS};
        // The pointer: 90 on, 120 off.
        assert_eq!(hover_on(false), Go::Fade(FADE_IN_MS, EASE_OUT));
        assert_eq!(hover_off(false, false, false), Go::Fade(FADE_OUT_MS, EASE_IN));
        // The keyboard took over, or a press lies there now: at once.
        assert_eq!(hover_off(true, false, false), Go::Now);
        assert_eq!(hover_off(false, true, false), Go::Now);
        // The cursor leaves no trail; a press let go eases back over 90;
        // an open row goes over 120 with its submenu.
        assert_eq!(mark_off(Tone::Cursor, false), Go::Now);
        assert_eq!(mark_off(Tone::Pressed, false), Go::Fade(FADE_IN_MS, EASE_OUT));
        assert_eq!(mark_off(Tone::Open, false), Go::Fade(FADE_OUT_MS, EASE_IN));
        // High contrast: nothing fades (12.3).
        assert_eq!((hover_on(true), hover_off(false, false, true)), (Go::Now, Go::Now));
        for t in [Tone::Cursor, Tone::Pressed, Tone::Open] {
            assert_eq!(mark_off(t, true), Go::Now);
        }
    }

    #[test]
    fn fills_lie_on_the_vessel_line_and_days_are_circles() {
        let mut week = Item::grid((1..=7).map(|d| d.to_string()).collect(), None);
        week.cell_ids = (1..=7).collect();
        let nav = Item::month("November", super::super::MonthNav { prev: 1, next: 2, ..Default::default() });
        let event = Item::event(9, super::super::EventRow { time: "10:00".into(), title: "x".into(), ..Default::default() });
        let items = vec![Item::new(1, "a"), nav, week, event];
        let rows = vec![(4.0, 28.0), (32.0, 32.0), (64.0, 32.0), (96.0, 48.0)];
        let w = 276.0;
        // A row: 4 in from each side, its own height, corners 8.
        assert_eq!(target_box(Target::Row(0), &items, &rows, w, false, 8.0), Some((4.0, 4.0, 268.0, 28.0, 8.0)));
        // An agenda row: its card, 2 inside the row's top and bottom.
        assert_eq!(target_box(Target::Row(3), &items, &rows, w, false, 8.0), Some((4.0, 98.0, 268.0, 44.0, 8.0)));
        // A day: a 28 circle in the middle of its 36 column.
        let (x, y, cw, ch, r) = target_box(Target::Cell(2, 0), &items, &rows, w, false, 8.0).unwrap();
        assert_eq!((cw, ch, r), (28.0, 28.0, 14.0));
        assert_eq!((x, y), (12.0 + 18.0 - 14.0, 64.0 + 2.0));
        // Right to left the first day is on the right.
        let (xr, ..) = target_box(Target::Cell(2, 0), &items, &rows, w, true, 8.0).unwrap();
        assert_eq!(xr, w - 12.0 - 18.0 - 14.0);
        assert_eq!(target_box(Target::Cell(2, 9), &items, &rows, w, false, 8.0), None);
        // A month header's buttons: round, 28, on the last two columns.
        let (bx, by, bw, bh, br) = target_box(Target::Nav(1, NavPart::Next), &items, &rows, w, false, 8.0).unwrap();
        assert_eq!((bw, bh, br, by), (28.0, 28.0, 14.0, 34.0));
        assert_eq!(bx, 12.0 + 36.0 * 6.0 + 4.0);
        // Its title: a row's corners.
        assert_eq!(target_box(Target::Nav(1, NavPart::Title), &items, &rows, w, false, 8.0).map(|b| b.4), Some(8.0));
    }

    #[test]
    fn the_clock_finds_its_day_period() {
        // Before the digits (Korean), after them, with dots, or none.
        assert_eq!(period_runs("오전 9:41:00"), vec![(0, 2)]);
        assert_eq!(period_runs("9:41:00 AM"), vec![(8, 2)]);
        assert_eq!(period_runs("9:41:00 a.m."), vec![(8, 4)]);
        assert_eq!(period_runs("21:41:00"), Vec::<(u32, u32)>::new());
        // A separator dot between digits is not one ("9.41.00").
        assert_eq!(period_runs("9.41.00"), Vec::<(u32, u32)>::new());
    }
}
