//! The window's one picture, in DIPs (460 x 280): the title strip (its
//! close only), the NopBar icon, an arrow, the Programs folder, the hint
//! (the one line of text), the progress bar or a failure message. Drawn by Direct2D into the window or, for the
//! preview, into a bitmap.
//!
//! Right to left (Arabic, Persian, Hebrew) the picture is mirrored: the
//! icon on the right, the arrow pointing left to the folder, the bar
//! filling leftward, the close button top left. The layout below stays
//! left to right; `on_screen` / `screen_x` mirror it when drawing. Clicks
//! are hit-tested in window coordinates against the drawn rects (the same
//! half-open `[left, right)` either way); only the dragged icon's position
//! goes back through `logical`. Text and the gnome mark are placed mirrored
//! but not flipped.

use windows::core::{w, Interface, Result, BOOL, HSTRING};

use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_FILL_MODE_WINDING, D2D1_GRADIENT_STOP, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Factory, ID2D1PathGeometry, ID2D1RenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2, D2D1_LINE_JOIN_ROUND, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFactory2, IDWriteFontCollection, IDWriteTextFormat, IDWriteTextFormat1, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, DWRITE_READING_DIRECTION_RIGHT_TO_LEFT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_UNICODE_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
};
use windows_numerics::{Matrix3x2, Vector2};

use super::theme::{Palette, Rgba};
use crate::strings::{Lang, Strings};

// ---------------------------------------------------------------- layout (DIP)

pub const W: f32 = 460.0;
pub const H: f32 = 280.0;
pub const STRIP: f32 = 30.0;
pub const ICON: f32 = 96.0;
/// The icon's home (center): the left column of the mockup's grid.
pub const ICON_HOME: (f32, f32) = (119.0, 143.0);
/// The folder's center: the right column.
pub const FOLDER: (f32, f32) = (341.0, 143.0);
pub const FOLDER_W: f32 = 100.0;
pub const FOLDER_H: f32 = 80.0;
/// Where labels under the icon and the folder would start (none are
/// drawn: the hint says it all); the folder's click area reaches it.
pub const LABEL_Y: f32 = 199.0;
pub const PROGRESS_Y: f32 = 223.0;
pub const PROGRESS_W: f32 = 84.0;
/// Where the icon flies to (into the folder's mouth).
pub const INTO_FOLDER: (f32, f32) = (FOLDER.0, FOLDER.1 + 2.0);
/// How small the icon gets as it goes in (the mockup's scale(.4)).
pub const INTO_SCALE: f32 = 0.4;

pub const fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}

fn inside(r: &D2D_RECT_F, p: (f32, f32)) -> bool {
    p.0 >= r.left && p.0 < r.right && p.1 >= r.top && p.1 < r.bottom
}

/// The Windows-style close button.
pub const CLOSE: D2D_RECT_F = rect(W - 44.0, 0.0, W, STRIP);

/// The folder and its label: a click there installs.
pub fn folder_rect() -> D2D_RECT_F {
    rect(FOLDER.0 - 62.0, FOLDER.1 - FOLDER_H / 2.0 - 8.0, FOLDER.0 + 62.0, LABEL_Y + 22.0)
}

/// Where a dragged icon (its center) counts as dropped on the folder.
pub fn drop_zone() -> D2D_RECT_F {
    let r = folder_rect();
    rect(r.left - 24.0, r.top - 24.0, r.right + 24.0, r.bottom)
}

/// The icon at `center` (where a press starts a drag).
pub fn icon_rect(center: (f32, f32)) -> D2D_RECT_F {
    rect(center.0 - ICON / 2.0, center.1 - ICON / 2.0, center.0 + ICON / 2.0, center.1 + ICON / 2.0)
}

/// The failure message and its retry button.
pub const MESSAGE: D2D_RECT_F = rect(24.0, 224.0, W - 24.0, 244.0);
pub const RETRY: D2D_RECT_F = rect(W / 2.0 - 48.0, 250.0, W / 2.0 + 48.0, 272.0);
pub const HINT: D2D_RECT_F = rect(0.0, 248.0, W, 268.0);

