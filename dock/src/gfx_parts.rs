//! Harmony v2.1's shared drawing (design system 2.4, 2.5, 2.6, 6.4, 11):
//! the parts every screen draws the same way: text in a ramp style (its
//! optical-size instance and its tracking), the focus ring, a well, a
//! knob, the fade at a scroll edge, the progress ring and the quiet states.
//! One of each: the screens move to them with their own batches and drop
//! their copies.
//!
//! Everything draws in `dc`'s own units; `k` is how many of them a DIP is
//! (1 on a canvas in DIPs, the display's scale on one in pixels).

use std::mem::ManuallyDrop;

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D1_GRADIENT_STOP, D2D_RECT_F, D2D_SIZE_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, ID2D1Geometry, ID2D1RenderTarget, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_LINE_JOIN_ROUND,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, IDWriteTextLayout1, DWRITE_FONT_WEIGHT, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE};
use windows_numerics::{Matrix3x2, Vector2};

use super::Gfx;
use crate::glyphs::Glyph;
use crate::theme::metrics::size;
use crate::theme::ramp::TextStyle;
use crate::theme::{Level, Rgba, Role, State, Theme};

fn col(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

fn grown(r: D2D_RECT_F, by: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: r.left - by, top: r.top - by, right: r.right + by, bottom: r.bottom + by }
}

fn rounded(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    let cap = ((r.right - r.left).min(r.bottom - r.top) / 2.0).max(0.0);
    let rad = radius.clamp(0.0, cap);
    D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }
}

// ---- text ----------------------------------------------------------------------

/// Pure: the runs of `text` that take tracking, as UTF-16 (start, length):
/// everything but Hangul (negative tracking smears it, and its own font
/// spaces it already).
pub fn tracked_runs(text: &str) -> Vec<(u32, u32)> {
    let hangul = |c: char| super::HANGUL.iter().any(|&(a, b)| (a..=b).contains(&(c as u32)));
    let mut out: Vec<(u32, u32)> = Vec::new();
    let (mut at, mut open): (u32, Option<u32>) = (0, None);
    for c in text.chars() {
        let n = c.len_utf16() as u32;
        match (hangul(c), open) {
            (false, None) => open = Some(at),
            (true, Some(start)) => {
                out.push((start, at - start));
                open = None;
            }
            _ => {}
        }
        at += n;
    }
    if let Some(start) = open {
        out.push((start, at - start));
    }
    out
}

impl Gfx {
    /// A text layout in ramp style `style` at `scale` (px per DIP): drawn
    /// with the style's optical-size instance when `family` is the UI
    /// font's (fonts::face_for), its tracking (fonts::tracking: per font) on
    /// Latin and digits. Returns it with its width and height in px.
    pub fn text_style(&self, text: &str, family: &str, style: TextStyle, scale: f32) -> Result<(IDWriteTextLayout, f32, f32)> {
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().collect();
            let mapped = crate::fonts::map(family);
            let face = crate::fonts::face_for(&mapped, style.size);
            let fam = windows::core::HSTRING::from(&*face);
            let size_px = style.size * scale * crate::fonts::scale();
            let format = super::ui_format(&self.dwrite, &fam, DWRITE_FONT_WEIGHT(style.weight as i32), size_px)?;
            let layout = self.dwrite.CreateTextLayout(&wide, &format, 4000.0, 400.0)?;
            // Per font: Pretendard's own table, else the ramp's (Segoe's).
            let tracking = crate::fonts::tracking(&face, &style);
            if tracking != 0.0 {
                if let Ok(l1) = layout.cast::<IDWriteTextLayout1>() {
                    for (start, len) in tracked_runs(text) {
                        let _ = l1.SetCharacterSpacing(0.0, tracking * size_px, 0.0, DWRITE_TEXT_RANGE { startPosition: start, length: len });
                    }
                }
            }
            let rtl = super::read_rtl(&layout);
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m)?;
            let w = m.widthIncludingTrailingWhitespace.ceil();
            if rtl {
                let _ = layout.SetMaxWidth(w.max(1.0));
            }
            Ok((layout, w, m.height.ceil()))
        }
    }
}

// ---- the focus ring (6.4) --------------------------------------------------------

/// Pure: the focus ring's stroke around `r` (corners `radius`): its
/// centerline's rectangle and corner. FOCUS_GAP outside the element,
/// FOCUS_W wide, concentric with its corners (a capsule stays a capsule).
pub fn focus_ring_rect(r: D2D_RECT_F, radius: f32, k: f32) -> (D2D_RECT_F, f32) {
    let o = (size::FOCUS_GAP + size::FOCUS_W / 2.0) * k;
    (grown(r, o), radius.max(0.0) + o)
}

/// The ring's color: the ink (black on light, white on dark) at .80; the
/// accent when one was chosen; Highlight in high contrast.
pub fn focus_color(theme: &Theme) -> Rgba {
    if let Some(h) = theme.hc {
        return h.highlight;
    }
    if let Some(a) = theme.accent_rgba {
        return a;
    }
    let v = if theme.is_light { 0 } else { 255 };
    Rgba { r: v, g: v, b: v, a: 204 }
}

/// The keyboard's focus ring around `r` (corners `radius`). Shown only
/// when the keyboard moved the focus (Tab, a surface opened by key); at
/// once, never faded in. Not for a menu's rows (their cursor is the
/// selected fill: there is no room for a ring). On whole device pixels
/// (snap_ring): its outer edge on a pixel's edge and its width whole
/// pixels, so it is one crisp line at every scale (states-issues 1: at
/// 150 % the 3 px ring of a slider on a half pixel spread over 4 grey
/// rows and read as a thick double border).
pub unsafe fn focus_ring(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, theme: &Theme, k: f32) -> Result<()> {
    let (rr, rad) = focus_ring_rect(r, radius, k);
    let (rr, rad, w) = on_device_pixels(dc, rr, rad, size::FOCUS_W * k);
    let b = dc.CreateSolidColorBrush(&col(focus_color(theme)), None)?;
    dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rr, radiusX: rad, radiusY: rad }, &b, w, None);
    Ok(())
}

