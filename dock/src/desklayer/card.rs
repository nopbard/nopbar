//! The widgets' glass and typography, for live cards and offscreen
//! pictures alike (docs/design/widgets-2026-11-17b/mock5.html, A).
//!
//! The glass is static: the wallpaper under the card, blurred once with its
//! colour kept (crate::desklayer::backdrop), under a thin dark or light
//! tint (thicker on a bright piece, so the body text keeps 4.5:1); a 1 DIP
//! light inside the edge, brightest along the top; one soft shadow. Text is
//! three opaque greys (ink): the RichEdit a note is typed in takes no
//! transparency, and typing must look the same as the card at rest.
pub use super::widget::{PAD, RADIUS};
use std::sync::LazyLock;

use super::backdrop::Under;
use crate::{
    companion::chat::paint::{color, rect, rounded},
    gfx::Gfx,
    theme::{AdaptiveGlass, Rgba, Theme},
};
use windows::{
    core::{Interface, Result},
    Win32::Graphics::{
        Direct2D::{Common::*, *},
        DirectWrite::*,
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM},
    },
};
use windows_numerics::{Matrix3x2, Vector2};

const fn grey(v: u8, a: u8) -> Rgba {
    Rgba { r: v, g: v, b: v, a }
}

/// The card without a picture under it (none, a plain wallpaper, not read).
pub fn surface(light: bool) -> Rgba {
    if light {
        Rgba { r: 0xF2, g: 0xF2, b: 0xF2, a: 255 }
    } else {
        Rgba { r: 0x1C, g: 0x1C, b: 0x1E, a: 255 }
    }
}

/// The tint over the picture at its thinnest: dark #121214 at 32 %, light
/// #FFFFFF at 50 % (mock5.html A).
pub fn tint(light: bool) -> Rgba {
    if light {
        Rgba { r: 0xFF, g: 0xFF, b: 0xFF, a: (0.50f32 * 255.).round() as u8 }
    } else {
        Rgba { r: 0x12, g: 0x12, b: 0x14, a: (0.32f32 * 255.).round() as u8 }
    }
}

/// The tint's thinnest and thickest cover (dark 32..55 %, light 50..72 %).
pub fn cover_range(light: bool) -> (f32, f32) {
    if light {
        (0.50, 0.72)
    } else {
        (0.32, 0.55)
    }
}

/// Pure: WCAG relative luminance of an sRGB colour (0..255).
#[cfg(test)]
fn luminance(c: [f32; 3]) -> f32 {
    let lin = |v: f32| {
        let v = (v / 255.).clamp(0., 1.);
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// Pure: the contrast ratio of two sRGB colours.
#[cfg(test)]
fn contrast(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The contrast the body text (14) keeps on the glass.
#[cfg(test)]
const BODY_CONTRAST: f32 = 4.5;

/// Pure: the glass's colour: `mean` (the picture's, sRGB) under the tint at
/// `a`.
#[cfg(test)]
pub(super) fn glass_colour(light: bool, mean: [f32; 3], a: f32) -> [f32; 3] {
    let t = tint(light);
    let tc = [t.r as f32, t.g as f32, t.b as f32];
    std::array::from_fn(|i| mean[i] * (1. - a) + tc[i] * a)
}

/// Pure: how thick the tint lies over a picture of mean colour `mean`: its
/// thinnest, or just thick enough for the body text to keep 4.5:1, up to
/// its thickest (cover_range).
#[cfg(test)]
fn cover(light: bool, mean: [f32; 3]) -> f32 {
    let (lo, hi) = cover_range(light);
    let text = [ink_value(light, 2) as f32; 3];
    // Whole percents, from the thinnest up.
    let (lo_pc, hi_pc) = ((lo * 100.).round() as i32, (hi * 100.).round() as i32);
    (lo_pc..=hi_pc).map(|p| p as f32 / 100.).find(|a| contrast(text, glass_colour(light, mean, *a)) >= BODY_CONTRAST).unwrap_or(hi)
}

/// The light inside the edge, top to bottom: (offset, white alpha) stops
/// (mock5.html's ::before: 35 %, 5 % at 45 %, 8 %).
pub fn rim() -> [(f32, f32); 3] {
    [(0., 0.35), (0.45, 0.05), (1., 0.08)]
}

/// The hairline inside the edge: white 10 % (light: 50 %).
pub fn hairline(light: bool) -> f32 {
    if light {
        0.50
    } else {
        0.10
    }
}

/// The shadow (x, y, blur, colour; CSS box-shadow terms): one soft one
/// (y 4, blur 14, black 18 %; light 12 %); a lifted card (dragged) casts
/// a deeper one (y 18, blur 40, 35 %).
pub fn shadows(light: bool, lift: bool) -> [(f32, f32, f32, Rgba); 1] {
    let k = |a: f32| Rgba { r: 0, g: 0, b: 0, a: (a * 255.).round() as u8 };
    if lift {
        [(0., 18., 40., k(0.35))]
    } else if light {
        [(0., 4., 14., k(0.12))]
    } else {
        [(0., 4., 14., k(0.18))]
    }
}

fn resting_shadow(light: bool, adaptive: bool) -> [(f32, f32, f32, Rgba); 1] {
    if adaptive {
        [(0., 6., 20., grey(0, if light { 20 } else { 0 }))]
    } else {
        shadows(light, false)
    }
}

/// The shadow of a card lifted by `t` (0 lying .. 1 held): between the
/// lying one and the lifted one.
pub fn lift_shadow(light: bool, adaptive: bool, t: f32) -> [(f32, f32, f32, Rgba); 1] {
    let t = t.clamp(0., 1.);
    let (a, b) = (resting_shadow(light, adaptive)[0], shadows(light, true)[0]);
    let mix = |x: f32, y: f32| x + (y - x) * t;
    [(0., mix(a.1, b.1), mix(a.2, b.2), Rgba { r: 0, g: 0, b: 0, a: mix(a.3.a as f32, b.3.a as f32).round() as u8 })]
}

/// The opaque grey of text tier `tier` (see ink()).
fn ink_value(light: bool, tier: u8) -> u8 {
    match (light, tier) {
        // Title #FFF; body white 86 % and header white 75 % laid over the
        // glass's usual grey (opaque: the RichEdit takes no alpha).
        (false, 1) => 0xFF,
        (false, 2) => 0xE4,
        (false, _) => 0xC8,
        // Light: #111; black 72 % and 60 % over the light glass.
        (true, 1) => 0x11,
        (true, 2) => 0x40,
        (true, _) => 0x5E,
    }
}

/// The three opaque greys of a card's text: 1 titles and values, 2 body,
/// 3 labels and secondary marks.
pub fn ink(theme: &Theme, tier: u8) -> Rgba {
    grey(ink_value(theme.is_light, tier), 255)
}

pub fn glass_for(theme: &Theme, under: Option<&Under>) -> Option<AdaptiveGlass> {
    if theme.high_contrast || theme.transparency_off {
        return None;
    }
    under.filter(|u| u.card.right > u.card.left && u.card.bottom > u.card.top).map(|u| u.glass)
}

/// Widget text and icons use a fixed palette for each glass polarity.
pub fn theme_for<'a>(theme: &'a Theme, under: Option<&Under>) -> &'a Theme {
    static PALETTES: LazyLock<[Theme; 2]> = LazyLock::new(|| {
        [Theme::graphite(), Theme { is_light: true, ..Theme::graphite() }]
    });
    match glass_for(theme, under) {
        Some(glass) => &PALETTES[usize::from(glass.light)],
        None => theme,
    }
}
pub fn rgba(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255., g: c.g as f32 / 255., b: c.b as f32 / 255., a: c.a as f32 / 255. }
}
fn rr(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}