// Hit tests take a window point `p` (DIP) and test it against the rect as
// drawn (on_screen): right to left, the mirrored rect, with the same
// half-open edges as it is drawn with.
pub fn hit_close(p: (f32, f32), rtl: bool) -> bool {
    inside(&on_screen(CLOSE, rtl), p)
}
pub fn hit_folder(p: (f32, f32), rtl: bool) -> bool {
    inside(&on_screen(folder_rect(), rtl), p)
}
/// A dragged icon's center (layout coordinates) over the folder.
pub fn hit_drop(center: (f32, f32)) -> bool {
    inside(&drop_zone(), center)
}
/// `center` is the icon's layout center (Scene::icon).
pub fn hit_icon(p: (f32, f32), center: (f32, f32), rtl: bool) -> bool {
    inside(&on_screen(icon_rect(center), rtl), p)
}
pub fn hit_retry(p: (f32, f32), rtl: bool) -> bool {
    inside(&on_screen(RETRY, rtl), p)
}
/// The title strip (drags the window) but its close button.
pub fn hit_caption(p: (f32, f32), rtl: bool) -> bool {
    p.1 >= 0.0 && p.1 < STRIP && !hit_close(p, rtl)
}

/// Where layout x `x` is drawn: mirrored across the window when `rtl`.
pub fn screen_x(x: f32, rtl: bool) -> f32 {
    if rtl { W - x } else { x }
}

/// Where layout rect `r` is drawn (see screen_x; its edges stay ordered).
pub fn on_screen(r: D2D_RECT_F, rtl: bool) -> D2D_RECT_F {
    if rtl { rect(W - r.right, r.top, W - r.left, r.bottom) } else { r }
}

/// The layout point under window point `p` (DIP): where a dragged icon
/// goes (not for hit tests: mirrored, a rect's edges would swap which one
/// is included). Its own inverse.
pub fn logical(p: (f32, f32), rtl: bool) -> (f32, f32) {
    (screen_x(p.0, rtl), p.1)
}

// ---------------------------------------------------------------- scene

/// What to draw this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene<'a> {
    pub icon: (f32, f32),
    pub icon_scale: f32,
    pub icon_alpha: f32,
    pub folder_scale: f32,
    pub progress: Option<f32>,
    pub hint: bool,
    pub message: Option<&'a str>,
    /// The keyboard focus ring (on the folder, or on Retry when it shows).
    pub focus: bool,
    pub close_hover: bool,
    pub close_enabled: bool,
    pub retry_hover: bool,
}

impl Scene<'_> {
    pub fn ready() -> Scene<'static> {
        Scene {
            icon: ICON_HOME,
            icon_scale: 1.0,
            icon_alpha: 1.0,
            folder_scale: 1.0,
            progress: None,
            hint: true,
            message: None,
            focus: false,
            close_hover: false,
            close_enabled: true,
            retry_hover: false,
        }
    }
}

// ---------------------------------------------------------------- the mark

/// The gnome mark (assets/brand/icon.svg), in its 1024 viewBox: the hat
/// (with the nose under its brim) and the beard, each in its own color.
pub const GNOME_HAT: &str = "M251 558 C238 552 246 534 256 512 C298 426 362 336 422 242 C486 144 560 98 636 98 C712 98 768 138 782 198 C796 248 772 280 748 281 C738 281 735 275 742 264 C758 238 718 190 678 190 C636 190 602 218 588 254 C576 294 596 330 618 362 C652 420 690 488 704 530 C710 544 712 552 702 558 C650 522 590 505 530 502 C528 534 512 549 482 549 C452 549 438 532 437 502 C370 505 300 524 251 558 Z";
pub const GNOME_BEARD: &str = "M264 590 C270 658 364 712 478 712 C592 712 686 658 694 590 C718 610 724 648 712 696 C684 778 608 854 508 914 C490 927 478 932 460 922 C366 874 276 794 248 720 C228 666 234 614 264 590 Z";
/// Each part's top and bottom in the box (its gradient's ends).
const HAT_SPAN: (f32, f32) = (98.0, 558.0);
const BEARD_SPAN: (f32, f32) = (590.0, 932.0);

/// A path of `M x y`, `C x1 y1 x2 y2 x y` (repeatable) and `Z`.
#[derive(Debug, Clone, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Cubic([f32; 6]),
    Close,
}