/// A stroke on `dc` (its centerline `rr`, corner `rad`, `w` wide, in the
/// target's units) moved onto whole device pixels by snap_ring, through
/// the target's transform (a scale, mirrored or not, and a move) and DPI.
/// Any other transform (a turn): as it is.
pub unsafe fn on_device_pixels(dc: &ID2D1DeviceContext, rr: D2D_RECT_F, rad: f32, w: f32) -> (D2D_RECT_F, f32, f32) {
    let mut t = Matrix3x2::default();
    dc.GetTransform(&mut t);
    let (mut dpi_x, mut dpi_y) = (96.0f32, 96.0f32);
    dc.GetDpi(&mut dpi_x, &mut dpi_y);
    let d = if dpi_x > 0.0 { dpi_x / 96.0 } else { 1.0 };
    if t.M12 != 0.0 || t.M21 != 0.0 || t.M11 == 0.0 || t.M22 == 0.0 {
        return (rr, rad, w);
    }
    let (sx, sy) = (t.M11 * d, t.M22 * d);
    // To device pixels (a mirrored axis swaps its edges).
    let (x0, x1) = ((rr.left * t.M11 + t.M31) * d, (rr.right * t.M11 + t.M31) * d);
    let (y0, y1) = ((rr.top * t.M22 + t.M32) * d, (rr.bottom * t.M22 + t.M32) * d);
    let px = D2D_RECT_F { left: x0.min(x1), top: y0.min(y1), right: x0.max(x1), bottom: y0.max(y1) };
    let (s, wp) = snap_ring(px, w * sx.abs());
    // Back to the target's units.
    let back_x = |x: f32| (x / d - t.M31) / t.M11;
    let back_y = |y: f32| (y / d - t.M32) / t.M22;
    let (l, r) = (back_x(s.left), back_x(s.right));
    let (tp, b) = (back_y(s.top), back_y(s.bottom));
    let out = D2D_RECT_F { left: l.min(r), top: tp.min(b), right: l.max(r), bottom: tp.max(b) };
    // The corner moves with the edge (concentric still).
    let grow = (out.right - out.left - (rr.right - rr.left)) / 2.0;
    let _ = sy;
    (out, (rad + grow).max(0.0), wp / sx.abs())
}

/// Pure: a ring's stroke (its centerline `c` and width `w`, device
/// pixels) on whole pixels: the width rounded (at least 1), the outer
/// edge rounded to a pixel's edge, the centerline half the width inside
/// it. Returns the centerline and the width.
pub fn snap_ring(c: D2D_RECT_F, w: f32) -> (D2D_RECT_F, f32) {
    let wp = w.round().max(1.0);
    let half = wp / 2.0;
    let outer = D2D_RECT_F { left: (c.left - w / 2.0).round(), top: (c.top - w / 2.0).round(), right: (c.right + w / 2.0).round(), bottom: (c.bottom + w / 2.0).round() };
    (D2D_RECT_F { left: outer.left + half, top: outer.top + half, right: outer.right - half, bottom: outer.bottom - half }, wp)
}

/// Pure: the box of a ring around an app icon `side` DIP wide (6.4: the
/// keyboard's focus on a squircle): FOCUS_GAP out of the icon, FOCUS_W
/// wide; (how far it reaches past the icon on each side, its stroke).
pub fn icon_ring() -> (f32, f32) {
    (size::FOCUS_GAP + size::FOCUS_W, size::FOCUS_W)
}

impl Gfx {
    /// A squircle ring filling a `side` px square: the dock's superellipse
    /// (an app icon's shape) stroked `stroke` px wide, all of the stroke
    /// inside the square, in `color`. The focus ring of an icon (drawn
    /// icon_ring() larger than it), high contrast's pressed outline (drawn
    /// its size).
    pub fn squircle_ring(&self, side: u32, stroke: f32, color: Rgba) -> Result<windows::UI::Composition::CompositionDrawingSurface> {
        let factory = self.d2d_factory().clone();
        self.draw(side, side, move |dc, ox, oy| unsafe { paint_squircle_ring(dc, &factory, ox, oy, side as f32, stroke, color) })
    }
}

/// Squircle_ring's paint at (`ox`, `oy`) px.
pub unsafe fn paint_squircle_ring(dc: &ID2D1DeviceContext, factory: &windows::Win32::Graphics::Direct2D::ID2D1Factory1, ox: f32, oy: f32, side: f32, stroke: f32, color: Rgba) -> Result<()> {
    let t = stroke.max(0.0).min(side / 2.0);
    let geom = super::squircle_geometry(factory, ox + t / 2.0, oy + t / 2.0, (side - t).max(0.0))?;
    let b = dc.CreateSolidColorBrush(&col(color), None)?;
    dc.DrawGeometry(&geom, &b, t, None);
    Ok(())
}

// ---- wells and knobs (2.4, 2.6) -----------------------------------------------------

/// The least size (DIP) a round well (a dot) has its shade at.
pub const WELL_SHADE_MIN: f32 = 8.0;

/// A well: something sunk into its surface (a slider's or progress bar's
/// track, a field, a switch that is off, a segmented bar's ground): the
/// track fill and, along its top inner edge only, the shade its upper wall
/// casts (the one light is above). This inner shade is what tells sunk
/// from raised; nothing raised has one. High contrast: no fill, a 1 DIP
/// outline.
pub unsafe fn well(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, theme: &Theme, k: f32) -> Result<()> {
    let rr = rounded(r, radius);
    if let Some((c, w)) = theme.outline_of(Role::Well, State::Rest) {
        let b = dc.CreateSolidColorBrush(&col(c), None)?;
        let inset = w * k / 2.0;
        dc.DrawRoundedRectangle(&rounded(grown(r, -inset), (rr.radiusX - inset).max(0.0)), &b, w * k, None);
        return Ok(());
    }
    dc.FillRoundedRectangle(&rr, &dc.CreateSolidColorBrush(&col(theme.fill_of(Role::Well, State::Rest)), None)?);
    // The shade: black .10 (light) / .34 (dark), 1 DIP down, a little soft;
    // never more than a quarter of a thin track. None on a dot (as wide
    // as tall) smaller than WELL_SHADE_MIN: a page dot's shade ate its top
    // and it read notched on dark (states-issues 10).
    let a = if theme.is_light { 0.10 } else { 0.34 };
    let d = k.min((r.bottom - r.top) / 4.0);
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let dot = (w - h).abs() < 0.5 * k && h < WELL_SHADE_MIN * k;
    if d <= 0.0 || dot {
        return Ok(());
    }
    let factory = dc.GetFactory()?;
    let mask = factory.CreateRoundedRectangleGeometry(&rr)?;
    let params = D2D1_LAYER_PARAMETERS1 {
        contentBounds: r,
        geometricMask: ManuallyDrop::new(Some(mask.cast()?)),
        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        maskTransform: Matrix3x2::identity(),
        opacity: 1.0,
        opacityBrush: ManuallyDrop::new(None),
        layerOptions: D2D1_LAYER_OPTIONS1_NONE,
    };
    dc.PushLayer(&params, None);
    let _ = ManuallyDrop::into_inner(params.geometricMask);
    // The well's own shape moved down by the shade's reach and widened
    // past its sides (concentric corners): what its stroke covers inside
    // the well is the upper wall's shadow only, never the side walls
    // (wide and faint, then narrow and firmer: a soft edge without a blur).
    let out = d * 2.0;
    let low = rounded(
        D2D_RECT_F { left: r.left - out, right: r.right + out, top: r.top + d, bottom: r.bottom + d },
        rr.radiusX + out,
    );
    let drawn = (|| -> Result<()> {
        for (w, share) in [(d * 2.0, 0.55), (d, 0.45)] {
            let b = dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: a * share }, None)?;
            dc.DrawRoundedRectangle(&low, &b, w, None);
        }
        Ok(())
    })();
    dc.PopLayer();
    drawn
}