/// The card's plate at (0, 0), `w` x `h` DIP: its shadow (shadow())
/// and its glass (body()) in one picture: edit mode, the gallery's
/// preview and offscreen pictures. A live card is the two in two windows:
/// the glass in the card's own, the shadow in a click-through one under it
/// (crate::desklayer::shade); together they are this picture.
pub unsafe fn plate(dc: &ID2D1DeviceContext, theme: &Theme, w: f32, h: f32, lift: bool, under: Option<&Under>) -> Result<()> {
    let glass = glass_for(theme, under);
    if lift {
        crate::gfx::soft_shadows(dc, rect(0., 0., w, h), RADIUS, &shadows(theme.is_light, true))?;
    } else {
        shadow(dc, glass.map_or(theme.is_light, |g| g.light), glass.is_some(), w, h)?;
    }
    body(dc, theme, w, h, under)
}

/// What falls outside the card: its shadow.
pub unsafe fn shadow(dc: &ID2D1DeviceContext, light: bool, adaptive: bool, w: f32, h: f32) -> Result<()> {
    if adaptive && !light {
        return Ok(());
    }
    crate::gfx::soft_shadows(dc, rect(0., 0., w, h), RADIUS, &resting_shadow(light, adaptive))
}

/// Apply the tint and precomputed dither, turning the result into opaque BGRA.
pub(super) fn tinted(px: &mut [u8], t: Rgba) {
    let a = f32::from(t.a) / 255.;
    let (tb, tg, tr) = (t.b as f32 * a, t.g as f32 * a, t.r as f32 * a);
    let k = 1. - a;
    let gain = 2. * super::backdrop::DITHER / 255.;
    for p in px.as_chunks_mut::<4>().0 {
        let dither = f32::from(p[3]) * gain - super::backdrop::DITHER;
        p[0] = (p[0] as f32 * k + tb + dither).round() as u8;
        p[1] = (p[1] as f32 * k + tg + dither).round() as u8;
        p[2] = (p[2] as f32 * k + tr + dither).round() as u8;
        p[3] = 255;
    }
}