pub fn parse_path(d: &str) -> Option<Vec<Seg>> {
    let mut out = Vec::new();
    // Commands may touch their numbers ("M251 558").
    let spaced: String = d.chars().flat_map(|c| if c.is_ascii_alphabetic() { [' ', c, ' '] } else if c == ',' { [' ', ' ', ' '] } else { [c, ' ', ' '] }).filter(|&c| c != ' ').collect();
    let mut tokens = spaced.split_ascii_whitespace().peekable();
    let num = |t: &mut std::iter::Peekable<std::str::SplitAsciiWhitespace>| t.next()?.parse::<f32>().ok();
    while let Some(cmd) = tokens.next() {
        match cmd {
            "M" => out.push(Seg::Move(num(&mut tokens)?, num(&mut tokens)?)),
            "C" => loop {
                let mut c = [0.0; 6];
                for v in &mut c {
                    *v = num(&mut tokens)?;
                }
                out.push(Seg::Cubic(c));
                if !tokens.peek().is_some_and(|t| t.parse::<f32>().is_ok()) {
                    break;
                }
            },
            "Z" => out.push(Seg::Close),
            _ => return None,
        }
    }
    Some(out)
}

fn v(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn gnome_geometry(factory: &ID2D1Factory, d: &str) -> Result<ID2D1PathGeometry> {
    let segs = parse_path(d).expect("the gnome path parses");
    unsafe {
        let g = factory.CreatePathGeometry()?;
        let sink = g.Open()?;
        sink.SetFillMode(D2D1_FILL_MODE_WINDING);
        let mut open = false;
        for s in segs {
            match s {
                Seg::Move(x, y) => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    }
                    sink.BeginFigure(v(x, y), D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                }
                Seg::Cubic(c) => sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: v(c[0], c[1]), point2: v(c[2], c[3]), point3: v(c[4], c[5]) }),
                Seg::Close => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                }
            }
        }
        if open {
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        }
        sink.Close()?;
        Ok(g)
    }
}

// ---------------------------------------------------------------- resources

pub struct Resources {
    pub factory: ID2D1Factory,
    /// The language reads right to left: the picture is mirrored.
    pub rtl: bool,
    dw: IDWriteFactory,
    hint: IDWriteTextFormat,
    message: IDWriteTextFormat,
    button: IDWriteTextFormat,
    /// The gnome mark's hat and beard.
    gnome: (ID2D1PathGeometry, ID2D1PathGeometry),
    round: ID2D1StrokeStyle,
}

/// Hangul's blocks (jamo, compatibility jamo, extended jamo, syllables).
const HANGUL: [(u32, u32); 5] = [(0x1100, 0x11FF), (0x3130, 0x318F), (0xA960, 0xA97F), (0xAC00, 0xD7AF), (0xD7B0, 0xD7FF)];

impl Resources {
    pub fn new(factory: ID2D1Factory, lang: Lang) -> Result<Resources> {
        unsafe {
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let family = HSTRING::from(if has_family(&dw, "Segoe UI Variable Text") { "Segoe UI Variable Text" } else { "Segoe UI" });
            let locale = HSTRING::from(lang.locale());
            let rtl = lang.rtl();
            // Korean in Malgun Gothic (Windows' Korean UI font), the rest as
            // the system falls back.
            let fallback = (|| -> Result<_> {
                let f2: IDWriteFactory2 = dw.cast()?;
                let b = f2.CreateFontFallbackBuilder()?;
                if has_family(&dw, "Malgun Gothic") {
                    let ranges = HANGUL.map(|(first, last)| DWRITE_UNICODE_RANGE { first, last });
                    b.AddMapping(&ranges, &[w!("Malgun Gothic").as_ptr()], None::<&IDWriteFontCollection>, windows::core::PCWSTR::null(), windows::core::PCWSTR::null(), 1.0)?;
                }
                b.AddMappings(&f2.GetSystemFontFallback()?)?;
                b.CreateFontFallback()
            })()
            .ok();
            let format = |size: f32, weight: DWRITE_FONT_WEIGHT, wrap: bool| -> Result<IDWriteTextFormat> {
                let f = dw.CreateTextFormat(&family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, &locale)?;
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                f.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
                // Right to left: the words run in their order, punctuation
                // and Latin words ("Enter", "Windows") where they belong.
                f.SetReadingDirection(if rtl { DWRITE_READING_DIRECTION_RIGHT_TO_LEFT } else { DWRITE_READING_DIRECTION_LEFT_TO_RIGHT })?;
                if let (Some(fb), Ok(f1)) = (&fallback, f.cast::<IDWriteTextFormat1>()) {
                    f1.SetFontFallback(fb)?;
                }
                Ok(f)
            };
            let round = factory.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 10.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            )?;
            Ok(Resources {
                hint: format(11.0, DWRITE_FONT_WEIGHT_NORMAL, false)?,
                message: format(12.0, DWRITE_FONT_WEIGHT_NORMAL, false)?,
                button: format(12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, false)?,
                gnome: (gnome_geometry(&factory, GNOME_HAT)?, gnome_geometry(&factory, GNOME_BEARD)?),
                round,
                factory,
                rtl,
                dw,
            })
        }
    }
}