/// A knob (a slider's, a switch's, a segmented bar's thumb as a circle):
/// a raised thing, so elevation E1 falling down, the contact shadow all
/// round (a white knob on a white card keeps its outline) and its fill.
/// `on`: it sits in a well that is on (drawn in the "on" ink). High
/// contrast: the fill alone.
pub unsafe fn knob(dc: &ID2D1DeviceContext, cx: f32, cy: f32, radius: f32, theme: &Theme, on: bool, k: f32) -> Result<()> {
    // Glass: a capsule (knob_half_w) on one soft shadow; Flat: a flat disc,
    // no shadow (its contact ring keeps a white one on a white card).
    let hw = knob_half_w(radius, theme);
    let r = D2D_RECT_F { left: cx - hw, top: cy - radius, right: cx + hw, bottom: cy + radius };
    if theme.glass() {
        let layers: Vec<_> = theme.knob_shadow().into_iter().map(|(x, y, blur, c)| (x * k, y * k, blur * k, c)).collect();
        super::soft_shadows(dc, r, radius, &layers)?;
    }
    let contact = theme.contact();
    if contact.a > 0 {
        let w = crate::theme::CONTACT_W * k;
        let b = dc.CreateSolidColorBrush(&col(contact), None)?;
        dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: grown(r, w / 2.0), radiusX: radius + w / 2.0, radiusY: radius + w / 2.0 }, &b, w, None);
    }
    let fill = theme.fill_of(Role::Knob, if on { State::Selected } else { State::Rest });
    dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &dc.CreateSolidColorBrush(&col(fill), None)?);
    Ok(())
}

/// Pure: half a knob's width for its `radius` (half its height): Glass's
/// is a capsule a third wider than tall, Flat's (and high contrast's) a
/// disc.
pub fn knob_half_w(radius: f32, theme: &Theme) -> f32 {
    if theme.glass() {
        radius * KNOB_GLASS_W
    } else {
        radius
    }
}

/// Glass's knob: its width over its height.
pub const KNOB_GLASS_W: f32 = 1.35;
pub const SWITCH_W: f32 = 40.0;
pub const SWITCH_H: f32 = 20.0;

// ---- the caret --------------------------------------------------------------------------

/// A text field's caret: its width (DIP).
pub const CARET_W: f32 = crate::theme::metrics::space::S1;

/// A text field's caret from `top` to `bottom` (its line's box), its left
/// edge at `x`: CARET_W wide with round ends, in the primary text color
/// (WindowText in high contrast). One caret for every field drawn in D2D
/// (its blink is the field's: the system's caret time).
pub unsafe fn caret(dc: &ID2D1DeviceContext, x: f32, top: f32, bottom: f32, theme: &Theme, k: f32) -> Result<()> {
    let w = CARET_W * k;
    let b = dc.CreateSolidColorBrush(&col(theme.text_of(Level::Primary, State::Rest)), None)?;
    dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: x, top, right: x + w, bottom }, radiusX: w / 2.0, radiusY: w / 2.0 }, &b);
    Ok(())
}

// ---- the light on a drawn plate (2.2, 2.6) -------------------------------------------

/// The contact shadow of a plate `r` (corners `radius`) drawn in D2D (a
/// tooltip, a label: surfaces that are one picture): a ring CONTACT_W wide
/// just outside its edge, no blur, no direction, in `color` (theme::
/// contact; clear: nothing). Drawn before the plate's fill.
pub unsafe fn contact(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, color: Rgba, k: f32) -> Result<()> {
    if color.a == 0 {
        return Ok(());
    }
    let w = crate::theme::CONTACT_W * k;
    let b = dc.CreateSolidColorBrush(&col(color), None)?;
    dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: grown(r, w / 2.0), radiusX: radius + w / 2.0, radiusY: radius + w / 2.0 }, &b, w, None);
    Ok(())
}

/// Pure: the rim's stroke on a plate `w` x `h` with corners `radius`, `px`
/// thick (one device pixel in the plate's units): (inset, size, corner).
/// Half the stroke in from the edge, concentric with the plate's corners
/// (spike S7; popup::chrome_at's and the dock label's are the same).
pub fn rim_stroke(w: f32, h: f32, radius: f32, px: f32) -> (f32, (f32, f32), f32) {
    (px / 2.0, ((w - px).max(0.0), (h - px).max(0.0)), (radius - px / 2.0).max(0.0))
}

/// The rim of a plate `r` drawn in D2D: light along its inner edge, `px`
/// thick (one device pixel), `stops` (theme::rim) at theme::RIM_STOPS from
/// its top to its bottom. Drawn over the plate's fill.
pub unsafe fn rim(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, stops: [Rgba; 3], px: f32) -> Result<()> {
    let list: Vec<D2D1_GRADIENT_STOP> = crate::theme::RIM_STOPS.iter().zip(stops).map(|(at, c)| D2D1_GRADIENT_STOP { position: *at, color: col(c) }).collect();
    let rt: ID2D1RenderTarget = dc.cast()?;
    let coll = rt.CreateGradientStopCollection(&list, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
    let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: r.left, Y: r.top }, endPoint: Vector2 { X: r.left, Y: r.bottom } };
    let brush = rt.CreateLinearGradientBrush(&props, None, &coll)?;
    let (inset, (w, h), rad) = rim_stroke(r.right - r.left, r.bottom - r.top, radius, px);
    let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: r.left + inset, top: r.top + inset, right: r.left + inset + w, bottom: r.top + inset + h }, radiusX: rad, radiusY: rad };
    dc.DrawRoundedRectangle(&rr, &brush, px, None);
    Ok(())
}

/// A plate `r` (corners `radius`) drawn in D2D, filled: Flat v2's fall
/// from top to bottom down `r` (`fall`: Theme::flat_fill), else `color`.
pub unsafe fn plate_fill(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, fall: Option<(Rgba, Rgba)>, color: Rgba) -> Result<()> {
    let rr = rounded(r, radius);
    match fall {
        Some((top, bottom)) => {
            let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: col(top) }, D2D1_GRADIENT_STOP { position: 1.0, color: col(bottom) }];
            let rt: ID2D1RenderTarget = dc.cast()?;
            let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: r.left, Y: r.top }, endPoint: Vector2 { X: r.left, Y: r.bottom } };
            dc.FillRoundedRectangle(&rr, &rt.CreateLinearGradientBrush(&props, None, &coll)?);
        }
        None => dc.FillRoundedRectangle(&rr, &dc.CreateSolidColorBrush(&col(color), None)?),
    }
    Ok(())
}