/// The card itself: the glass over the picture `under` (solid without
/// one) and the light inside its edge.
pub unsafe fn body(dc: &ID2D1DeviceContext, theme: &Theme, w: f32, h: f32, under: Option<&Under>) -> Result<()> {
    let light = theme.is_light;
    let card = rect(0., 0., w, h);
    let live_cover = if theme.high_contrast || theme.transparency_off { None } else { super::live::area(dc, w, h, light) };
    if let Some(alpha) = live_cover {
        let mut tint = rgba(tint(light));
        tint.a = alpha;
        let brush = dc.CreateSolidColorBrush(&tint, None)?;
        // Replace a gallery panel or shadow already painted underneath: the
        // host brush below this surface must show through the card interior.
        let blend = dc.GetPrimitiveBlend();
        dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_COPY);
        dc.FillRoundedRectangle(&rr(card, RADIUS), &brush);
        dc.SetPrimitiveBlend(blend);
    } else if let Some((u, glass)) = under.zip(glass_for(theme, under)) {
        // The piece of the display's picture under the card, cut at the
        // card's own px (the picture is at the display's): pixel for pixel.
        let (cw, ch, mut px) = u.grey.cut(u.card);
        tinted(&mut px, glass.tint);
        let bmp = bitmap(dc, cw as u32, ch as u32, &px)?;
        let brush = dc.CreateBitmapBrush(
            &bmp,
            Some(&D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_INTERPOLATION_MODE_LINEAR }),
            None,
        )?;
        brush.SetTransform(&Matrix3x2 { M11: w / cw as f32, M22: h / ch as f32, ..Matrix3x2::identity() });
        dc.FillRoundedRectangle(&rr(card, RADIUS), &brush);
        return glass_edge(dc, glass.light, w, h);
    } else {
        rounded(dc, card, RADIUS, rgba(surface(light)))?;
    }
    edge(dc, light, w, h)
}

/// The C card's inset outline and top highlight, each one DIP thick.
pub(super) unsafe fn glass_edge(dc: &ID2D1DeviceContext, light: bool, w: f32, h: f32) -> Result<()> {
    let inner = rr(rect(0.5, 0.5, w - 1., h - 1.), RADIUS - 0.5);
    let (v, a, top) = if light { (0., 0.06, 0.55) } else { (1., 0.07, 0.12) };
    let hair = dc.CreateSolidColorBrush(&color(v, a), None)?;
    dc.DrawRoundedRectangle(&inner, &hair, 1., None);
    let highlight = dc.CreateSolidColorBrush(&color(1., top), None)?;
    dc.DrawLine(Vector2 { X: RADIUS, Y: 0.5 }, Vector2 { X: w - RADIUS, Y: 0.5 }, &highlight, 1., None);
    Ok(())
}

/// The light inside the edge: a hairline all round, and the top light
/// fading down over it.
pub(super) unsafe fn edge(dc: &ID2D1DeviceContext, light: bool, w: f32, h: f32) -> Result<()> {
    let inner = rr(rect(0.5, 0.5, w - 1., h - 1.), RADIUS - 0.5);
    let hair = dc.CreateSolidColorBrush(&color(1., hairline(light)), None)?;
    dc.DrawRoundedRectangle(&inner, &hair, 1., None);
    let stops: Vec<D2D1_GRADIENT_STOP> = rim().iter().map(|(at, a)| D2D1_GRADIENT_STOP { position: *at, color: color(1., *a) }).collect();
    let rt: &ID2D1RenderTarget = dc;
    let stops = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
    let rim = dc.CreateLinearGradientBrush(
        &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: Vector2 { X: 0., Y: 0. }, endPoint: Vector2 { X: 0., Y: h } },
        None,
        &stops,
    )?;
    dc.DrawRoundedRectangle(&inner, &rim, 1., None);
    Ok(())
}

/// Pure: a fixed value in [-1, 1] for pixel (x, y) (a hash: no pattern,
/// never changing).
pub fn noise(x: u32, y: u32) -> f32 {
    let mut v = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ 0x27D4_EB2F;
    v ^= v >> 15;
    v = v.wrapping_mul(0x2C1B_3C6D);
    v ^= v >> 12;
    v = v.wrapping_mul(0x297A_2D39);
    v ^= v >> 15;
    (v as f32 / u32::MAX as f32) * 2. - 1.
}

/// A device bitmap of `pixels` (premultiplied BGRA).
unsafe fn bitmap(dc: &ID2D1DeviceContext, w: u32, h: u32, pixels: &[u8]) -> Result<ID2D1Bitmap1> {
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.,
        dpiY: 96.,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(pixels.as_ptr() as *const _), w * 4, &props)
}

/// Scales what follows by edit_view::LIFT about the middle of a card
/// `w` x `h` at (0, 0): the card being dragged.
pub unsafe fn lifted(dc: &ID2D1DeviceContext, w: f32, h: f32) {
    use crate::companion::render::{scale as by, translate};
    let k = super::edit_view::LIFT;
    let mut t = Matrix3x2::identity();
    dc.GetTransform(&mut t);
    dc.SetTransform(&(translate(-w / 2., -h / 2.) * by(k, k) * translate(w / 2., h / 2.) * t));
}