/// Which text format a string sets in (Resources::width).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Text {
    Hint,
    Message,
    Button,
}

impl Resources {
    /// How wide `s` sets in the failure message's format (DIP).
    pub fn message_width(&self, s: &str) -> f32 {
        self.width(s, Text::Message)
    }

    /// How wide `s` sets in format `t` (DIP).
    pub fn width(&self, s: &str, t: Text) -> f32 {
        let wide: Vec<u16> = s.encode_utf16().collect();
        let format = match t {
            Text::Hint => &self.hint,
            Text::Message => &self.message,
            Text::Button => &self.button,
        };
        unsafe {
            let mut m = Default::default();
            match self.dw.CreateTextLayout(&wide, format, 10_000.0, 100.0).and_then(|l| l.GetMetrics(&mut m)) {
                Ok(()) => m.widthIncludingTrailingWhitespace,
                Err(_) => f32::MAX,
            }
        }
    }
}

fn has_family(dw: &IDWriteFactory, name: &str) -> bool {
    unsafe {
        let mut fonts: Option<IDWriteFontCollection> = None;
        if dw.GetSystemFontCollection(&mut fonts, false).is_err() {
            return false;
        }
        let Some(fonts) = fonts else { return false };
        let (mut index, mut exists) = (0u32, BOOL(0));
        fonts.FindFamilyName(&HSTRING::from(name), &mut index, &mut exists).is_ok() && exists.as_bool()
    }
}

// ---------------------------------------------------------------- drawing

/// `s` around (cx, cy).
pub fn scale_at(s: f32, cx: f32, cy: f32) -> Matrix3x2 {
    Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: cx - s * cx, M32: cy - s * cy }
}

fn rr(r: D2D_RECT_F, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }
}

fn brush(rt: &ID2D1RenderTarget, c: Rgba) -> Result<ID2D1SolidColorBrush> {
    unsafe { rt.CreateSolidColorBrush(&c.d2d(), None) }
}

fn text(rt: &ID2D1RenderTarget, s: &str, format: &IDWriteTextFormat, r: D2D_RECT_F, c: Rgba) -> Result<()> {
    let wide: Vec<u16> = s.encode_utf16().collect();
    let b = brush(rt, c)?;
    unsafe { rt.DrawText(&wide, format, &r, &b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL) };
    Ok(())
}