thread_local! {
    /// The grain's tile as a D2D bitmap, and the device it was made on
    /// (grain: made again on another device, a device lost and found).
    static GRAIN: std::cell::RefCell<Option<(windows::Win32::Graphics::Direct2D::ID2D1Device, windows::Win32::Graphics::Direct2D::ID2D1Bitmap1)>> = const { std::cell::RefCell::new(None) };
}

/// Flat v2's grain over a plate `r` (corners `radius`) drawn in D2D (a
/// picture that is one plate, Launchpad's folder panel): glass's noise
/// tile (glass::noise_tile, what the Composition plates lay over their
/// tint), one of its pixels a device pixel, at `opacity` (Theme::
/// flat_grain; 0: nothing: a Flat the PC forces, a glass, high contrast).
/// The tile is made once per D2D device and kept; another device (lost
/// and found) makes it again.
pub unsafe fn grain(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, opacity: f32) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
    use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_BRUSH_PROPERTIES, D2D1_EXTEND_MODE_WRAP, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR};
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    if opacity <= 0.0 {
        return Ok(());
    }
    let device = dc.GetDevice()?;
    let kept = GRAIN.with(|g| g.borrow().as_ref().filter(|(d, _)| *d == device).map(|(_, b)| b.clone()));
    let bmp = match kept {
        Some(b) => b,
        None => {
            let side = crate::glass::TILE;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                ..Default::default()
            };
            let tile = crate::glass::noise_tile();
            let b = dc.CreateBitmap(D2D_SIZE_U { width: side, height: side }, Some(tile.as_ptr() as _), side * 4, &props)?;
            GRAIN.with(|g| *g.borrow_mut() = Some((device, b.clone())));
            b
        }
    };
    // One tile pixel a device pixel, whatever the canvas's transform.
    let mut t = Matrix3x2::default();
    dc.GetTransform(&mut t);
    let k = 1.0 / t.M11.abs().max(0.01);
    let bp = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_WRAP, extendModeY: D2D1_EXTEND_MODE_WRAP, interpolationMode: D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR };
    let brush_props = D2D1_BRUSH_PROPERTIES { opacity, transform: Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: 0.0, M32: 0.0 } };
    let brush = dc.CreateBitmapBrush(&bmp, Some(&bp), Some(&brush_props))?;
    dc.FillRoundedRectangle(&rounded(r, radius), &brush);
    Ok(())
}

/// Flat v2's card under its fill (Theme::card_fx; nothing elsewhere): a
/// drop just under the card `r` (CSS `0 .5px 1px`: black at the card's
/// `drop`), as two fills half a DIP down, the outer a quarter wider (the
/// blur's spread). `s`: the card's state (none on the "on" fill), `alpha`:
/// its own fade. Every raised thing drawn in D2D goes through this and
/// card_over (rounded ones here, any shape by card_under_shape).
pub unsafe fn card_under(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, theme: &Theme, s: State, k: f32, alpha: f32) -> Result<()> {
    if theme.card_fx(s).is_none() {
        return Ok(());
    }
    let shape = dc.GetFactory()?.CreateRoundedRectangleGeometry(&rounded(r, radius))?;
    card_under_shape(dc, &shape.cast()?, theme, s, k, alpha)
}

/// card_under for a raised thing of any `shape` (the icons' squircle).
pub unsafe fn card_under_shape(dc: &ID2D1DeviceContext, shape: &ID2D1Geometry, theme: &Theme, s: State, k: f32, alpha: f32) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::D2D1_COMBINE_MODE_EXCLUDE;
    let Some(fx) = theme.card_fx(s) else { return Ok(()) };
    let a = fx.drop.a as f32 / 255.0 * alpha;
    if a <= 0.0 {
        return Ok(());
    }
    let f = dc.GetFactory()?;
    let b = shape.GetBounds(None)?;
    let (w, h) = (b.right - b.left, b.bottom - b.top);
    if w <= 0.0 || h <= 0.0 {
        return Ok(());
    }
    let black = |a: f32| D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a };
    // A quarter DIP wider on every side (about its middle), half a DIP down.
    let (cx, cy) = ((b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0);
    let (sx, sy) = ((w + 0.5 * k) / w, (h + 0.5 * k) / h);
    let outer = f.CreateTransformedGeometry(shape, &Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: cx - sx * cx, M32: cy - sy * cy + 0.5 * k })?;
    let inner = f.CreateTransformedGeometry(shape, &Matrix3x2::translation(0.0, 0.5 * k))?;
    // Only what lies outside the card (its fill may be a visual of its
    // own under this picture: Launchpad's Done).
    for (g, share) in [(outer, 0.45), (inner, 0.55)] {
        let out = f.CreatePathGeometry()?;
        let sink = out.Open()?;
        g.CombineWithGeometry(shape, D2D1_COMBINE_MODE_EXCLUDE, None, 0.25, &sink)?;
        sink.Close()?;
        dc.FillGeometry(&out, &dc.CreateSolidColorBrush(&black(a * share), None)?, None);
    }
    Ok(())
}

/// Pure: where a raised thing that fills its own picture `r` (a Launchpad
/// tile drawn as a surface the size of its cell) lies so Flat v2's card
/// light fits in the picture (`k`: units per DIP): a quarter DIP in at the
/// sides, three quarters at the bottom (the drop: half a DIP down, a
/// quarter wider); `r` itself where there is no light (card_fx None).
pub fn card_room(theme: &Theme, s: State, r: D2D_RECT_F, k: f32) -> D2D_RECT_F {
    match theme.card_fx(s) {
        Some(_) => D2D_RECT_F { left: r.left + 0.25 * k, top: r.top, right: r.right - 0.25 * k, bottom: r.bottom - 0.75 * k },
        None => r,
    }
}

/// Flat v2's card over its fill (before what lies on it): the light along
/// its top edge (CSS `inset 0 1px 0`: white at the card's `light`, one
/// DIP down the top, thinning along the corners), none pressed or open (a
/// card pushed in is not lit) and none on the "on" fill (Theme::card_fx).
/// Nothing elsewhere.
pub unsafe fn card_over(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, theme: &Theme, s: State, k: f32, alpha: f32) -> Result<()> {
    if theme.card_fx(s).is_none_or(|fx| fx.light.a == 0) {
        return Ok(());
    }
    let shape = dc.GetFactory()?.CreateRoundedRectangleGeometry(&rounded(r, radius))?;
    card_over_shape(dc, &shape.cast()?, theme, s, k, alpha)
}