/// Wrapped text cut with "…" to the lines that fit `h` (13 DIP text on
/// 19 DIP lines; others at 1.35).
#[allow(clippy::too_many_arguments)]
pub unsafe fn text(dc: &ID2D1DeviceContext, gfx: &Gfx, s: &str, x: f32, y: f32, size: f32, weight: DWRITE_FONT_WEIGHT, ink: D2D1_COLOR_F, w: f32, h: f32) -> Result<()> {
    if h <= 0. || w <= 0. {
        return Ok(());
    }
    let normalized = crate::gfx::keep_words(s);
    let line_h = if size == 13. { 19. } else { size * 1.35 };
    let make = |s: &str| -> Result<IDWriteTextLayout> {
        let (l, _, _) = gfx.text_layout(s, crate::fonts::THEME_DEFAULT, size, weight)?;
        l.SetMaxWidth(w.max(1.))?;
        l.SetMaxHeight(10000.)?;
        l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        l.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, line_h, size)?;
        Ok(l)
    };
    let mut l = make(&normalized)?;
    let mut n = 0;
    let _ = l.GetLineMetrics(None, &mut n);
    let allowed = (h / line_h).floor().max(1.) as usize;
    if n as usize > allowed {
        let mut lines = vec![DWRITE_LINE_METRICS::default(); n as usize];
        l.GetLineMetrics(Some(&mut lines), &mut n)?;
        let units: usize = lines.iter().take(allowed).map(|l| l.length as usize).sum();
        let wide: Vec<u16> = normalized.encode_utf16().take(units).collect();
        let mut prefix = String::from_utf16_lossy(&wide).trim_end().to_string();
        loop {
            l = make(&format!("{}…", prefix.trim_end()))?;
            let mut count = 0;
            let _ = l.GetLineMetrics(None, &mut count);
            if count as usize <= allowed || prefix.pop().is_none() {
                break;
            }
        }
    }
    l.SetMaxHeight(h.max(line_h))?;
    let sign = gfx.dwrite().CreateEllipsisTrimmingSign(&l)?;
    l.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() }, &sign)?;
    let b = dc.CreateSolidColorBrush(&ink, None)?;
    dc.PushAxisAlignedClip(&rect(x, y, w.max(1.), h.max(1.)), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    if super::live::drawing() {
        let shadow = dc.CreateSolidColorBrush(&color(0., 0.12), None)?;
        dc.DrawTextLayout(Vector2 { X: x, Y: y + 0.75 }, &l, &shadow, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    dc.DrawTextLayout(Vector2 { X: x, Y: y }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
    dc.PopAxisAlignedClip();
    Ok(())
}

/// How one line of text is set.
#[derive(Clone, Copy)]
pub struct Line {
    pub size: f32,
    pub weight: DWRITE_FONT_WEIGHT,
    /// The line box (CSS line-height): the text sits in its middle.
    pub height: f32,
    /// Letter spacing in em (-0.03: −3 %).
    pub tracking: f32,
    /// Figures all one width.
    pub tabular: bool,
}
impl Line {
    pub const fn new(size: f32, weight: i32, height: f32) -> Self {
        Self { size, weight: DWRITE_FONT_WEIGHT(weight), height, tracking: 0., tabular: false }
    }
}

/// One line of `s` at (x, y) (the top of its line box), cut with "…" at
/// `w`; returns the width drawn.
#[allow(clippy::too_many_arguments)]
pub unsafe fn line(dc: &ID2D1DeviceContext, gfx: &Gfx, s: &str, x: f32, y: f32, l: Line, ink: Rgba, w: f32) -> Result<f32> {
    if w <= 0. || s.is_empty() {
        return Ok(0.);
    }
    let (layout, _, th) = if l.tabular {
        gfx.text_layout_tabular(s, crate::fonts::THEME_DEFAULT, l.size, l.weight)?
    } else {
        gfx.text_layout(s, crate::fonts::THEME_DEFAULT, l.size, l.weight)?
    };
    layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
    if l.tracking != 0. {
        if let Ok(l1) = layout.cast::<IDWriteTextLayout1>() {
            let n = s.encode_utf16().count() as u32;
            let _ = l1.SetCharacterSpacing(0., l.tracking * l.size, 0., DWRITE_TEXT_RANGE { startPosition: 0, length: n });
        }
    }
    layout.SetMaxWidth(w.max(1.))?;
    let sign = gfx.dwrite().CreateEllipsisTrimmingSign(&layout)?;
    layout.SetTrimming(&DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() }, &sign)?;
    let mut m = DWRITE_TEXT_METRICS::default();
    layout.GetMetrics(&mut m)?;
    let natural = if m.height > 0. { m.height } else { th };
    let b = dc.CreateSolidColorBrush(&rgba(ink), None)?;
    let top = y + (l.height - natural) / 2.;
    dc.PushAxisAlignedClip(&rect(x, y - 4., w, l.height + 8.), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    if super::live::drawing() {
        let shadow = dc.CreateSolidColorBrush(&color(0., 0.12), None)?;
        dc.DrawTextLayout(Vector2 { X: x, Y: top + 0.75 }, &layout, &shadow, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    dc.DrawTextLayout(Vector2 { X: x, Y: top }, &layout, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
    dc.PopAxisAlignedClip();
    Ok(m.widthIncludingTrailingWhitespace.min(w))
}

/// The width one line of `s` takes.
pub fn width(gfx: &Gfx, s: &str, l: Line) -> f32 {
    let r = if l.tabular { gfx.text_layout_tabular(s, crate::fonts::THEME_DEFAULT, l.size, l.weight) } else { gfx.text_layout(s, crate::fonts::THEME_DEFAULT, l.size, l.weight) };
    r.map_or(0., |(_, w, _)| w)
}

/// The header's name: 12/600.
pub const NAME: Line = Line::new(12., 600, 16.);
/// The header's value (a time, a count): 12/500, figures one width.
pub const META: Line = Line { tabular: true, ..Line::new(12., 500, 16.) };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Note,
    Inbox,
    Battery,
}

/// A line glyph on the 24-unit grid (mock4.html's symbols).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Note,
    Inbox,
    Battery,
    Keyboard,
    Mouse,
    Sun,
    Moon,
    Download,
    Calendar,
    File,
    Image,
    Archive,
}

/// Draws glyph `m` in a `size` DIP box at (x, y), lines 1.7 units wide.
pub unsafe fn mark(dc: &ID2D1DeviceContext, gfx: &Gfx, m: Mark, x: f32, y: f32, size: f32, ink: Rgba) -> Result<()> {
    let u = size / 24.;
    let p = |a: f32, b: f32| Vector2 { X: x + a * u, Y: y + b * u };
    let r = |a: f32, b: f32, w: f32, h: f32| rect(x + a * u, y + b * u, w * u, h * u);
    let brush = dc.CreateSolidColorBrush(&rgba(ink), None)?;
    let factory: &ID2D1Factory = gfx.d2d_factory();
    let style = factory.CreateStrokeStyle(&D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, lineJoin: D2D1_LINE_JOIN_ROUND, ..Default::default() }, None)?;
    let sw = 1.7 * u;
    let line = |a: (f32, f32), b: (f32, f32)| dc.DrawLine(p(a.0, a.1), p(b.0, b.1), &brush, sw, &style);
    let dot = |a: f32, b: f32| dc.FillEllipse(&D2D1_ELLIPSE { point: p(a, b), radiusX: sw / 2., radiusY: sw / 2. }, &brush);
    match m {
        Mark::Note => {
            dc.DrawRoundedRectangle(&rr(r(4.5, 3.5, 15., 17.), 3. * u), &brush, sw, &style);
            line((8., 8.5), (16., 8.5));
            line((8., 12.), (16., 12.));
            line((8., 15.5), (13., 15.5));
        }
        Mark::Inbox => {
            let (outline, tray) = inbox_paths();
            for (points, closed) in [(outline, true), (tray, false)] {
                let path = gfx.d2d_factory().CreatePathGeometry()?;
                let sink = path.Open()?;
                sink.BeginFigure(p(points[0].0, points[0].1), D2D1_FIGURE_BEGIN_HOLLOW);
                for (i, &(a, b)) in points.iter().enumerate().skip(1) {
                    if closed && [2, 4, 7, 9].contains(&i) {
                        sink.AddArc(&D2D1_ARC_SEGMENT { point: p(a, b), size: D2D_SIZE_F { width: 2. * u, height: 2. * u }, rotationAngle: 0., sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE, arcSize: D2D1_ARC_SIZE_SMALL });
                    } else {
                        sink.AddLine(p(a, b));
                    }
                }
                sink.EndFigure(if closed { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
                sink.Close()?;
                dc.DrawGeometry(&path, &brush, sw, &style);
            }
        }
        Mark::Battery => {
            dc.DrawRoundedRectangle(&rr(r(2.5, 7., 17., 10.), 2.5 * u), &brush, sw, &style);
            line((21.5, 10.5), (21.5, 13.5));
            dc.FillRoundedRectangle(&rr(r(4.5, 9., 9., 6.), u), &brush);
        }
        Mark::Keyboard => {
            dc.DrawRoundedRectangle(&rr(r(2.5, 6.5, 19., 11.), 2.5 * u), &brush, sw, &style);
            for a in [6., 9., 12., 15., 18.] {
                dot(a, 10.);
            }
            line((7.5, 14.), (16.5, 14.));
        }
        Mark::Mouse => {
            dc.DrawRoundedRectangle(&rr(r(6.5, 3., 11., 18.), 5.5 * u), &brush, sw, &style);
            line((12., 7.), (12., 10.));
        }
        Mark::Sun => {
            dc.DrawEllipse(&D2D1_ELLIPSE { point: p(12.,12.), radiusX: 4.*u, radiusY: 4.*u }, &brush, sw, &style);
            for (a,b) in [((12.,2.),(12.,4.)),((12.,20.),(12.,22.)),((2.,12.),(4.,12.)),((20.,12.),(22.,12.)),((5.,5.),(6.5,6.5)),((17.5,17.5),(19.,19.)),((5.,19.),(6.5,17.5)),((17.5,6.5),(19.,5.))] { line(a,b); }
        }
        Mark::Moon => {
            let path = factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.BeginFigure(p(19.5,15.5), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1:p(9.,20.),point2:p(4.,9.),point3:p(10.,3.) });
            sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1:p(-1.,5.),point2:p(2.,22.),point3:p(13.,21.) });
            sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1:p(16.,21.),point2:p(18.5,18.5),point3:p(19.5,15.5) });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED); sink.Close()?;
            dc.DrawGeometry(&path,&brush,sw,&style);
        }
        Mark::Download => {
            line((12.,3.),(12.,15.)); line((7.,10.),(12.,15.)); line((12.,15.),(17.,10.));
            line((4.,17.),(4.,21.));line((4.,21.),(20.,21.));line((20.,21.),(20.,17.));
        }
        Mark::Calendar => {
            dc.DrawRoundedRectangle(&rr(r(3.,5.,18.,16.),2.*u),&brush,sw,&style);
            line((3.,10.),(21.,10.));line((8.,3.),(8.,7.));line((16.,3.),(16.,7.));
            for (a,b) in [(8.,14.),(12.,14.),(16.,14.),(8.,17.),(12.,17.)] { dot(a,b); }
        }
        Mark::File | Mark::Archive | Mark::Image => {
            let path=factory.CreatePathGeometry()?; let sink=path.Open()?;
            sink.BeginFigure(p(5.,3.),D2D1_FIGURE_BEGIN_HOLLOW);
            for (a,b) in [(14.,3.),(19.,8.),(19.,21.),(5.,21.)] {sink.AddLine(p(a,b));}
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);sink.Close()?;dc.DrawGeometry(&path,&brush,sw,&style);
            line((14.,3.),(14.,8.));line((14.,8.),(19.,8.));
            if m==Mark::Image {line((8.,17.),(11.,13.));line((11.,13.),(16.,18.));dot(8.,10.);}
            if m==Mark::Archive {for y in [6.,9.,12.,15.] {line((9.,y),(11.,y));}line((9.,18.),(12.,18.));}
        }
    }
    Ok(())
}