/// Draws `scene` with its top-left at `base` (the window: identity),
/// mirrored when `res.rtl`.
pub fn draw(rt: &ID2D1RenderTarget, res: &Resources, pal: &Palette, s: &Strings, scene: &Scene, base: Matrix3x2) -> Result<()> {
    let rtl = res.rtl;
    let sx = |x: f32| screen_x(x, rtl);
    let place = |r: D2D_RECT_F| on_screen(r, rtl);
    unsafe {
        rt.SetTransform(&base);
        rt.FillRectangle(&rect(0.0, 0.0, W, H), &brush(rt, pal.bg)?);

        // Title strip: a Windows-style close (the taskbar names the window).
        rt.FillRectangle(&rect(0.0, 0.0, W, STRIP), &brush(rt, pal.strip)?);
        let close = place(CLOSE);
        if scene.close_hover && scene.close_enabled {
            rt.FillRectangle(&close, &brush(rt, pal.hover)?);
        }
        let x = brush(rt, if scene.close_hover && scene.close_enabled { pal.ink } else { pal.ink2 })?;
        if !scene.close_enabled {
            x.SetOpacity(0.35);
        }
        let (cx, cy) = ((close.left + close.right) / 2.0, STRIP / 2.0);
        rt.DrawLine(v(cx - 5.0, cy - 5.0), v(cx + 5.0, cy + 5.0), &x, 1.3, None);
        rt.DrawLine(v(cx + 5.0, cy - 5.0), v(cx - 5.0, cy + 5.0), &x, 1.3, None);

        // The arrow between the two (toward the folder).
        let ink2 = brush(rt, pal.ink2)?;
        let (ax, ay) = (W / 2.0, ICON_HOME.1);
        rt.DrawLine(v(sx(ax - 26.0), ay), v(sx(ax + 24.0), ay), &ink2, 1.4, &res.round);
        rt.DrawLine(v(sx(ax + 18.0), ay - 5.0), v(sx(ax + 24.0), ay), &ink2, 1.4, &res.round);
        rt.DrawLine(v(sx(ax + 18.0), ay + 5.0), v(sx(ax + 24.0), ay), &ink2, 1.4, &res.round);

        draw_folder(rt, res, pal, (sx(FOLDER.0), FOLDER.1), scene.folder_scale, base)?;
        rt.SetTransform(&base);

        if let Some(p) = scene.progress {
            let track = place(rect(FOLDER.0 - PROGRESS_W / 2.0, PROGRESS_Y, FOLDER.0 + PROGRESS_W / 2.0, PROGRESS_Y + 4.0));
            rt.FillRoundedRectangle(&rr(track, 2.0), &brush(rt, pal.track)?);
            if let Some(o) = pal.outline {
                rt.DrawRoundedRectangle(&rr(track, 2.0), &brush(rt, o)?, 1.0, None);
            }
            let p = p.clamp(0.0, 1.0);
            if p > 0.0 {
                // From the start of the reading direction.
                let len = PROGRESS_W * p.max(4.0 / PROGRESS_W);
                let fill = if rtl { rect(track.right - len, track.top, track.right, track.bottom) } else { rect(track.left, track.top, track.left + len, track.bottom) };
                rt.FillRoundedRectangle(&rr(fill, 2.0), &brush(rt, pal.fill)?);
            }
        }

        if scene.focus && scene.message.is_none() {
            let r = place(folder_rect());
            rt.DrawRoundedRectangle(&rr(rect(r.left, r.top, r.right, r.bottom - 2.0), 12.0), &brush(rt, pal.focus)?, 2.0, None);
        }

        if scene.hint {
            text(rt, s.hint, &res.hint, place(HINT), pal.ink2)?;
        }
        if let Some(m) = scene.message {
            text(rt, m, &res.message, place(MESSAGE), pal.ink)?;
            let retry = place(RETRY);
            let pill = rr(retry, 6.0);
            rt.FillRoundedRectangle(&pill, &brush(rt, if scene.retry_hover { pal.hover.alpha(pal.hover.a * 1.8) } else { pal.hover })?);
            if let Some(o) = pal.outline {
                rt.DrawRoundedRectangle(&pill, &brush(rt, o)?, 1.0, None);
            }
            text(rt, s.retry, &res.button, retry, pal.ink)?;
            if scene.focus {
                let f = rect(retry.left - 3.0, retry.top - 3.0, retry.right + 3.0, retry.bottom + 3.0);
                rt.DrawRoundedRectangle(&rr(f, 8.0), &brush(rt, pal.focus)?, 2.0, None);
            }
        }

        // The icon last: dragged, it passes over everything.
        if scene.icon_alpha > 0.005 {
            let a = scene.icon_alpha.clamp(0.0, 1.0);
            draw_icon(rt, res, pal, (sx(scene.icon.0), scene.icon.1), ICON * scene.icon_scale, a, base)?;
            rt.SetTransform(&base);
        }
    }
    Ok(())
}