/// card_over for a raised thing of any `shape`.
pub unsafe fn card_over_shape(dc: &ID2D1DeviceContext, shape: &ID2D1Geometry, theme: &Theme, s: State, k: f32, alpha: f32) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::D2D1_COMBINE_MODE_EXCLUDE;
    let Some(fx) = theme.card_fx(s) else { return Ok(()) };
    let a = fx.light.a as f32 / 255.0 * alpha;
    let b = shape.GetBounds(None)?;
    if a <= 0.0 || b.bottom - b.top <= k {
        return Ok(());
    }
    let f = dc.GetFactory()?;
    let below = f.CreateTransformedGeometry(shape, &Matrix3x2::translation(0.0, k))?;
    let band = f.CreatePathGeometry()?;
    let sink = band.Open()?;
    shape.CombineWithGeometry(&below, D2D1_COMBINE_MODE_EXCLUDE, None, 0.25, &sink)?;
    sink.Close()?;
    dc.FillGeometry(&band, &dc.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a }, None)?, None);
    Ok(())
}

/// A raised thing's fill `fill` on `r` (corners `radius`) with Flat v2's
/// card light round it (card_under, the fill, card_over): what draws a
/// card, a tile or a button in D2D.
#[allow(clippy::too_many_arguments)]
pub unsafe fn card(dc: &ID2D1DeviceContext, r: D2D_RECT_F, radius: f32, theme: &Theme, s: State, fill: Rgba, k: f32, alpha: f32) -> Result<()> {
    card_under(dc, r, radius, theme, s, k, alpha)?;
    let c = col(fill);
    if c.a * alpha > 0.0 {
        dc.FillRoundedRectangle(&rounded(r, radius), &dc.CreateSolidColorBrush(&D2D1_COLOR_F { a: c.a * alpha, ..c }, None)?);
    }
    card_over(dc, r, radius, theme, s, k, alpha)
}

// ---- the scroll edge (2.5) ----------------------------------------------------------

/// Pure: how opaque scrolled content is `y` below the top of a scroll area
/// `h` tall (both in DIP): 0 at an edge that fades, 1 from SCROLL_FADE in.
/// `top`: the content is scrolled (something is hidden above); `bottom`:
/// a fixed foot follows and content runs under it.
pub fn scroll_alpha(y: f32, h: f32, top: bool, bottom: bool) -> f32 {
    let band = size::SCROLL_FADE;
    let mut a = 1.0f32;
    if top {
        a = a.min((y / band).clamp(0.0, 1.0));
    }
    if bottom {
        a = a.min(((h - y) / band).clamp(0.0, 1.0));
    }
    a
}

/// Draws scrolled content (`paint`) inside `r`, cut at its edges; at an
/// edge that fades (`top`, `bottom`: see scroll_alpha) its alpha goes from
/// 0 to 1 over SCROLL_FADE. An alpha mask on the content, not a band of
/// color over it: on a see-through plate the tint is not laid on twice.
/// No line is drawn: the band does that job. High contrast: no band, a
/// 1 DIP line in the text color at that edge.
pub unsafe fn scroll_fade(dc: &ID2D1DeviceContext, r: D2D_RECT_F, top: bool, bottom: bool, theme: &Theme, k: f32, paint: impl FnOnce() -> Result<()>) -> Result<()> {
    let (h, band) = (r.bottom - r.top, size::SCROLL_FADE * k);
    if theme.high_contrast || !(top || bottom) || h <= band * 2.0 {
        dc.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_ALIASED);
        let out = paint();
        dc.PopAxisAlignedClip();
        if let Some(hc) = theme.hc {
            let b = dc.CreateSolidColorBrush(&col(hc.text), None)?;
            if top {
                dc.FillRectangle(&D2D_RECT_F { bottom: r.top + k, ..r }, &b);
            }
            if bottom {
                dc.FillRectangle(&D2D_RECT_F { top: r.bottom - k, ..r }, &b);
            }
        }
        return out;
    }
    let at = |a: f32| D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a };
    let stops = [
        D2D1_GRADIENT_STOP { position: 0.0, color: at(if top { 0.0 } else { 1.0 }) },
        D2D1_GRADIENT_STOP { position: band / h, color: at(1.0) },
        D2D1_GRADIENT_STOP { position: 1.0 - band / h, color: at(1.0) },
        D2D1_GRADIENT_STOP { position: 1.0, color: at(if bottom { 0.0 } else { 1.0 }) },
    ];
    let rt: ID2D1RenderTarget = dc.cast()?;
    let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
    let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: r.left, Y: r.top }, endPoint: Vector2 { X: r.left, Y: r.bottom } };
    let brush = rt.CreateLinearGradientBrush(&props, None, &coll)?;
    let params = D2D1_LAYER_PARAMETERS1 {
        contentBounds: r,
        geometricMask: ManuallyDrop::new(None),
        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        maskTransform: Matrix3x2::identity(),
        opacity: 1.0,
        opacityBrush: ManuallyDrop::new(Some(brush.cast()?)),
        layerOptions: D2D1_LAYER_OPTIONS1_NONE,
    };
    dc.PushLayer(&params, None);
    let _ = ManuallyDrop::into_inner(params.opacityBrush);
    let out = paint();
    dc.PopLayer();
    out
}

// ---- quiet states (11) ---------------------------------------------------------------

/// One turn of the progress ring (ms).
pub const SPINNER_TURN_MS: f32 = 900.0;

/// Pure: the ring's turn (0..1) `elapsed_ms` in. With reduced motion it
/// does not turn: a still arc.
pub fn spinner_phase(elapsed_ms: f32, full_motion: bool) -> f32 {
    if !full_motion || !elapsed_ms.is_finite() {
        return 0.0;
    }
    (elapsed_ms / SPINNER_TURN_MS).rem_euclid(1.0)
}