/// The header every card has: its glyph (14) and name (12/600) in the
/// third grey, a value (12/500, figures one width) at the right.
pub unsafe fn header(dc: &ID2D1DeviceContext, gfx: &Gfx, theme: &Theme, kind: Kind, name: &str, meta: &str, w: f32) -> Result<()> {
    let glyph = match kind {
        Kind::Note => Mark::Note,
        Kind::Inbox => Mark::Inbox,
        Kind::Battery => Mark::Battery,
    };
    header_mark(dc,gfx,theme,glyph,name,meta,w)
}

/// Shared header with a content-specific glyph, including assistant cards.
pub unsafe fn header_mark(dc: &ID2D1DeviceContext, gfx: &Gfx, theme: &Theme, glyph: Mark, name: &str, meta: &str, w: f32) -> Result<()> {
    let ink = ink(theme, 3);
    mark(dc, gfx, glyph, PAD, PAD + 1., 14., ink)?;
    let x = PAD + 20.;
    let mw = if meta.is_empty() { 0. } else { width(gfx, meta, META).min(w * 0.45) };
    line(dc, gfx, name, x, PAD, NAME, ink, (w - PAD - x - mw - 6.).max(1.))?;
    if !meta.is_empty() {
        line(dc, gfx, meta, w - PAD - mw, PAD, META, ink, mw + 1.)?;
    }
    Ok(())
}