/// The neutral folder (our own drawing): a tab, a back and a front with a
/// 2 x 2 grid, centered at `center`, scaled `scale` around it.
fn draw_folder(rt: &ID2D1RenderTarget, _res: &Resources, pal: &Palette, center: (f32, f32), scale: f32, base: Matrix3x2) -> Result<()> {
    unsafe {
        rt.SetTransform(&(scale_at(scale, center.0, center.1) * base));
        let (x0, y0) = (center.0 - FOLDER_W / 2.0, center.1 - FOLDER_H / 2.0);
        let back = brush(rt, pal.folder_back)?;
        let tab = rr(rect(x0 + 4.0, y0, x0 + 44.0, y0 + 14.0), 5.0);
        let body = rr(rect(x0, y0 + 8.0, x0 + FOLDER_W, y0 + FOLDER_H), 9.0);
        let front = rr(rect(x0, y0 + 20.0, x0 + FOLDER_W, y0 + FOLDER_H), 9.0);
        rt.FillRoundedRectangle(&tab, &back);
        rt.FillRoundedRectangle(&body, &back);
        if let Some(o) = pal.outline {
            let o = brush(rt, o)?;
            rt.DrawRoundedRectangle(&tab, &o, 1.0, None);
            rt.DrawRoundedRectangle(&body, &o, 1.0, None);
        }
        rt.FillRoundedRectangle(&front, &brush(rt, pal.folder_front)?);
        if let Some(o) = pal.outline {
            rt.DrawRoundedRectangle(&front, &brush(rt, o)?, 1.0, None);
        }
        // The grid: 26 DIP, the mockup's 24-unit glyph.
        let u = 26.0 / 24.0;
        let (gx, gy) = (center.0 - 13.0, y0 + 50.0 - 13.0);
        let glyph = brush(rt, pal.folder_glyph)?;
        for (x, y) in [(4.0, 4.0), (14.0, 4.0), (4.0, 14.0), (14.0, 14.0)] {
            let r = rect(gx + x * u, gy + y * u, gx + (x + 6.0) * u, gy + (y + 6.0) * u);
            rt.DrawRoundedRectangle(&rr(r, 1.5 * u), &glyph, 2.0 * u, None);
        }
    }
    Ok(())
}