/// The progress ring at (`cx`, `cy`): 16 DIP across, a 270 degree arc with
/// round ends in the secondary text color, turned by `phase` (0..1). The
/// one size; shown only once something has taken 300 ms (motion::
/// TIP_DELAY_MS). When progress is known, a bar is drawn instead.
pub unsafe fn spinner(dc: &ID2D1DeviceContext, cx: f32, cy: f32, phase: f32, theme: &Theme, k: f32) -> Result<()> {
    let stroke = size::SPINNER_STROKE * k;
    let r = (size::SPINNER * k - stroke) / 2.0;
    let factory = dc.GetFactory()?;
    let style = factory.CreateStrokeStyle(
        &D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, dashCap: D2D1_CAP_STYLE_ROUND, lineJoin: D2D1_LINE_JOIN_ROUND, miterLimit: 10.0, dashStyle: D2D1_DASH_STYLE_SOLID, dashOffset: 0.0 },
        None,
    )?;
    // The arc about (`x`, `y`) in `b`.
    let arc = |rt: &windows::Win32::Graphics::Direct2D::ID2D1RenderTarget, x: f32, y: f32, b: &windows::Win32::Graphics::Direct2D::ID2D1SolidColorBrush| -> Result<()> {
        let a0 = phase * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let a1 = a0 + std::f32::consts::PI * 1.5;
        let pt = |a: f32| Vector2 { X: x + r * a.cos(), Y: y + r * a.sin() };
        let geom = factory.CreatePathGeometry()?;
        let sink = geom.Open()?;
        sink.BeginFigure(pt(a0), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddArc(&D2D1_ARC_SEGMENT { point: pt(a1), size: D2D_SIZE_F { width: r, height: r }, rotationAngle: 0.0, sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE, arcSize: D2D1_ARC_SIZE_LARGE });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        rt.DrawGeometry(&geom, b, stroke, &style);
        Ok(())
    };
    let b = dc.CreateSolidColorBrush(&col(theme.text_of(Level::Secondary, State::Rest)), None)?;
    arc(dc, cx, cy, &b)
}

/// What a quiet block says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quiet {
    /// Nothing here: the area's own glyph in the tertiary color, one line
    /// in the secondary.
    Empty,
    /// Waiting: the ring alone (a line only once it has taken long).
    Loading,
    /// Something went wrong: the warning glyph and one line, both in the
    /// primary color. No color, no shake: lightness and the glyph say it.
    Error,
}

/// Pure: where a quiet block's parts sit in an area `h` tall (DIP), for a
/// text line `line` tall: (the glyph's center y, the text's top), the
/// block centered. The glyph's slot is 24, the gap 8.
pub fn quiet_rows(h: f32, line: f32) -> (f32, f32) {
    let (slot, gap) = (size::ICON_L, crate::theme::metrics::space::S3);
    let block = slot + if line > 0.0 { gap + line } else { 0.0 };
    let top = ((h - block) / 2.0).max(0.0);
    (top + slot / 2.0, top + slot + gap)
}

impl Gfx {
    /// A quiet state's block in the middle of `r` (11.1; areas at least
    /// QUIET_MIN_H tall: search results, a stack, a folder): a 24 DIP
    /// glyph, 8 below it one line of body text, centered. `glyph` is the
    /// area's own (a magnifier for search); Error draws the warning,
    /// Loading the ring (`phase`: spinner_phase). `text` empty: no line.
    /// A button, if any, is the caller's (12 below the line). High
    /// contrast: everything in the text color.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn quiet(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, kind: Quiet, glyph: Glyph, text: &str, theme: &Theme, k: f32, phase: f32) -> Result<()> {
        let (glyph_c, text_c) = match kind {
            Quiet::Empty => (theme.text_of(Level::Tertiary, State::Rest), theme.text_of(Level::Secondary, State::Rest)),
            Quiet::Loading => (theme.text_of(Level::Secondary, State::Rest), theme.text_of(Level::Secondary, State::Rest)),
            Quiet::Error => (theme.text_of(Level::Primary, State::Rest), theme.text_of(Level::Primary, State::Rest)),
        };
        let laid = if text.is_empty() { None } else { Some(self.text_style(text, &theme.font.family, crate::theme::ramp::BODY, k)?) };
        let line = laid.as_ref().map_or(0.0, |l| l.2);
        let (gy, ty) = quiet_rows((r.bottom - r.top) / k, line / k);
        let cx = (r.left + r.right) / 2.0;
        match kind {
            Quiet::Loading => spinner(dc, cx, r.top + gy * k, phase, theme, k)?,
            Quiet::Empty => crate::gfx::draw_glyph(dc, glyph, cx, r.top + gy * k, size::ICON_L * k, col(glyph_c))?,
            Quiet::Error => crate::gfx::draw_glyph(dc, Glyph::Warning, cx, r.top + gy * k, size::ICON_L * k, col(glyph_c))?,
        }
        if let Some((layout, w, _)) = laid {
            let b = dc.CreateSolidColorBrush(&col(text_c), None)?;
            crate::gfx::draw_text(dc, Vector2 { X: (cx - w / 2.0).round(), Y: (r.top + ty * k).round() }, &layout, &b, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
        Ok(())
    }
}

/// The tokens' sheet for the design audit (tests only).
#[cfg(test)]
#[path = "gfx_parts_harmony.rs"]
mod harmony;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_skips_hangul() {
        assert_eq!(tracked_runs("Dock"), vec![(0, 4)]);
        assert_eq!(tracked_runs("독"), Vec::<(u32, u32)>::new());
        // "NopBar 설정 13": the Latin run with its space, then the digits
        // (the space before them is not Hangul either).
        assert_eq!(tracked_runs("NopBar 설정 13"), vec![(0, 7), (9, 3)]);
        // UTF-16 positions: an emoji is two units.
        assert_eq!(tracked_runs("가😀a"), vec![(1, 3)]);
        assert_eq!(tracked_runs(""), Vec::<(u32, u32)>::new());
    }

    /// Premultiplied BGRA's green and alpha at (x, y) of a finished canvas.
    fn ga(px: &[u8], w: u32, x: u32, y: u32) -> (u8, u8) {
        let i = ((y * w + x) * 4) as usize;
        (px[i + 1], px[i + 3])
    }

    /// The review's P2 5 (2026-11-11): every raised thing drawn in D2D
    /// wears Flat v2's card light through card(): the drop just under it,
    /// the light along its top (gone pressed), neither on the "on" fill or
    /// a glass.
    #[test]
    fn a_raised_thing_wears_the_cards_light_through_card() {
        use crate::gfx::promo_kit::{rect, Canvas};
        use crate::syslook::SysLook;
        use crate::theme::Finish;
        let r = rect(10.0, 10.0, 110.0, 40.0);
        let draw = |theme: &Theme, s: State| {
            let cv = Canvas::at(120.0, 50.0, 1.0);
            unsafe {
                cv.dc.Clear(Some(&col(Rgba { r: 0x80, g: 0x80, b: 0x80, a: 255 })));
                card(&cv.dc, r, 8.0, theme, s, theme.fill_of(Role::Raised, s), 1.0, 1.0).unwrap();
            }
            cv.finish()
        };
        for light in [false, true] {
            let sys = SysLook { light, ..Default::default() };
            let flat = Theme::graphite().resolve_fx(&sys, true);
            let (px, w, _) = draw(&flat, State::Rest);
            let fill = ga(&px, w, 60, 25).0;
            // Flat's cards (since 2026-11-12) are plain paper: the top
            // edge the card's own grey, nothing under its bottom.
            assert_eq!(ga(&px, w, 60, 10).0, fill, "light {light}: the top edge");
            assert_eq!(ga(&px, w, 60, 40).0, 0x80, "light {light}: no drop");
            // On, or a glass: the fill alone (the row under it untouched).
            let mut raw = Theme::graphite();
            raw.finish = Finish::Glass;
            for (t, s) in [(&flat, State::Selected), (&raw.resolve_fx(&sys, true), State::Rest)] {
                let (q, _, _) = draw(t, s);
                assert_eq!(ga(&q, w, 60, 40).0, 0x80, "light {light} {s:?}: no drop");
            }
        }
        // card_room keeps the light inside a picture it fills.
        let flat = Theme::graphite().resolve_fx(&SysLook::default(), true);
        let room = card_room(&flat, State::Rest, rect(0.0, 0.0, 60.0, 60.0), 2.0);
        assert_eq!((room.left, room.top, room.right, room.bottom), (0.5, 0.0, 59.5, 58.5));
    }