/// Shared pill label: DirectWrite centers its real line box in both axes.
pub fn pill_layout(gfx: &Gfx, s: &str, r: D2D_RECT_F, size: f32) -> Result<IDWriteTextLayout> {
    let (layout,_,_)=gfx.text_layout(s,crate::fonts::THEME_DEFAULT,size,DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
    unsafe {
        layout.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        layout.SetMaxWidth((r.right-r.left-24.).max(1.))?;
        layout.SetMaxHeight((r.bottom-r.top).max(1.))?;
        layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let sign=gfx.dwrite().CreateEllipsisTrimmingSign(&layout)?;
        layout.SetTrimming(&DWRITE_TRIMMING{granularity:DWRITE_TRIMMING_GRANULARITY_CHARACTER,..Default::default()},&sign)?;
    }
    Ok(layout)
}

/// Graphite action pill: primary in tier one, secondary in the hover material.
pub unsafe fn pill(dc: &ID2D1DeviceContext,gfx:&Gfx,theme:&Theme,s:&str,r:D2D_RECT_F,primary:bool)->Result<()> {
    let face=if primary {rgba(ink(theme,1))} else {color(if theme.is_light{0.}else{1.},if theme.is_light{0.07}else{0.12})};
    let text=if primary {color(if theme.is_light{1.}else{0.},1.)} else {rgba(ink(theme,1))};
    draw_pill(dc,gfx,s,r,face,text)
}

unsafe fn draw_pill(dc: &ID2D1DeviceContext, gfx: &Gfx, s: &str, r: D2D_RECT_F, face: D2D1_COLOR_F, text: D2D1_COLOR_F) -> Result<()> {
    rounded(dc,r,16.,face)?;
    let layout=pill_layout(gfx,s,r,13.)?;
    let brush=dc.CreateSolidColorBrush(&text,None)?;
    dc.PushAxisAlignedClip(&r,D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    dc.DrawTextLayout(Vector2{X:r.left+12.,Y:r.top},&layout,&brush,D2D1_DRAW_TEXT_OPTIONS_NONE);
    dc.PopAxisAlignedClip();
    Ok(())
}

/// A note's title line (DIP).
pub const TITLE_LINE: f32 = 26.;
/// A note's title: 20/700 on 26 (18 on a small card). No −3 % tracking
/// (mock4.html has it): the editor's RichEdit cannot set it, and the title
/// must not move when typing starts (review: it showed doubled).
pub fn title_line(w: f32) -> Line {
    Line::new(if w < 200. { 18. } else { 20. }, 700, TITLE_LINE)
}
/// A note's body: 14 on 20.
pub const BODY: Line = Line::new(14., 400, 20.);
/// Where a note's title and body start (DIP from the card's top).
pub const TITLE_TOP: f32 = 44.;
pub const BODY_TOP: f32 = 74.;

/// Pure: the body lines that fit a card `h` tall.
pub fn body_lines(h: f32) -> usize {
    ((h - PAD - BODY_TOP) / BODY.height).floor().max(0.) as usize
}

/// A note's face: header (its date at the right), the first line as the
/// title, the next lines each on one line cut with "…".
pub unsafe fn note(dc: &ID2D1DeviceContext, gfx: &Gfx, theme: &Theme, body: &str, date: &str, w: f32, h: f32) -> Result<()> {
    header(dc, gfx, theme, Kind::Note, crate::i18n::t("widgets.notes"), date, w)?;
    let (title, rest) = crate::notes::face::split(body);
    let inner = w - 2. * PAD;
    if title.is_empty() {
        line(dc, gfx, crate::i18n::t("notes.new"), PAD, TITLE_TOP, title_line(w), ink(theme, 3), inner)?;
    } else {
        line(dc, gfx, &title, PAD, TITLE_TOP, title_line(w), ink(theme, 1), inner)?;
    }
    for (i, l) in rest.lines().take(body_lines(h)).enumerate() {
        line(dc, gfx, l, PAD, BODY_TOP + i as f32 * BODY.height, BODY, ink(theme, 2), inner)?;
    }
    Ok(())
}

/// A hover button on a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Btn {
    /// A new note beside this one.
    Add,
    /// The card's menu.
    More,
}
pub const BTN: f32 = 26.;
const BTN_INSET: f32 = 10.;
const BTN_GAP: f32 = 6.;
/// The hover buttons of a card `w` DIP wide, right to left from its top
/// right corner, inside the card: a note has + and …, others … only.
pub fn buttons(kind: Kind, w: f32) -> Vec<(Btn, D2D_RECT_F)> {
    let at = |i: f32| rect(w - BTN_INSET - BTN - i * (BTN + BTN_GAP), BTN_INSET, BTN, BTN);
    match kind {
        Kind::Note => vec![(Btn::More, at(0.)), (Btn::Add, at(1.))],
        Kind::Inbox | Kind::Battery => vec![(Btn::More, at(0.))],
    }
}
/// The hover button under (x, y) (card DIP).
pub fn button_at(kind: Kind, w: f32, x: f32, y: f32) -> Option<Btn> {
    buttons(kind, w).into_iter().find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(b, _)| b)
}
/// The hover buttons at `alpha` (0: none): 26 DIP circles of white 12 %
/// (black 7 % on light), 13 DIP glyphs in the first grey.
pub unsafe fn controls(dc: &ID2D1DeviceContext, theme: &Theme, kind: Kind, w: f32, alpha: f32) -> Result<()> {
    if alpha <= 0. {
        return Ok(());
    }
    let v = if theme.is_light { 0. } else { 1. };
    let i = ink(theme, 1);
    let ink = dc.CreateSolidColorBrush(&D2D1_COLOR_F { a: alpha, ..rgba(i) }, None)?;
    for (b, r) in buttons(kind, w) {
        rounded(dc, r, BTN / 2., color(v, if theme.is_light { 0.07 } else { 0.12 } * alpha))?;
        let (cx, cy) = ((r.left + r.right) / 2., (r.top + r.bottom) / 2.);
        // 13 DIP glyphs on the 24-unit grid (mock4.html #plus, #dots).
        let u = 13. / 24.;
        match b {
            Btn::Add => {
                dc.DrawLine(Vector2 { X: cx - 7. * u, Y: cy }, Vector2 { X: cx + 7. * u, Y: cy }, &ink, 2. * u, None);
                dc.DrawLine(Vector2 { X: cx, Y: cy - 7. * u }, Vector2 { X: cx, Y: cy + 7. * u }, &ink, 2. * u, None);
            }
            Btn::More => {
                for dx in [-6., 0., 6.] {
                    dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx + dx * u, Y: cy }, radiusX: 1.6 * u, radiusY: 1.6 * u }, &ink);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn review_11_inbox_tray_has_sloped_sides_and_recess() {
        let (outline, tray) = inbox_paths();
        assert_eq!(outline[0], (4., 13.5));
        assert_eq!(outline[1], (6.3, 6.));
        assert_eq!(tray, &[(4., 13.5), (8.5, 13.5), (9.5, 15.5), (14.5, 15.5), (15.5, 13.5), (20., 13.5)]);
        assert!(outline.iter().any(|&(_, y)| y == 20.));
    }
    /// Every hover button sits inside its card (owner 0.1.91: the battery's
    /// … stuck out of the card), clear of the rounded corner.
    #[test]
    fn hover_buttons_stay_inside_the_card() {
        for w in [136., 144., 288., 296.] {
            for kind in [Kind::Note, Kind::Inbox, Kind::Battery] {
                for (_, r) in buttons(kind, w) {
                    assert!(r.left >= PAD - 6. && r.right <= w - 10. && r.top >= 10. && r.bottom <= 36., "{r:?}");
                    // The corner circle (radius RADIUS) holds the button's top right.
                    let (cx, cy) = (w - RADIUS, RADIUS);
                    let (dx, dy) = (r.right - cx, cy - r.top);
                    assert!(dx <= 0. || dy <= 0. || (dx * dx + dy * dy).sqrt() <= RADIUS, "{r:?}");
                }
            }
            assert_eq!(button_at(Kind::Note, w, w - 20., 20.), Some(Btn::More));
            assert_eq!(button_at(Kind::Note, w, w - 52., 20.), Some(Btn::Add));
            assert_eq!(button_at(Kind::Battery, w, w - 52., 20.), None);
            assert_eq!(button_at(Kind::Inbox, w, w - 5., 5.), None);
        }
    }
    /// mock5.html A's numbers: three opaque greys per theme, the tint,
    /// the edge light, the shadow and the fallback colours.
    #[test]
    fn glass_takes_the_mock_numbers() {
        let dark = Theme::graphite();
        let light = Theme { is_light: true, ..Theme::graphite() };
        assert_eq!([ink(&dark, 1), ink(&dark, 2), ink(&dark, 3)].map(|c| c.r), [0xFF, 0xE4, 0xC8]);
        assert_eq!([ink(&light, 1), ink(&light, 2), ink(&light, 3)].map(|c| c.r), [0x11, 0x40, 0x5E]);
        for t in [1, 2, 3] {
            for th in [&dark, &light] {
                let c = ink(th, t);
                assert!(c.r == c.g && c.g == c.b && c.a == 255, "opaque grey");
            }
        }
        assert_eq!(tint(false), Rgba { r: 0x12, g: 0x12, b: 0x14, a: 82 });
        assert_eq!(tint(true), Rgba { r: 0xFF, g: 0xFF, b: 0xFF, a: 128 });
        assert_eq!(rim(), [(0., 0.35), (0.45, 0.05), (1., 0.08)]);
        assert_eq!((hairline(false), hairline(true)), (0.10, 0.50));
        assert_eq!(shadows(false, false)[0], (0., 4., 14., Rgba { r: 0, g: 0, b: 0, a: 46 }));
        assert_eq!(shadows(false, true)[0], (0., 18., 40., Rgba { r: 0, g: 0, b: 0, a: 89 }));
        assert_eq!(surface(false), Rgba { r: 0x1C, g: 0x1C, b: 0x1E, a: 255 });
        assert_eq!(surface(true), Rgba { r: 0xF2, g: 0xF2, b: 0xF2, a: 255 });
    }
    /// The tint thickens only where the body text would fall under 4.5:1,
    /// never past its thickest; a dark (light) wallpaper keeps the thinnest.
    #[test]
    fn the_tint_thickens_for_the_body_text() {
        let night = [30., 40., 80.];
        let sky = [200., 210., 215.];
        let grass = [60., 90., 50.];
        assert_eq!(cover(false, night), 0.32);
        assert_eq!(cover(true, sky), 0.50);
        let body = |light: bool| [ink_value(light, 2) as f32; 3];
        // Bright sky in the dark theme: thicker, just enough.
        let a = cover(false, sky);
        assert!(a > 0.32 && a <= 0.55, "{a}");
        if a < 0.55 {
            assert!(contrast(body(false), glass_colour(false, sky, a)) >= BODY_CONTRAST);
            assert!(contrast(body(false), glass_colour(false, sky, a - 0.01)) < BODY_CONTRAST, "no thicker than needed");
        }
        // White in the dark theme: as thick as it goes, no further.
        assert_eq!(cover(false, [255.; 3]), 0.55);
        // Dark grass in the light theme: thicker, up to 72 %.
        let b = cover(true, grass);
        assert!(b > 0.50 && b <= 0.72, "{b}");
        if b < 0.72 {
            assert!(contrast(body(true), glass_colour(true, grass, b)) >= BODY_CONTRAST);
        }
        // Black in the light theme: thick enough, never past 72 %.
        let k = cover(true, [0.; 3]);
        assert!(k > 0.50 && k <= 0.72 && contrast(body(true), glass_colour(true, [0.; 3], k)) >= BODY_CONTRAST, "{k}");
        // WCAG's own figures: black on white is 21:1.
        assert!((contrast([0.; 3], [255.; 3]) - 21.).abs() < 0.01);
    }
    /// The tint is laid over the piece, opaque.
    #[test]
    fn the_glass_is_the_piece_under_its_tint() {
        let mut px = vec![200, 100, 50, 255];
        tinted(&mut px, AdaptiveGlass::new(0.15, None).tint);
        let dither = super::super::backdrop::DITHER;
        let mix = |p: u8, t: u8| ((f32::from(p) * 156. + f32::from(t) * 99.) / 255. + dither).round() as u8;
        assert_eq!(px, vec![mix(200, 20), mix(100, 18), mix(50, 18), 255]);
    }
    #[test]
    fn a_note_shows_the_body_lines_that_fit() {
        // mock4.html: a medium note shows title and three lines of body.
        assert!(body_lines(176.) >= 3);
        assert_eq!(body_lines(80.), 0);
        assert!(BODY_TOP + body_lines(176.) as f32 * BODY.height <= 176. - PAD);
    }
}

type GlyphPoints = &'static [(f32, f32)];
fn inbox_paths() -> (GlyphPoints, GlyphPoints) {
    // mock.html #inbox, on its original 24-unit viewBox.
    (&[(4., 13.5), (6.3, 6.), (8.2, 4.6), (15.8, 4.6), (17.7, 6.), (20., 13.5), (20., 18.), (18., 20.), (6., 20.), (4., 18.)], &[(4., 13.5), (8.5, 13.5), (9.5, 15.5), (14.5, 15.5), (15.5, 13.5), (20., 13.5)])
}