/// The app icon (assets/brand/icon.svg's colors and mark; the plate the
/// setup mockup's rounded square): top-to-bottom gradient, the gnome mark
/// in its two colors; `size` DIP wide at `center`, opacity `a`.
fn draw_icon(rt: &ID2D1RenderTarget, res: &Resources, pal: &Palette, center: (f32, f32), size: f32, a: f32, base: Matrix3x2) -> Result<()> {
    unsafe {
        rt.SetTransform(&base);
        let (ox, oy) = (center.0 - size / 2.0, center.1 - size / 2.0);
        let body = rect(ox, oy, ox + size, oy + size);
        // The mockup's square: rx 208 on 896 (icon.svg before its squircle).
        let radius = size * 208.0 / 896.0;
        if pal.shadow.a > 0.0 {
            // A soft drop shadow (the mockup's 0 6px 16px): equal thin
            // layers from inside the icon (their edges hidden under it) out
            // to 14 DIP, so the darkness fades with no edge.
            let k = size / ICON;
            let layers = 14;
            let b = brush(rt, pal.shadow.alpha(pal.shadow.a * a * a * 1.25 / layers as f32))?;
            for i in 0..layers {
                let grow = (-6.0 + 20.0 * i as f32 / (layers - 1) as f32) * k;
                let r = rect(body.left - grow, body.top - grow + 6.0 * k, body.right + grow, body.bottom + grow + 6.0 * k);
                rt.FillRoundedRectangle(&rr(r, (radius + grow).max(1.0)), &b);
            }
        }
        let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: pal.icon_top.d2d() }, D2D1_GRADIENT_STOP { position: 1.0, color: pal.icon_bottom.d2d() }];
        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        let grad = rt.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: v(center.0, body.top), endPoint: v(center.0, body.bottom) }, None, &coll)?;
        grad.SetOpacity(a);
        rt.FillRoundedRectangle(&rr(body, radius), &grad);
        if let Some(o) = pal.outline {
            rt.DrawRoundedRectangle(&rr(body, radius), &brush(rt, o)?, 1.0, None);
        }
        // The mark at 0.7 of its 1024 box, centred like icon.svg's, on a
        // square that starts at 64 of that box.
        let k = size / 896.0;
        let m = Matrix3x2 { M11: 0.7 * k, M12: 0.0, M21: 0.0, M22: 0.7 * k, M31: ox + 89.6 * k, M32: oy + 89.6 * k };
        rt.SetTransform(&(m * base));
        // Each part in its gradient (top to bottom of its own box, in the
        // mark's box: the brush takes the same transform).
        for (geom, (top, bottom), (y0, y1)) in [(&res.gnome.0, pal.hat, HAT_SPAN), (&res.gnome.1, pal.beard, BEARD_SPAN)] {
            let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: top.d2d() }, D2D1_GRADIENT_STOP { position: 1.0, color: bottom.d2d() }];
            let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            let fill = rt.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: v(512.0, y0), endPoint: v(512.0, y1) }, None, &coll)?;
            fill.SetOpacity(a);
            rt.FillGeometry(geom, &fill, None);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gnome_path_parses() {
        let (hat, beard) = (parse_path(GNOME_HAT).unwrap(), parse_path(GNOME_BEARD).unwrap());
        for segs in [&hat, &beard] {
            assert_eq!(segs.iter().filter(|s| matches!(s, Seg::Move(..))).count(), 1);
            assert_eq!(segs.iter().filter(|s| matches!(s, Seg::Close)).count(), 1);
        }
        assert_eq!(hat.iter().chain(&beard).filter(|s| matches!(s, Seg::Cubic(..))).count(), 22);
        assert_eq!(hat[0], Seg::Move(251.0, 558.0));
        assert_eq!(beard[0], Seg::Move(264.0, 590.0));
        assert_eq!(parse_path("M 1 2 L 3 4"), None, "only M, C and Z");
        assert_eq!(parse_path("M1,2C3 4 5 6 7 8Z"), Some(vec![Seg::Move(1.0, 2.0), Seg::Cubic([3.0, 4.0, 5.0, 6.0, 7.0, 8.0]), Seg::Close]));
        assert_eq!(parse_path("M 1"), None);
        // icon.svg has the same mark.
        let svg = include_str!("../../../assets/brand/icon.svg");
        assert!(svg.contains(GNOME_HAT) && svg.contains(GNOME_BEARD));
    }

    #[test]
    fn layout_matches_the_mockup() {
        // Grid: 36 DIP padding, 1fr 56px 1fr.
        let col = (W - 72.0 - 56.0) / 2.0;
        assert_eq!(ICON_HOME.0, 36.0 + col / 2.0);
        assert_eq!(FOLDER.0, W - 36.0 - col / 2.0);
        assert_eq!(STRIP + (H - STRIP) / 2.0 - 12.0, ICON_HOME.1);
        // Nothing overlaps: label < progress < hint; message < retry.
        const { assert!(LABEL_Y + 18.0 <= PROGRESS_Y && PROGRESS_Y + 4.0 < HINT.top) };
        const { assert!(MESSAGE.bottom <= RETRY.top && RETRY.bottom <= H) };
        const { assert!(ICON_HOME.1 + ICON / 2.0 < LABEL_Y) };
    }

    #[test]
    fn hit_testing() {
        assert!(hit_close((W - 5.0, 5.0), false));
        assert!(!hit_close((W - 50.0, 5.0), false));
        assert!(hit_caption((W / 2.0, 10.0), false) && !hit_caption((W - 5.0, 10.0), false) && !hit_caption((W / 2.0, 40.0), false));
        assert!(hit_folder(FOLDER, false) && hit_folder((FOLDER.0, LABEL_Y + 8.0), false));
        assert!(!hit_folder(ICON_HOME, false));
        assert!(hit_icon(ICON_HOME, ICON_HOME, false) && !hit_icon(FOLDER, ICON_HOME, false));
        assert!(hit_drop(FOLDER) && hit_drop((FOLDER.0 - 80.0, FOLDER.1)), "a near miss still counts");
        assert!(!hit_drop((W / 2.0 - 20.0, FOLDER.1)), "the arrow is not the folder");
        assert!(hit_retry((W / 2.0, 260.0), false));
        let m = scale_at(2.0, 10.0, 10.0);
        assert_eq!((m.M31, m.M32), (-10.0, -10.0));
    }

    /// Right to left: what is drawn mirrored is hit mirrored. A click on
    /// the window's top left closes it; on the left, the folder installs;
    /// on the right, the icon is grabbed.
    #[test]
    fn right_to_left_hits_what_it_draws() {
        assert!(hit_close((5.0, 5.0), true) && !hit_close((W - 5.0, 5.0), true));
        assert!(hit_caption((W - 5.0, 10.0), true) && !hit_caption((5.0, 10.0), true));
        assert!(hit_folder((W - FOLDER.0, FOLDER.1), true) && !hit_folder(FOLDER, true));
        assert!(hit_icon((W - ICON_HOME.0, ICON_HOME.1), ICON_HOME, true) && !hit_icon(ICON_HOME, ICON_HOME, true));
        assert!(hit_retry((W / 2.0, 260.0), true));
        // A dragged icon: the layout point under the pointer.
        assert_eq!(logical((W - FOLDER.0, FOLDER.1), true), FOLDER);
        // Left to right nothing moves; twice is the identity.
        for p in [(0.0, 0.0), (17.5, 40.0), FOLDER, (W, H)] {
            assert_eq!(logical(p, false), p);
            assert_eq!(logical(logical(p, true), true), p);
        }
        // Each drawn rect is the mirror of the hit rect.
        for r in [CLOSE, folder_rect(), RETRY, MESSAGE, HINT, icon_rect(ICON_HOME)] {
            let m = on_screen(r, true);
            assert!(m.left <= m.right);
            assert_eq!((m.left, m.right, m.top, m.bottom), (W - r.right, W - r.left, r.top, r.bottom));
            assert_eq!(on_screen(r, false), r);
        }
        assert_eq!(screen_x(FOLDER.0, true), W - FOLDER.0);
        // The text rects are centered: mirrored, they stay where they are.
        for r in [MESSAGE, RETRY, HINT] {
            assert_eq!(on_screen(r, true), r);
        }
    }

    /// The edges: each click target is hit exactly where it is drawn, the
    /// drawn rect's left edge in and its right edge out, either direction.
    /// (Mirrored layout points once swapped them: right to left, x = 0 on
    /// the close button dragged the window and x = 44 closed it.)
    #[test]
    fn edges_are_hit_as_drawn_both_ways() {
        type Hit = fn((f32, f32), bool) -> bool;
        let icon: Hit = |p, rtl| hit_icon(p, ICON_HOME, rtl);
        let targets: [(&str, D2D_RECT_F, Hit); 4] = [("close", CLOSE, hit_close), ("folder", folder_rect(), hit_folder), ("retry", RETRY, hit_retry), ("icon", icon_rect(ICON_HOME), icon)];
        for rtl in [false, true] {
            for (name, r, hit) in targets {
                let m = on_screen(r, rtl);
                let y = (m.top + m.bottom) / 2.0;
                assert!(hit((m.left, y), rtl), "{name} rtl {rtl}: left edge {}", m.left);
                assert!(hit((m.right - 0.01, y), rtl), "{name} rtl {rtl}: just inside the right edge");
                assert!(!hit((m.right, y), rtl), "{name} rtl {rtl}: right edge {} is outside", m.right);
                assert!(!hit((m.left - 0.01, y), rtl), "{name} rtl {rtl}: just left of it");
            }
        }
        // The review's case: 96 DPI, right to left, the close button is
        // window x 0..44.
        assert!(hit_close((0.0, 15.0), true) && !hit_caption((0.0, 15.0), true));
        assert!(hit_close((43.9, 15.0), true));
        assert!(!hit_close((44.0, 15.0), true) && hit_caption((44.0, 15.0), true));
        // Left to right, the same at the right end.
        assert!(hit_close((W - 44.0, 15.0), false) && !hit_close((W - 44.01, 15.0), false));
        assert!(!hit_close((W, 15.0), false));
    }
}