    /// The review's P2 4 (2026-11-11): Flat v2's edge light is 1 DIP, not
    /// one device pixel: at 150 % the rim covers one and a half device
    /// pixels in from the edge, a glass's one.
    #[test]
    fn flat_v2s_rim_covers_one_dip_of_device_pixels() {
        use crate::gfx::promo_kit::{rect, Canvas};
        use crate::syslook::SysLook;
        use crate::theme::{Finish, Material};
        let white = Rgba { r: 255, g: 255, b: 255, a: 255 };
        // How much light the rim lays across the left edge, halfway down,
        // in device pixels of full white.
        let laid = |theme: &Theme, s: f32| {
            let cv = Canvas::at(80.0, 40.0, s);
            unsafe {
                cv.dc.Clear(Some(&col(Rgba { r: 0, g: 0, b: 0, a: 255 })));
                rim(&cv.dc, rect(10.0, 10.0, 70.0, 30.0), 6.0, [white; 3], theme.rim_width(Material::Panel, s)).unwrap();
            }
            let (px, w, _) = cv.finish();
            let y = (20.0 * s) as u32;
            (0..(16.0 * s) as u32).map(|x| ga(&px, w, x, y).0 as f32 / 255.0).sum::<f32>()
        };
        let flat = Theme::graphite().resolve_fx(&SysLook::default(), true);
        let mut raw = Theme::graphite();
        raw.finish = Finish::Glass;
        let glass = raw.resolve_fx(&SysLook::default(), true);
        for s in [1.0f32, 1.5, 2.0] {
            assert!((laid(&flat, s) - s).abs() < 0.15, "Flat v2 at {s}: {} device px", laid(&flat, s));
            assert!((laid(&glass, s) - 1.0).abs() < 0.15, "a glass at {s}: {} device px", laid(&glass, s));
        }
    }

    /// The review's P2 3 (2026-11-11): Launchpad's folder panel (one D2D
    /// picture) wears Flat v2's grain; its tile is made once per device
    /// and again on another.
    #[test]
    fn the_grain_lies_on_a_d2d_plate_and_its_tile_is_kept_per_device() {
        use crate::gfx::promo_kit::{rect, Canvas};
        let grey = Rgba { r: 0x2C, g: 0x2C, b: 0x2C, a: 255 };
        let draw = |cv: &Canvas, opacity: f32| unsafe {
            cv.dc.Clear(Some(&col(grey)));
            grain(&cv.dc, rect(0.0, 0.0, 64.0, 64.0), 8.0, opacity).unwrap();
        };
        let kept = || GRAIN.with(|g| g.borrow().as_ref().map(|(_, b)| b.clone()));
        let cv = Canvas::at(64.0, 64.0, 1.5);
        draw(&cv, 0.035);
        let first = kept().expect("made");
        draw(&cv, 0.035);
        assert_eq!(kept().as_ref(), Some(&first), "the same device: the same tile");
        let (px, w, _) = cv.finish();
        let mut seen: Vec<u8> = (8..88).map(|x| ga(&px, w, x, 48).0).collect();
        seen.sort_unstable();
        seen.dedup();
        assert!(seen.len() >= 3, "grain: {seen:?}");
        // Another device (a canvas of its own): made again.
        let other = Canvas::at(64.0, 64.0, 1.0);
        draw(&other, 0.035);
        assert_ne!(kept().as_ref(), Some(&first), "another device: another tile");
        // None (a Flat the PC forces): the plate as it is.
        let none = Canvas::at(64.0, 64.0, 1.0);
        draw(&none, 0.0);
        let (px, w, _) = none.finish();
        assert!((0..64).all(|x| ga(&px, w, x, 32).0 == grey.g));
    }

    #[test]
    fn a_wells_shade_lies_along_its_top_only() {
        use crate::gfx::promo_kit::{rect, Canvas};
        use crate::syslook::SysLook;
        let theme = Theme::graphite().resolve_fx(&SysLook { light: false, ..Default::default() }, false);
        let cv = Canvas::at(120.0, 40.0, 1.0);
        unsafe { well(&cv.dc, rect(10.0, 6.0, 110.0, 34.0), 8.0, &theme, 1.0).unwrap() };
        let (px, w, _) = cv.finish();
        // Premultiplied BGRA: the green channel and alpha at (x, y).
        let at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            (px[i + 1], px[i + 3])
        };
        let middle = at(60, 20);
        // The side walls, halfway down: the plain fill, no shade.
        assert_eq!(at(10, 20), middle, "left wall");
        assert_eq!(at(109, 20), middle, "right wall");
        // The top wall: darker than the fill.
        assert!(at(60, 6).0 < middle.0, "top {:?} vs middle {:?}", at(60, 6), middle);
    }

    /// states-issues 1 (2026-11-11): the ring on whole device pixels. At
    /// 150 % a slider on a half pixel spread its 3 px ring over 4 grey
    /// rows. Snapped: the outer edge on a pixel's edge, the width whole,
    /// at every scale; drawn, every pixel of the ring's band is full (no
    /// grey fringe), on a rect at any fraction of a pixel.
    #[test]
    fn the_focus_ring_lands_on_whole_pixels() {
        for w in [2.0f32, 2.5, 3.0, 3.5] {
            for off in [0.0f32, 0.25, 0.5, 0.75] {
                let c = D2D_RECT_F { left: 10.0 + off, top: 20.0 + off, right: 90.0 + off, bottom: 50.0 + off };
                let (s, wp) = snap_ring(c, w);
                assert_eq!(wp, w.round());
                for edge in [s.left - wp / 2.0, s.top - wp / 2.0, s.right + wp / 2.0, s.bottom + wp / 2.0] {
                    assert_eq!(edge, edge.round(), "w {w} off {off}: {s:?}");
                }
                assert!((s.right - s.left - (c.right - c.left)).abs() <= 1.0);
            }
        }
        // Drawn at 150 % on a control half a pixel off: the ring's column
        // through its top edge is 3 full pixels, nothing grey beside them.
        let cv = crate::gfx::promo_kit::Canvas::at(60.0, 40.0, 1.5);
        let theme = crate::gfx::promo_kit::theme(false);
        unsafe {
            cv.dc.SetTransform(&Matrix3x2 { M11: 1.5, M12: 0.0, M21: 0.0, M22: 1.5, M31: 0.0, M32: 0.0 });
            focus_ring(&cv.dc, D2D_RECT_F { left: 10.0, top: 10.333, right: 50.0, bottom: 30.333 }, 8.0, &theme, 1.0).unwrap();
        }
        let (px, w, _) = cv.finish();
        let x = (30.0 * 1.5) as usize;
        let alphas: Vec<u8> = (0..30).map(|y| px[(y * w as usize + x) * 4 + 3]).filter(|&a| a > 0).collect();
        assert_eq!(alphas.len(), 3, "{alphas:?}");
    }

    /// states-issues 10: a well smaller than WELL_SHADE_MIN (a page dot)
    /// is drawn round, its top as its bottom (no shade eating its top).
    #[test]
    fn a_small_well_is_round() {
        let cv = crate::gfx::promo_kit::Canvas::at(20.0, 20.0, 1.5);
        let theme = crate::gfx::promo_kit::theme(false);
        unsafe {
            cv.dc.SetTransform(&Matrix3x2 { M11: 1.5, M12: 0.0, M21: 0.0, M22: 1.5, M31: 0.0, M32: 0.0 });
            well(&cv.dc, D2D_RECT_F { left: 6.0, top: 6.0, right: 12.0, bottom: 12.0 }, 3.0, &theme, 1.0).unwrap();
        }
        let (px, w, h) = cv.finish();
        let a = |x: usize, y: usize| px[(y * w as usize + x) * 4 + 3] as i32;
        let x = (9.0 * 1.5) as usize;
        let col: Vec<i32> = (0..h as usize).map(|y| a(x, y)).collect();
        let lit: Vec<usize> = (0..col.len()).filter(|&y| col[y] > 0).collect();
        let (top, bottom) = (lit[0], *lit.last().unwrap());
        assert!((col[top] - col[bottom]).abs() <= 8, "{col:?}");
    }

    #[test]
    fn the_focus_ring_is_two_out_two_wide_and_concentric() {
        let r = D2D_RECT_F { left: 10.0, top: 10.0, right: 90.0, bottom: 38.0 };
        // Its centerline is gap + half its width out; its corner grows by
        // as much, so it shares the element's center.
        let (rr, rad) = focus_ring_rect(r, 8.0, 1.0);
        assert_eq!((rr.left, rr.top, rr.right, rr.bottom, rad), (7.0, 7.0, 93.0, 41.0, 11.0));
        // Its inner edge is FOCUS_GAP from the element, its outer edge 4.
        assert_eq!(r.left - (rr.left + size::FOCUS_W / 2.0), size::FOCUS_GAP);
        assert_eq!(r.left - (rr.left - size::FOCUS_W / 2.0), 4.0);
        // A capsule stays a capsule; in px everything scales.
        let (cr, crad) = focus_ring_rect(r, 14.0, 1.0);
        assert_eq!(crad, (cr.bottom - cr.top) / 2.0);
        let (pr, prad) = focus_ring_rect(D2D_RECT_F { left: 20.0, top: 20.0, right: 180.0, bottom: 76.0 }, 16.0, 2.0);
        assert_eq!((pr.left, prad), (14.0, 22.0));
        // The ink at .80; the chosen accent; Highlight in high contrast.
        use crate::syslook::{HighContrast, SysLook};
        let t = Theme::graphite();
        assert_eq!(focus_color(&t.resolve(&SysLook { light: false, ..Default::default() })), Rgba { r: 255, g: 255, b: 255, a: 204 });
        let mut mine = t.clone();
        mine.accent = "#FFD60A".into();
        assert_eq!(focus_color(&mine.resolve(&SysLook::default())), Rgba { r: 0xFF, g: 0xD6, b: 0x0A, a: 255 });
        let (white, black, yellow) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 }, Rgba { r: 255, g: 255, b: 0, a: 255 });
        let hc = t.resolve(&SysLook { high_contrast: Some(HighContrast { window: black, text: white, highlight: yellow, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() });
        assert_eq!(focus_color(&hc), yellow);
    }

    #[test]
    fn a_scroll_edge_fades_over_twelve_dip() {
        // Not scrolled, no foot: nothing fades.
        assert_eq!(scroll_alpha(0.0, 200.0, false, false), 1.0);
        // Scrolled: 0 at the header's edge, full 12 DIP in.
        assert_eq!(scroll_alpha(0.0, 200.0, true, false), 0.0);
        assert_eq!(scroll_alpha(6.0, 200.0, true, false), 0.5);
        assert_eq!(scroll_alpha(12.0, 200.0, true, false), 1.0);
        assert_eq!(scroll_alpha(200.0, 200.0, true, false), 1.0, "the far edge is cut, not faded");
        // A fixed foot: the same band, the other way up.
        assert_eq!(scroll_alpha(200.0, 200.0, false, true), 0.0);
        assert_eq!(scroll_alpha(194.0, 200.0, true, true), 0.5);
        assert_eq!(size::SCROLL_FADE, 12.0);
    }

    #[test]
    fn the_ring_turns_once_in_900_ms_and_stands_still_reduced() {
        assert_eq!(spinner_phase(0.0, true), 0.0);
        assert!((spinner_phase(450.0, true) - 0.5).abs() < 1e-6);
        assert!((spinner_phase(900.0 + 225.0, true) - 0.25).abs() < 1e-6);
        // Reduced motion: a still arc.
        for t in [0.0, 450.0, 1234.0] {
            assert_eq!(spinner_phase(t, false), 0.0);
        }
        assert_eq!((size::SPINNER, size::SPINNER_STROKE), (16.0, 1.5));
    }

    #[test]
    fn a_quiet_block_is_centered() {
        // 24 glyph, 8, an 18 line: 50 tall, in the middle of 96.
        let (gy, ty) = quiet_rows(size::QUIET_MIN_H, 18.0);
        assert_eq!((gy, ty), (35.0, 55.0));
        // The ring alone (no line) sits in the middle.
        assert_eq!(quiet_rows(96.0, 0.0).0, 48.0);
        // Too short an area: from its top.
        assert_eq!(quiet_rows(40.0, 18.0), (12.0, 32.0));
    }
}
