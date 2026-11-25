//! NopBar's own dock tiles in their finished look (design C, "restrained
//! color", chosen 2026-11-11; docs/design/icons/mockup.html, results.md):
//! a squircle plate lit from above (a top-down gradient, a light inner top
//! edge, a darker inner bottom edge) and on it a solid, shaded object per
//! icon (the trash can, a tray with a falling arrow, sheets of paper, ...)
//! in pale tints, each object with two drop shadows, a lit top rim and a
//! soft floor shadow.
//!
//! The pictures are the mockup's SVG drawings ported as they are (paths on
//! a 100 x 100 grid, the same palette, the same filter numbers). They are
//! drawn by Direct2D on a WARP device of the calling thread's own (the
//! icon thread; never the GPU, never the UI's device), read back as
//! premultiplied BGRA and kept per (size, icon, theme), so each size is
//! drawn once.
//!
//! Small sizes: 32 px or less leaves the fine details out (grooves, text
//! lines, creases, glints: the mockup's LOD); glyphs::tile switches to the
//! single-color line glyph at 20 px or less and in high contrast. If
//! Direct2D fails, `draw` says None and the caller draws the line tile.
//!
//! Colors are an exception to NopBar's achromatic rule allowed for its own
//! icons only (owner, 2026-11-11).

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::Mutex;

use windows::core::{Interface, Result};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_COMPOSITE_MODE_DESTINATION_OUT, D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_FILL_MODE_WINDING, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Brush, ID2D1Factory, ID2D1RenderTarget, ID2D1CommandList, ID2D1DeviceContext, ID2D1Factory1, ID2D1Geometry, ID2D1Image, ID2D1PathGeometry, CLSID_D2D12DAffineTransform,
    CLSID_D2D1ColorMatrix, CLSID_D2D1Composite, CLSID_D2D1Shadow, D2D1_2DAFFINETRANSFORM_PROP_TRANSFORM_MATRIX, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ARC_SEGMENT,
    D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_BITMAP_OPTIONS, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_CPU_READ, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_CAP_STYLE_FLAT, D2D1_CAP_STYLE_ROUND, D2D1_COLORMATRIX_PROP_COLOR_MATRIX, D2D1_COMPOSITE_PROP_MODE, D2D1_DASH_STYLE_SOLID,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LAYER_OPTIONS1_NONE,
    D2D1_LAYER_PARAMETERS1, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_LINE_JOIN_MITER, D2D1_LINE_JOIN_ROUND, D2D1_MAP_OPTIONS_READ, D2D1_PROPERTY_TYPE_ENUM,
    D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_MATRIX_3X2, D2D1_PROPERTY_TYPE_MATRIX_5X4, D2D1_PROPERTY_TYPE_VECTOR4, D2D1_QUADRATIC_BEZIER_SEGMENT,
    D2D1_SHADOW_OPTIMIZATION_QUALITY, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION, D2D1_SHADOW_PROP_COLOR, D2D1_SHADOW_PROP_OPTIMIZATION, D2D1_STROKE_STYLE_PROPERTIES,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
use windows::Win32::Graphics::Direct3D11::{D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION};
use windows::Win32::Graphics::Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIDevice};
use windows_numerics::{Matrix3x2, Vector2};

use crate::glyphs::Glyph;
use crate::icons::Icon;

/// At this many px a side or fewer the fine details are left out.
pub const LOD_MAX_PX: u32 = 32;

/// The pictures (the mockup's ART).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Art {
    Launchpad,
    Trash,
    TrashFull,
    Downloads,
    Documents,
    Pictures,
    Music,
    Videos,
    Desktop,
    Folder,
    Mark,
}

impl Art {
    #[cfg(test)]
    pub const ALL: [Art; 11] =
        [Art::Launchpad, Art::Trash, Art::TrashFull, Art::Downloads, Art::Documents, Art::Pictures, Art::Music, Art::Videos, Art::Desktop, Art::Folder, Art::Mark];

    /// The picture a glyph's tile shows (the glyphs the dock's own tiles
    /// use: icons::drawn_tile, launchpad::glyph).
    pub fn of(g: Glyph) -> Option<Art> {
        Some(match g {
            Glyph::Launchpad => Art::Launchpad,
            Glyph::Trash => Art::Trash,
            Glyph::TrashFull => Art::TrashFull,
            Glyph::Download => Art::Downloads,
            Glyph::Document => Art::Documents,
            Glyph::Image => Art::Pictures,
            Glyph::Music => Art::Music,
            Glyph::Play => Art::Videos,
            Glyph::Monitor => Art::Desktop,
            Glyph::Folder => Art::Folder,
            Glyph::Mark => Art::Mark,
            _ => return None,
        })
    }
}

// ---- palette (mockup PAL.C) ---------------------------------------------------------

type Pair = (u32, u32);

/// The mockup's PAL.C for one theme (PAL.A's greys with tints on top).
struct Pal {
    plate: Pair,
    hi: f32,
    lo: f32,
    metal: Pair,
    metal_e: u32,
    metal_hi: u32,
    metal_d: Pair,
    back: Pair,
    folder: Pair,
    deep: Pair,
    paper: Pair,
    /// Paper outline: black at this alpha (at LOD_MAX_PX or less: the
    /// second, so white paper still reads on the light plate).
    paper_e: f32,
    paper_e_small: f32,
    fold: Pair,
    crease: f32,
    line: u32,
    line_acc: u32,
    ink: Pair,
    /// Grooves: black, their glints white, at these alphas.
    groove: f32,
    groove_hi: f32,
    sky: Pair,
    hill: Pair,
    hill2: Pair,
    sun: u32,
    screen: Pair,
    arrow: Pair,
    note: Pair,
    hat: Pair,
    lp: [Pair; 8],
    sh_a: f32,
    sh_c: f32,
    floor: f32,
    shade: f32,
    edge: f32,
}

/// The Launchpad's tiles: one grey each, no hue (the mockup's eight
/// pastels read as a toy on the plate; owner 2026-11-12).
const LP_LIGHT: [Pair; 8] = [(0x8C8E93, 0x626469); 8];
const LP_DARK: [Pair; 8] = [(0xF4F4F5, 0xC6C7CB); 8];

const LIGHT: Pal = Pal {
    plate: (0xFFFFFF, 0xE3E3E6),
    hi: 0.95,
    lo: 0.09,
    metal: (0xC6CDD5, 0x8A949F),
    metal_e: 0x6E7884,
    metal_hi: 0xF5F6F7,
    metal_d: (0x7D8793, 0x5E6772),
    back: (0x8FAACB, 0x6D8BB1),
    folder: (0xB5CCE5, 0x87A6CB),
    deep: (0x2E2F32, 0x4B4C50),
    paper: (0xFFFFFF, 0xECECEE),
    paper_e: 0.16,
    paper_e_small: 0.34,
    fold: (0xE4E4E7, 0xCBCCCF),
    crease: 0.13,
    line: 0xC8C9CD,
    line_acc: 0x93AFD0,
    ink: (0x696A6F, 0x2F3033),
    groove: 0.30,
    groove_hi: 0.65,
    sky: (0xD5E5F3, 0xA9C6E3),
    hill: (0x87AD85, 0x618962),
    hill2: (0xB2CCA8, 0x94B48C),
    sun: 0xF2D38A,
    screen: (0x6F92BA, 0x3B5983),
    arrow: (0x82A8D7, 0x5480B8),
    note: (0xC9939F, 0xA26B7B),
    hat: (0xC97B6E, 0xA0534A),
    lp: LP_LIGHT,
    sh_a: 0.26,
    sh_c: 0.22,
    floor: 0.16,
    shade: 0.12,
    edge: 0.5,
};

const DARK: Pal = Pal {
    plate: (0x4D4D50, 0x252527),
    hi: 0.24,
    lo: 0.28,
    metal: (0xE2E8EE, 0x9DA7B2),
    metal_e: 0x7A8490,
    metal_hi: 0xFFFFFF,
    metal_d: (0xB3BCC6, 0x87909B),
    back: (0x93AED0, 0x7290B6),
    folder: (0xC2D6EC, 0x8FAED2),
    deep: (0x141416, 0x2C2C2F),
    paper: (0xFBFBFB, 0xD8D8DB),
    paper_e: 0.28,
    paper_e_small: 0.28,
    fold: (0xCDCDD0, 0xAFB0B4),
    crease: 0.15,
    line: 0xB3B4B8,
    line_acc: 0xA3BCDA,
    ink: (0xF8F8F9, 0xC6C7CA),
    groove: 0.30,
    groove_hi: 0.7,
    sky: (0xCFE0F0, 0x9FBCDB),
    hill: (0x7EA47D, 0x5A7F5B),
    hill2: (0xA9C49F, 0x8BAA84),
    sun: 0xF4D894,
    screen: (0x5E80AA, 0x2E4870),
    arrow: (0x9DBDE4, 0x6C95C9),
    note: (0xDAA7B2, 0xB5808E),
    hat: (0xD88A7C, 0xB0615A),
    lp: LP_DARK,
    sh_a: 0.5,
    sh_c: 0.42,
    floor: 0.38,
    shade: 0.18,
    edge: 0.35,
};

fn pal(light: bool) -> &'static Pal {
    if light {
        &LIGHT
    } else {
        &DARK
    }
}

/// A line glyph's color on the plate (tiles without a picture of their own:
/// the stand-in apps of the design pictures).
pub fn plate_ink(light: bool) -> u32 {
    if light {
        0x3A3B3F
    } else {
        0xF2F2F3
    }
}

const WHITE: u32 = 0xFFFFFF;
const BLACK: u32 = 0x000000;

fn col(c: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((c >> 16) & 0xFF) as f32 / 255.0, g: ((c >> 8) & 0xFF) as f32 / 255.0, b: (c & 0xFF) as f32 / 255.0, a }
}

// ---- shapes -------------------------------------------------------------------------

/// The plate: a superellipse (n = 5) on 0..100, the mockup's SQ.
pub fn squircle_path() -> String {
    const N: usize = 240;
    let mut d = String::new();
    for i in 0..N {
        let t = i as f64 / N as f64 * std::f64::consts::TAU;
        let (c, s) = (t.cos(), t.sin());
        let x = 50.0 + 50.0 * c.signum() * c.abs().powf(2.0 / 5.0);
        let y = 50.0 + 50.0 * s.signum() * s.abs().powf(2.0 / 5.0);
        d += &format!("{}{:.2} {:.2}", if i == 0 { 'M' } else { 'L' }, x, y);
    }
    d + "Z"
}

/// Whether (x, y) on the 100 grid is inside the plate.
#[cfg(test)]
pub fn in_squircle(x: f32, y: f32) -> bool {
    let (u, v) = (((x - 50.0) / 50.0).abs(), ((y - 50.0) / 50.0).abs());
    u.powi(5) + v.powi(5) <= 1.0
}

fn rect(x: f32, y: f32, w: f32, h: f32, rx: f32) -> String {
    if rx <= 0.0 {
        return format!("M{x} {y}H{}V{}H{x}Z", x + w, y + h);
    }
    let r = rx.min(w / 2.0).min(h / 2.0);
    format!(
        "M{} {y}H{}A{r} {r} 0 0 1 {} {}V{}A{r} {r} 0 0 1 {} {}H{}A{r} {r} 0 0 1 {x} {}V{}A{r} {r} 0 0 1 {} {y}Z",
        x + r,
        x + w - r,
        x + w,
        y + r,
        y + h - r,
        x + w - r,
        y + h,
        x + r,
        y + h - r,
        y + r,
        x + r
    )
}

fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> String {
    format!("M{} {cy}A{rx} {ry} 0 1 0 {} {cy}A{rx} {ry} 0 1 0 {} {cy}Z", cx - rx, cx + rx, cx - rx)
}

fn poly(pts: &[(f32, f32)]) -> String {
    let mut d = String::new();
    for (i, (x, y)) in pts.iter().enumerate() {
        d += &format!("{}{x} {y}", if i == 0 { 'M' } else { 'L' });
    }
    d + "Z"
}

fn rotate(deg: f32, cx: f32, cy: f32) -> Matrix3x2 {
    Matrix3x2::rotation_around(deg, Vector2 { X: cx, Y: cy })
}

/// What a shape is filled or stroked with.
#[derive(Clone, Debug)]
enum Fill {
    Solid(u32, f32),
    /// Top to bottom over the shape's box.
    V(Pair),
    /// Stops (offset, color, alpha) over the shape's box, top to bottom or
    /// (true) left to right.
    Stops(Vec<(f32, u32, f32)>, bool),
    /// Top to bottom from y0 to y1 in the shape's own units.
    User(f32, f32, Pair),
}

// ---- SVG path data ------------------------------------------------------------------

fn number(b: &[u8], i: &mut usize) -> Option<f32> {
    while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == b',') {
        *i += 1;
    }
    let start = *i;
    if *i < b.len() && (b[*i] == b'-' || b[*i] == b'+') {
        *i += 1;
    }
    let mut digits = 0;
    while *i < b.len() && b[*i].is_ascii_digit() {
        *i += 1;
        digits += 1;
    }
    if *i < b.len() && b[*i] == b'.' {
        *i += 1;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        *i = start;
        return None;
    }
    std::str::from_utf8(&b[start..*i]).ok()?.parse().ok()
}

/// One absolute path command (SVG M L H V C Q T A Z, the mockup's set).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Cubic([f32; 6]),
    Quad([f32; 4]),
    Arc { rx: f32, ry: f32, rot: f32, large: bool, sweep: bool, x: f32, y: f32 },
    Close,
}

/// Parses SVG path data, None if malformed.
fn parse(d: &str) -> Option<Vec<Seg>> {
    let b = d.as_bytes();
    let (mut i, mut cmd) = (0usize, 0u8);
    let (mut cur, mut start) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
    let mut last_q: Option<(f32, f32)> = None;
    let mut out = Vec::new();
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i].is_ascii_alphabetic() {
            cmd = b[i];
            i += 1;
            if cmd.eq_ignore_ascii_case(&b'z') {
                out.push(Seg::Close);
                cur = start;
                last_q = None;
                continue;
            }
        } else if cmd == 0 || cmd.eq_ignore_ascii_case(&b'z') {
            return None;
        }
        let rel = cmd.is_ascii_lowercase();
        let (bx, by) = if rel { cur } else { (0.0, 0.0) };
        let mut n = || number(b, &mut i);
        let mut q = None;
        match cmd.to_ascii_uppercase() {
            b'M' => {
                cur = (bx + n()?, by + n()?);
                start = cur;
                out.push(Seg::Move(cur.0, cur.1));
                cmd = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                cur = (bx + n()?, by + n()?);
                out.push(Seg::Line(cur.0, cur.1));
            }
            b'H' => {
                cur = (bx + n()?, cur.1);
                out.push(Seg::Line(cur.0, cur.1));
            }
            b'V' => {
                cur = (cur.0, by + n()?);
                out.push(Seg::Line(cur.0, cur.1));
            }
            b'C' => {
                let v = [bx + n()?, by + n()?, bx + n()?, by + n()?, bx + n()?, by + n()?];
                cur = (v[4], v[5]);
                out.push(Seg::Cubic(v));
            }
            b'Q' => {
                let v = [bx + n()?, by + n()?, bx + n()?, by + n()?];
                q = Some((v[0], v[1]));
                cur = (v[2], v[3]);
                out.push(Seg::Quad(v));
            }
            b'T' => {
                let c = last_q.map(|(x, y)| (2.0 * cur.0 - x, 2.0 * cur.1 - y)).unwrap_or(cur);
                let to = (bx + n()?, by + n()?);
                q = Some(c);
                cur = to;
                out.push(Seg::Quad([c.0, c.1, to.0, to.1]));
            }
            b'A' => {
                let (rx, ry, rot) = (n()?, n()?, n()?);
                let (large, sweep) = (n()? != 0.0, n()? != 0.0);
                cur = (bx + n()?, by + n()?);
                out.push(Seg::Arc { rx, ry, rot, large, sweep, x: cur.0, y: cur.1 });
            }
            _ => return None,
        }
        last_q = q;
    }
    (!out.is_empty()).then_some(out)
}

unsafe fn geometry(f: &ID2D1Factory1, d: &str) -> Result<ID2D1PathGeometry> {
    let v = |x: f32, y: f32| Vector2 { X: x, Y: y };
    let f: &ID2D1Factory = f;
    let segs = parse(d).ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_INVALIDARG))?;
    let geom = f.CreatePathGeometry()?;
    let sink = geom.Open()?;
    sink.SetFillMode(D2D1_FILL_MODE_WINDING);
    let (mut open, mut at, mut start) = (false, (0.0f32, 0.0f32), (0.0f32, 0.0f32));
    for s in segs {
        if !open && !matches!(s, Seg::Move(..) | Seg::Close) {
            sink.BeginFigure(v(at.0, at.1), D2D1_FIGURE_BEGIN_FILLED);
            open = true;
        }
        match s {
            Seg::Move(x, y) => {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                }
                sink.BeginFigure(v(x, y), D2D1_FIGURE_BEGIN_FILLED);
                open = true;
                at = (x, y);
                start = at;
            }
            Seg::Line(x, y) => {
                sink.AddLine(v(x, y));
                at = (x, y);
            }
            Seg::Cubic(c) => {
                sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: v(c[0], c[1]), point2: v(c[2], c[3]), point3: v(c[4], c[5]) });
                at = (c[4], c[5]);
            }
            Seg::Quad(c) => {
                sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: v(c[0], c[1]), point2: v(c[2], c[3]) });
                at = (c[2], c[3]);
            }
            Seg::Arc { rx, ry, rot, large, sweep, x, y } => {
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: v(x, y),
                    size: D2D_SIZE_F { width: rx, height: ry },
                    rotationAngle: rot,
                    sweepDirection: if sweep { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                    arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                });
                at = (x, y);
            }
            Seg::Close => {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    open = false;
                }
                at = start;
            }
        }
    }
    if open {
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
    }
    sink.Close()?;
    Ok(geom)
}

// ---- the pen ------------------------------------------------------------------------

/// Draws on the 100 grid of a tile `k` px per unit.
struct Pen<'a> {
    dc: &'a ID2D1DeviceContext,
    f: &'a ID2D1Factory1,
    k: f32,
    m: std::cell::Cell<Matrix3x2>,
    lod: bool,
    p: &'static Pal,
}

#[derive(Clone, Copy)]
enum Ends {
    /// Butt caps, miter joins (SVG's default).
    Plain,
    /// Round caps and joins.
    Round,
    /// Butt caps, round joins.
    RoundJoin,
}

impl Pen<'_> {
    fn world(&self) -> Matrix3x2 {
        self.m.get() * Matrix3x2::scale(self.k, self.k)
    }

    unsafe fn brush(&self, fill: &Fill, g: &ID2D1PathGeometry, op: f32) -> Result<ID2D1Brush> {
        let lin = |stops: &[(f32, u32, f32)], p0: Vector2, p1: Vector2| -> Result<ID2D1Brush> {
            let st: Vec<D2D1_GRADIENT_STOP> = stops.iter().map(|&(o, c, a)| D2D1_GRADIENT_STOP { position: o, color: col(c, a) }).collect();
            let rt: &ID2D1RenderTarget = self.dc;
            let coll = rt.CreateGradientStopCollection(&st, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            self.dc.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: p0, endPoint: p1 }, None, &coll)?.cast()
        };
        let bounds = || g.GetBounds(None);
        let b: ID2D1Brush = match fill {
            Fill::Solid(c, a) => self.dc.CreateSolidColorBrush(&col(*c, *a), None)?.cast()?,
            Fill::V((a, b)) => {
                let r = bounds()?;
                lin(&[(0.0, *a, 1.0), (1.0, *b, 1.0)], Vector2 { X: r.left, Y: r.top }, Vector2 { X: r.left, Y: r.bottom })?
            }
            Fill::Stops(s, across) => {
                let r = bounds()?;
                let end = if *across { Vector2 { X: r.right, Y: r.top } } else { Vector2 { X: r.left, Y: r.bottom } };
                lin(s, Vector2 { X: r.left, Y: r.top }, end)?
            }
            Fill::User(y0, y1, (a, b)) => lin(&[(0.0, *a, 1.0), (1.0, *b, 1.0)], Vector2 { X: 0.0, Y: *y0 }, Vector2 { X: 0.0, Y: *y1 })?,
        };
        b.SetOpacity(op);
        Ok(b)
    }

    unsafe fn fill_op(&self, d: &str, fill: &Fill, op: f32) -> Result<()> {
        let g = geometry(self.f, d)?;
        let b = self.brush(fill, &g, op)?;
        self.dc.SetTransform(&self.world());
        self.dc.FillGeometry(&g, &b, None);
        Ok(())
    }

    unsafe fn fill(&self, d: &str, fill: &Fill) -> Result<()> {
        self.fill_op(d, fill, 1.0)
    }

    unsafe fn stroke(&self, d: &str, fill: &Fill, w: f32, ends: Ends) -> Result<()> {
        let g = geometry(self.f, d)?;
        let b = self.brush(fill, &g, 1.0)?;
        let (cap, join) = match ends {
            Ends::Plain => (D2D1_CAP_STYLE_FLAT, D2D1_LINE_JOIN_MITER),
            Ends::Round => (D2D1_CAP_STYLE_ROUND, D2D1_LINE_JOIN_ROUND),
            Ends::RoundJoin => (D2D1_CAP_STYLE_FLAT, D2D1_LINE_JOIN_ROUND),
        };
        let f: &ID2D1Factory = self.f;
        let style = f.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES { startCap: cap, endCap: cap, dashCap: cap, lineJoin: join, miterLimit: 4.0, dashStyle: D2D1_DASH_STYLE_SOLID, dashOffset: 0.0 },
            None,
        )?;
        self.dc.SetTransform(&self.world());
        self.dc.DrawGeometry(&g, &b, w, &style);
        Ok(())
    }

    /// `f` drawn with `t` (an SVG group transform) on top of the current.
    unsafe fn with(&self, t: Matrix3x2, f: impl FnOnce() -> Result<()>) -> Result<()> {
        let old = self.m.get();
        self.m.set(t * old);
        let r = f();
        self.m.set(old);
        r
    }

    /// What `f` draws, recorded (device pixels).
    unsafe fn record(&self, f: impl FnOnce() -> Result<()>) -> Result<ID2D1CommandList> {
        let old = self.dc.GetTarget()?;
        let list = self.dc.CreateCommandList()?;
        self.dc.SetTarget(&list);
        let r = f();
        self.dc.SetTarget(&old);
        list.Close()?;
        r.map(|_| list)
    }

    /// `input`'s alpha blurred by `sigma` px in black at `a`, `dy` px down.
    unsafe fn shadow(&self, input: &ID2D1Image, sigma: f32, a: f32, dy: f32) -> Result<()> {
        let fx = self.dc.CreateEffect(&CLSID_D2D1Shadow)?;
        fx.SetInput(0, input, true);
        fx.SetValue(D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32, D2D1_PROPERTY_TYPE_FLOAT, &sigma.to_le_bytes())?;
        let v: Vec<u8> = [0.0f32, 0.0, 0.0, a].iter().flat_map(|k| k.to_le_bytes()).collect();
        fx.SetValue(D2D1_SHADOW_PROP_COLOR.0 as u32, D2D1_PROPERTY_TYPE_VECTOR4, &v)?;
        fx.SetValue(D2D1_SHADOW_PROP_OPTIMIZATION.0 as u32, D2D1_PROPERTY_TYPE_ENUM, &(D2D1_SHADOW_OPTIMIZATION_QUALITY.0 as u32).to_le_bytes())?;
        self.dc.SetTransform(&Matrix3x2::identity());
        self.dc.DrawImage(&fx.GetOutput()?, Some(&Vector2 { X: 0.0, Y: dy }), None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
        Ok(())
    }

    /// The mockup's "sh" filter on what `f` draws: a wide soft shadow and a
    /// close firm one under it, and a white rim on its top edges (its alpha
    /// less itself moved down) over it.
    unsafe fn shadowed(&self, f: impl FnOnce() -> Result<()>) -> Result<()> {
        let list = self.record(f)?;
        let img: ID2D1Image = list.cast()?;
        let (k, p) = (self.k, self.p);
        self.shadow(&img, 2.2 * k, p.sh_a, 2.4 * k)?;
        self.shadow(&img, 0.55 * k, p.sh_c, 0.7 * k)?;
        self.dc.SetTransform(&Matrix3x2::identity());
        self.dc.DrawImage(&img, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
        if p.edge > 0.0 {
            let white = self.dc.CreateEffect(&CLSID_D2D1ColorMatrix)?;
            white.SetInput(0, &img, true);
            #[rustfmt::skip]
            let m: [f32; 20] = [
                0.0, 0.0, 0.0, 0.0,
                0.0, 0.0, 0.0, 0.0,
                0.0, 0.0, 0.0, 0.0,
                0.0, 0.0, 0.0, p.edge,
                1.0, 1.0, 1.0, 0.0,
            ];
            let bytes: Vec<u8> = m.iter().flat_map(|v| v.to_le_bytes()).collect();
            white.SetValue(D2D1_COLORMATRIX_PROP_COLOR_MATRIX.0 as u32, D2D1_PROPERTY_TYPE_MATRIX_5X4, &bytes)?;
            let down = self.dc.CreateEffect(&CLSID_D2D12DAffineTransform)?;
            down.SetInput(0, &img, true);
            let t: Vec<u8> = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.9 * k].iter().flat_map(|v| v.to_le_bytes()).collect();
            down.SetValue(D2D1_2DAFFINETRANSFORM_PROP_TRANSFORM_MATRIX.0 as u32, D2D1_PROPERTY_TYPE_MATRIX_3X2, &t)?;
            // Composite: input 0 the destination, 1 the source.
            let rim = self.dc.CreateEffect(&CLSID_D2D1Composite)?;
            rim.SetInput(0, &white.GetOutput()?, true);
            rim.SetInput(1, &down.GetOutput()?, true);
            rim.SetValue(D2D1_COMPOSITE_PROP_MODE.0 as u32, D2D1_PROPERTY_TYPE_ENUM, &(D2D1_COMPOSITE_MODE_DESTINATION_OUT.0 as u32).to_le_bytes())?;
            self.dc.DrawImage(&rim.GetOutput()?, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
        }
        Ok(())
    }

    /// A soft shadow where the object stands (the mockup's floorShadow).
    unsafe fn floor(&self, cx: f32, cy: f32, rx: f32) -> Result<()> {
        let list = self.record(|| self.fill(&ellipse(cx, cy, rx, 2.4), &Fill::Solid(BLACK, 1.0)))?;
        self.shadow(&list.cast()?, 1.6 * self.k, self.p.floor, 0.0)
    }

    /// `f` clipped to `d`.
    unsafe fn clipped(&self, d: &str, f: impl FnOnce() -> Result<()>) -> Result<()> {
        let g = geometry(self.f, d)?;
        let mask: ID2D1Geometry = self.f.CreateTransformedGeometry(&g, &self.world())?.cast()?;
        self.dc.SetTransform(&Matrix3x2::identity());
        let params = D2D1_LAYER_PARAMETERS1 {
            contentBounds: D2D_RECT_F { left: -f32::MAX, top: -f32::MAX, right: f32::MAX, bottom: f32::MAX },
            geometricMask: ManuallyDrop::new(Some(mask)),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        };
        self.dc.PushLayer(&params, None);
        let _ = ManuallyDrop::into_inner(params.geometricMask);
        let r = f();
        self.dc.PopLayer();
        r
    }

    fn metal_h(&self) -> Fill {
        let p = self.p;
        Fill::Stops(vec![(0.0, p.metal_e, 1.0), (0.32, p.metal.0, 1.0), (0.5, p.metal_hi, 1.0), (0.66, p.metal.0, 1.0), (1.0, p.metal.1, 1.0)], true)
    }

    /// The paper's outline.
    fn paper_edge(&self) -> Fill {
        Fill::Solid(BLACK, if self.lod { self.p.paper_e_small } else { self.p.paper_e })
    }

    fn shade_v(&self) -> Fill {
        Fill::Stops(vec![(0.0, BLACK, 0.0), (0.45, BLACK, 0.0), (1.0, BLACK, self.p.shade)], false)
    }
}

// ---- the pictures (mockup ART) ------------------------------------------------------

/// The mockup's seeded random numbers (mulberry32), so the crumpled paper
/// is the same every time and the same as the mockup's.
fn rng(seed: u32) -> impl FnMut() -> f64 {
    let mut s = seed;
    move || {
        s = s.wrapping_add(0x6D2B_79F5);
        let mut t = (s ^ (s >> 15)).wrapping_mul(1 | s);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        (t ^ (t >> 14)) as f64 / 4_294_967_296.0
    }
}

unsafe fn crumple(pen: &Pen, cx: f32, cy: f32, r: f32, seed: u32) -> Result<()> {
    let p = pen.p;
    let mut rnd = rng(seed);
    let (cx, cy, r) = (cx as f64, cy as f64, r as f64);
    let n = 10;
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        let a = -std::f64::consts::FRAC_PI_2 + i as f64 * std::f64::consts::TAU / n as f64 + (rnd() - 0.5) * 0.38;
        let rr = r * (0.8 + rnd() * 0.3);
        pts.push((cx + a.cos() * rr, cy + a.sin() * rr * 0.9));
    }
    let f32s = |v: &[(f64, f64)]| -> Vec<(f32, f32)> { v.iter().map(|&(x, y)| (x as f32, y as f32)).collect() };
    let outline = poly(&f32s(&pts));
    pen.fill(&outline, &Fill::V(p.paper))?;
    pen.stroke(&outline, &pen.paper_edge(), 0.5, Ends::RoundJoin)?;
    let ip = (cx + (rnd() - 0.5) * r * 0.35, cy + (rnd() - 0.5) * r * 0.35);
    for i in 0..n {
        let (a, b) = (pts[i], pts[(i + 1) % n]);
        let (mx, my) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        // Light from the top left.
        let lit = (my - cy) / r * 0.8 + (mx - cx) / r * 0.35;
        let k = p.crease as f64 * (0.35 + lit + (rnd() - 0.5) * 0.7).max(0.0);
        if pen.lod && i % 3 != 0 {
            continue;
        }
        if k > 0.01 {
            pen.fill(&poly(&f32s(&[ip, a, b])), &Fill::Solid(BLACK, k.min(0.32) as f32))?;
        }
    }
    if !pen.lod {
        let q = f32s(&[pts[1], ip, pts[6]]);
        pen.stroke(&format!("M{} {}L{} {}L{} {}", q[0].0, q[0].1, q[1].0, q[1].1, q[2].0, q[2].1), &Fill::Solid(WHITE, 0.5), 0.5, Ends::RoundJoin)?;
    }
    Ok(())
}

unsafe fn grooves(pen: &Pen, top: f32, bot: f32) -> Result<()> {
    if pen.lod {
        return Ok(());
    }
    for x in [40.0f32, 50.0, 60.0] {
        let xb = 50.0 + (x - 50.0) * 0.84;
        pen.stroke(&format!("M{x} {top}L{xb} {bot}"), &Fill::Solid(BLACK, pen.p.groove), 2.6, Ends::Round)?;
        pen.stroke(&format!("M{} {}L{} {bot}", x + 1.7, top + 0.6, xb + 1.5), &Fill::Solid(WHITE, pen.p.groove_hi), 0.7, Ends::Round)?;
    }
    Ok(())
}

const HAT: &str = "M251 558 C238 552 246 534 256 512 C298 426 362 336 422 242 C486 144 560 98 636 98 C712 98 768 138 782 198 C796 248 772 280 748 281 C738 281 735 275 742 264 C758 238 718 190 678 190 C636 190 602 218 588 254 C576 294 596 330 618 362 C652 420 690 488 704 530 C710 544 712 552 702 558 C650 522 590 505 530 502 C528 534 512 549 482 549 C452 549 438 532 437 502 C370 505 300 524 251 558 Z";
const BEARD: &str = "M264 590 C270 658 364 712 478 712 C592 712 686 658 694 590 C718 610 724 648 712 696 C684 778 608 854 508 914 C490 927 478 932 460 922 C366 874 276 794 248 720 C228 666 234 614 264 590 Z";

unsafe fn art(pen: &Pen, a: Art) -> Result<()> {
    let p = pen.p;
    let lod = pen.lod;
    match a {
        Art::Launchpad => {
            let (k, o) = (70.0f32 / 24.0, 15.0f32);
            let sz = 4.4 * k;
            let mut i = 0;
            for (row, cols) in [(4.0f32, &[4.0f32, 10.0, 16.0][..]), (10.0, &[4.0, 10.0, 16.0][..]), (16.0, &[7.0, 13.0][..])] {
                for &x in cols {
                    let (cx, cy) = (o + (x + 2.0) * k, o + (row + 2.0) * k);
                    let (l, t) = (cx - sz / 2.0, cy - sz / 2.0);
                    pen.shadowed(|| pen.fill(&rect(l, t, sz, sz, sz * 0.26), &Fill::V(p.lp[i])))?;
                    if !lod {
                        pen.stroke(&format!("M{} {}H{}", l + sz * 0.26, t + 0.75, l + sz - sz * 0.26), &Fill::Solid(WHITE, 0.55), 0.8, Ends::Round)?;
                    }
                    i += 1;
                }
            }
        }
        Art::Trash => {
            let body = "M30 36 L70 36 L66.8 77.2 Q66.4 81.5 62 81.5 L38 81.5 Q33.6 81.5 33.2 77.2 Z";
            pen.floor(50.0, 82.5, 20.0)?;
            pen.shadowed(|| pen.fill(body, &pen.metal_h()))?;
            pen.fill(body, &pen.shade_v())?;
            grooves(pen, 44.0, 74.5)?;
            pen.shadowed(|| {
                pen.stroke("M42.6 28.5V25Q42.6 21.2 46.4 21.2H53.6Q57.4 21.2 57.4 25V28.5", &Fill::V(p.metal_d), 3.6, Ends::Round)?;
                pen.fill(&rect(24.5, 27.5, 51.0, 9.0, 4.5), &Fill::V(p.metal))
            })?;
            pen.stroke("M28.5 29H71.5", &Fill::Solid(p.metal_hi, 0.85), 0.9, Ends::Round)?;
        }
        Art::TrashFull => {
            let front = "M28.5 38 A21.5 4.6 0 0 0 71.5 38 L67.2 77.2 Q66.8 81.5 62.4 81.5 H37.6 Q33.2 81.5 32.8 77.2 Z";
            pen.floor(50.0, 82.5, 20.0)?;
            pen.shadowed(|| pen.fill(&ellipse(50.0, 38.0, 21.5, 4.6), &Fill::V(p.deep)))?;
            pen.stroke("M28.5 38 A21.5 4.6 0 0 1 71.5 38", &Fill::V(p.metal_d), 2.2, Ends::Plain)?;
            pen.with(rotate(17.0, 62.0, 31.0), || {
                pen.shadowed(|| {
                    let sheet = "M55 15.5H67L71 19.5V42H55Z";
                    pen.fill(sheet, &Fill::V(p.paper))?;
                    pen.stroke(sheet, &pen.paper_edge(), 0.5, Ends::Plain)?;
                    pen.fill("M67 15.5V19.5H71Z", &Fill::V(p.fold))?;
                    if !lod {
                        pen.fill("M55 15.5L71 30V42H55Z", &Fill::Solid(BLACK, p.crease * 0.6))?;
                        pen.fill(&rect(57.5, 23.0, 9.0, 1.6, 0.8), &Fill::Solid(p.line, 1.0))?;
                        pen.fill(&rect(57.5, 27.0, 11.0, 1.6, 0.8), &Fill::Solid(p.line, 1.0))?;
                    }
                    Ok(())
                })
            })?;
            pen.shadowed(|| crumple(pen, 39.5, 33.0, 10.0, 7))?;
            pen.shadowed(|| crumple(pen, 53.0, 37.0, 7.2, 21))?;
            pen.shadowed(|| pen.fill(front, &pen.metal_h()))?;
            pen.fill(front, &pen.shade_v())?;
            grooves(pen, 48.0, 74.5)?;
            pen.stroke("M28.5 38 A21.5 4.6 0 0 0 71.5 38", &Fill::Solid(p.metal_hi, 1.0), 1.6, Ends::Round)?;
        }
        Art::Downloads => {
            let front = "M21.5 60H78.5V71Q78.5 78 71.5 78H28.5Q21.5 78 21.5 71Z";
            pen.floor(50.0, 79.0, 24.0)?;
            pen.fill("M25.5 55H74.5L78.5 60H21.5Z", &Fill::V(p.deep))?;
            pen.shadowed(|| {
                pen.fill(
                    "M44 20Q44 17 47 17H53Q56 17 56 20V41H65.5Q69 41 66.8 43.6L52.3 61Q50 63.6 47.7 61L33.2 43.6Q31 41 34.5 41H44Z",
                    &Fill::V(p.arrow),
                )
            })?;
            if !lod {
                pen.stroke("M45.6 19.5V42", &Fill::Solid(WHITE, 0.35), 1.0, Ends::Round)?;
            }
            pen.shadowed(|| pen.fill(front, &Fill::V(p.metal)))?;
            pen.fill(front, &pen.shade_v())?;
            pen.stroke("M22.5 60.7H77.5", &Fill::Solid(p.metal_hi, 0.9), 0.9, Ends::Round)?;
            if !lod {
                pen.fill(&rect(40.0, 66.5, 20.0, 3.0, 1.5), &Fill::Solid(BLACK, p.groove))?;
            }
        }
        Art::Documents => {
            let page = "M35 18H58L71 31V77Q71 81 67 81H35Q31 81 31 77V22Q31 18 35 18Z";
            pen.floor(51.0, 82.0, 20.0)?;
            pen.with(rotate(-8.0, 50.0, 50.0), || pen.shadowed(|| pen.fill(&rect(30.0, 20.0, 40.0, 60.0, 3.5), &Fill::V(p.fold))))?;
            pen.shadowed(|| {
                pen.fill(page, &Fill::V(p.paper))?;
                pen.stroke(page, &pen.paper_edge(), 0.5, Ends::Plain)
            })?;
            pen.shadowed(|| pen.fill("M58 18V28Q58 31 61 31H71Z", &Fill::V(p.fold)))?;
            if lod {
                pen.fill(&rect(37.0, 41.0, 27.0, 4.0, 2.0), &Fill::Solid(p.line_acc, 1.0))?;
                pen.fill(&rect(37.0, 52.0, 27.0, 4.0, 2.0), &Fill::Solid(p.line, 1.0))?;
                pen.fill(&rect(37.0, 63.0, 18.0, 4.0, 2.0), &Fill::Solid(p.line, 1.0))?;
            } else {
                pen.fill(&rect(37.0, 40.0, 20.0, 3.2, 1.6), &Fill::Solid(p.line_acc, 1.0))?;
                for (i, y) in [47.0f32, 53.0, 59.0, 65.0].into_iter().enumerate() {
                    pen.fill(&rect(37.0, y, if i == 3 { 17.0 } else { 27.0 }, 2.6, 1.3), &Fill::Solid(p.line, 1.0))?;
                }
            }
        }
        Art::Pictures => {
            pen.floor(51.0, 81.0, 22.0)?;
            pen.with(rotate(-9.0, 48.0, 50.0), || {
                pen.shadowed(|| {
                    let card = rect(24.0, 26.0, 46.0, 40.0, 2.5);
                    pen.fill(&card, &Fill::V(p.paper))?;
                    pen.stroke(&card, &pen.paper_edge(), 0.5, Ends::Plain)?;
                    if !lod {
                        pen.fill_op(&rect(28.0, 30.0, 38.0, 26.0, 1.2), &Fill::V(p.sky), 0.55)?;
                    }
                    Ok(())
                })
            })?;
            pen.with(rotate(6.0, 54.0, 52.0), || {
                pen.shadowed(|| {
                    let card = rect(30.0, 34.0, 48.0, 41.0, 2.5);
                    pen.fill(&card, &Fill::V(p.paper))?;
                    pen.stroke(&card, &pen.paper_edge(), 0.5, Ends::Plain)
                })?;
                pen.clipped(&rect(34.0, 38.0, 40.0, 28.0, 1.2), || {
                    pen.fill(&rect(34.0, 38.0, 40.0, 28.0, 0.0), &Fill::V(p.sky))?;
                    pen.fill(&ellipse(45.0, 45.5, 3.6, 3.6), &Fill::Solid(p.sun, 1.0))?;
                    pen.fill("M34 66L52 47.5L64 58L70 52.5L76 58V66Z", &Fill::V(p.hill2))?;
                    pen.fill("M34 66V61Q45 53.5 56 60.5T76 60V66Z", &Fill::V(p.hill))
                })
            })?;
        }
        Art::Music => {
            pen.floor(52.0, 80.0, 20.0)?;
            let note = Fill::User(22.0, 78.0, p.note);
            pen.shadowed(|| {
                pen.with(rotate(-22.0, 36.0, 69.0), || pen.fill(&ellipse(36.0, 69.0, 8.8, 6.5), &note))?;
                pen.with(rotate(-22.0, 64.0, 62.5), || pen.fill(&ellipse(64.0, 62.5, 8.8, 6.5), &note))?;
                pen.fill("M40.4 68V30.2L72.6 23.5V62H68.4V33L44.6 38.1V68Z", &note)
            })?;
            if !lod {
                pen.stroke("M42 30.6L71 24.6", &Fill::Solid(WHITE, 0.45), 0.9, Ends::Round)?;
            }
        }
        Art::Videos => {
            let body = rect(17.5, 27.0, 65.0, 46.0, 6.0);
            pen.floor(50.0, 76.0, 26.0)?;
            pen.shadowed(|| pen.fill(&body, &Fill::V(p.metal)))?;
            pen.fill(&body, &pen.shade_v())?;
            pen.stroke("M22 27.7H78", &Fill::Solid(p.metal_hi, 0.85), 0.9, Ends::Round)?;
            for i in 0..6 {
                let x = 23.2 + i as f32 * 9.4;
                pen.fill(&rect(x, 30.4, 4.6, 3.4, 1.0), &Fill::V(p.deep))?;
                pen.fill(&rect(x, 66.2, 4.6, 3.4, 1.0), &Fill::V(p.deep))?;
            }
            pen.fill(&rect(22.5, 37.0, 55.0, 26.0, 2.2), &Fill::V(p.screen))?;
            pen.shadowed(|| pen.fill("M45 44Q45 41.4 47.2 42.6L58.6 48.7Q60.8 50 58.6 51.3L47.2 57.4Q45 58.6 45 56Z", &Fill::V(p.paper)))?;
        }
        Art::Desktop => {
            pen.floor(50.0, 80.5, 19.0)?;
            pen.fill("M44.5 66H55.5L57.5 76.5H42.5Z", &pen.metal_h())?;
            pen.shadowed(|| pen.fill(&rect(33.0, 75.5, 34.0, 4.6, 2.3), &Fill::V(p.metal)))?;
            pen.shadowed(|| pen.fill(&rect(15.0, 20.0, 70.0, 47.5, 4.6), &Fill::V(p.metal)))?;
            pen.stroke("M19 20.8H81", &Fill::Solid(p.metal_hi, 0.85), 0.9, Ends::Round)?;
            pen.fill(&rect(18.6, 23.6, 62.8, 38.0, 2.0), &Fill::V(p.screen))?;
            if !lod {
                pen.fill("M18.6 23.6H54L36 61.6H18.6Z", &Fill::Solid(WHITE, 0.07))?;
                pen.fill(&rect(38.0, 55.4, 24.0, 3.4, 1.7), &Fill::Solid(WHITE, 0.38))?;
            }
        }
        Art::Folder => {
            let front = "M17.5 42Q17.5 38.5 21 38.5H79Q82.5 38.5 82.5 42L81 73Q80.8 78 76 78H24Q19.2 78 19 73Z";
            pen.floor(50.0, 79.0, 26.0)?;
            pen.shadowed(|| pen.fill("M19 31Q19 26 24 26H39.5Q42 26 43.6 27.9L46.4 31.2H76Q81 31.2 81 36V72Q81 77 76 77H24Q19 77 19 72Z", &Fill::V(p.back)))?;
            if !lod {
                pen.fill(&rect(24.0, 33.0, 52.0, 14.0, 1.5), &Fill::V(p.paper))?;
            }
            pen.shadowed(|| pen.fill(front, &Fill::V(p.folder)))?;
            pen.fill(front, &pen.shade_v())?;
            pen.stroke("M21 39.5H79", &Fill::Solid(WHITE, 0.55), 0.9, Ends::Round)?;
        }
        Art::Mark => {
            let s = 57.0f32 / 834.0;
            let (tx, ty) = (50.0 - 512.0 * s, 50.0 - 515.0 * s);
            pen.shadowed(|| {
                pen.with(Matrix3x2::scale(s, s) * Matrix3x2::translation(tx, ty), || {
                    pen.fill(HAT, &Fill::V(p.hat))?;
                    pen.fill(BEARD, &Fill::V(p.ink))
                })
            })?;
        }
    }
    Ok(())
}

// ---- Flat's pictures ----------------------------------------------------------------

/// Flat's tiles (the owner's pick, 2026-11-12: docs/superpowers/plans/
/// 2026-11-12-flat-glass-distinct.md): one matte plate and the same
/// objects in two flat greys, details cut out in the plate's color. No
/// gradient, no shadow, no light; the gnome's hat keeps the brand's brick.
struct FlatPal {
    plate: u32,
    /// The object's main grey.
    a: u32,
    /// Its second grey (what stands behind, a second part).
    b: u32,
    hat: u32,
}

const FLAT_DARK: FlatPal = FlatPal { plate: 0x3F3F3F, a: 0xEDEDED, b: 0x9E9E9E, hat: 0xD88A7C };
const FLAT_LIGHT: FlatPal = FlatPal { plate: 0xFFFFFF, a: 0x464646, b: 0xA8A8A8, hat: 0xC97B6E };

fn flat_pal(light: bool) -> &'static FlatPal {
    if light {
        &FLAT_LIGHT
    } else {
        &FLAT_DARK
    }
}

unsafe fn flat_art(pen: &Pen, a: Art, fp: &FlatPal) -> Result<()> {
    let s = |c: u32| Fill::Solid(c, 1.0);
    let (pa, pb, cut) = (s(fp.a), s(fp.b), s(fp.plate));
    let lod = pen.lod;
    match a {
        Art::Launchpad => {
            let (k, o) = (70.0f32 / 24.0, 15.0f32);
            let sz = 4.4 * k;
            for (row, cols) in [(4.0f32, &[4.0f32, 10.0, 16.0][..]), (10.0, &[4.0, 10.0, 16.0][..]), (16.0, &[7.0, 13.0][..])] {
                for &x in cols {
                    let (cx, cy) = (o + (x + 2.0) * k, o + (row + 2.0) * k);
                    pen.fill(&rect(cx - sz / 2.0, cy - sz / 2.0, sz, sz, sz * 0.26), &pa)?;
                }
            }
        }
        Art::Trash => {
            pen.fill("M30 38.5L70 38.5L66.8 77.2Q66.4 81.5 62 81.5L38 81.5Q33.6 81.5 33.2 77.2Z", &pb)?;
            flat_grooves(pen, 45.0, &cut)?;
            pen.stroke("M42.6 28.5V25Q42.6 21.2 46.4 21.2H53.6Q57.4 21.2 57.4 25V28.5", &pa, 3.6, Ends::Round)?;
            pen.fill(&rect(24.5, 27.5, 51.0, 9.0, 4.5), &pa)?;
        }
        Art::TrashFull => {
            // A sheet and two crumples over the rim (no lid).
            pen.with(rotate(17.0, 62.0, 31.0), || pen.fill("M55 15.5H67L71 19.5V42H55Z", &pa))?;
            for (cx, cy, r, seed) in [(39.5f64, 33.0f64, 10.0f64, 7u32), (53.0, 37.0, 7.2, 21)] {
                let mut rnd = rng(seed);
                let pts: Vec<(f32, f32)> = (0..10)
                    .map(|i| {
                        let t = -std::f64::consts::FRAC_PI_2 + i as f64 * std::f64::consts::TAU / 10.0 + (rnd() - 0.5) * 0.38;
                        let rr = r * (0.8 + rnd() * 0.3);
                        ((cx + t.cos() * rr) as f32, (cy + t.sin() * rr * 0.9) as f32)
                    })
                    .collect();
                pen.fill(&poly(&pts), &pa)?;
            }
            pen.fill("M28.5 38A21.5 4.6 0 0 0 71.5 38L67.2 77.2Q66.8 81.5 62.4 81.5H37.6Q33.2 81.5 32.8 77.2Z", &pb)?;
            flat_grooves(pen, 48.0, &cut)?;
        }
        Art::Downloads => {
            pen.fill("M44 20Q44 17 47 17H53Q56 17 56 20V41H65.5Q69 41 66.8 43.6L52.3 61Q50 63.6 47.7 61L33.2 43.6Q31 41 34.5 41H44Z", &pa)?;
            // The tray, notched where the arrow's tip comes down.
            pen.fill("M21.5 62H39L43 68H57L61 62H78.5V71Q78.5 78 71.5 78H28.5Q21.5 78 21.5 71Z", &pb)?;
        }
        Art::Documents => {
            pen.with(rotate(-8.0, 50.0, 50.0), || pen.fill(&rect(30.0, 20.0, 40.0, 60.0, 3.5), &pb))?;
            pen.fill("M35 18H58L71 31V77Q71 81 67 81H35Q31 81 31 77V22Q31 18 35 18Z", &pa)?;
            pen.fill("M58 18V28Q58 31 61 31H71Z", &pb)?;
            if lod {
                for (y, w) in [(41.0f32, 27.0f32), (52.0, 27.0), (63.0, 18.0)] {
                    pen.fill(&rect(37.0, y, w, 4.0, 2.0), &cut)?;
                }
            } else {
                pen.fill(&rect(37.0, 40.0, 20.0, 3.2, 1.6), &cut)?;
                for (i, y) in [47.0f32, 53.0, 59.0, 65.0].into_iter().enumerate() {
                    pen.fill(&rect(37.0, y, if i == 3 { 17.0 } else { 27.0 }, 2.6, 1.3), &cut)?;
                }
            }
        }
        Art::Pictures => {
            pen.with(rotate(-9.0, 48.0, 50.0), || pen.fill(&rect(24.0, 26.0, 46.0, 40.0, 2.5), &pb))?;
            pen.with(rotate(6.0, 54.0, 52.0), || {
                pen.fill(&rect(30.0, 34.0, 48.0, 41.0, 2.5), &pa)?;
                pen.fill(&rect(34.0, 38.0, 40.0, 28.0, 1.2), &cut)?;
                pen.clipped(&rect(34.0, 38.0, 40.0, 28.0, 1.2), || {
                    pen.fill(&ellipse(45.0, 45.5, 3.6, 3.6), &pb)?;
                    pen.fill("M34 66L52 47.5L64 58L70 52.5L76 58V66Z", &pb)?;
                    pen.fill("M34 66V61Q45 53.5 56 60.5T76 60V66Z", &pa)
                })
            })?;
        }
        Art::Music => {
            pen.with(rotate(-22.0, 36.0, 69.0), || pen.fill(&ellipse(36.0, 69.0, 8.8, 6.5), &pa))?;
            pen.with(rotate(-22.0, 64.0, 62.5), || pen.fill(&ellipse(64.0, 62.5, 8.8, 6.5), &pa))?;
            pen.fill("M40.4 68V30.2L72.6 23.5V62H68.4V33L44.6 38.1V68Z", &pa)?;
        }
        Art::Videos => {
            pen.fill(&rect(17.5, 27.0, 65.0, 46.0, 6.0), &pa)?;
            for i in 0..6 {
                let x = 23.2 + i as f32 * 9.4;
                pen.fill(&rect(x, 30.4, 4.6, 3.4, 1.0), &cut)?;
                pen.fill(&rect(x, 66.2, 4.6, 3.4, 1.0), &cut)?;
            }
            pen.fill(&rect(22.5, 37.0, 55.0, 26.0, 2.2), &pb)?;
            pen.fill("M45 44Q45 41.4 47.2 42.6L58.6 48.7Q60.8 50 58.6 51.3L47.2 57.4Q45 58.6 45 56Z", &pa)?;
        }
        Art::Desktop => {
            pen.fill("M44.5 66H55.5L57.5 76.5H42.5Z", &pb)?;
            pen.fill(&rect(33.0, 75.5, 34.0, 4.6, 2.3), &pb)?;
            pen.fill(&rect(15.0, 20.0, 70.0, 47.5, 4.6), &pa)?;
            pen.fill(&rect(18.6, 23.6, 62.8, 38.0, 2.0), &pb)?;
        }
        Art::Folder => {
            pen.fill("M19 31Q19 26 24 26H39.5Q42 26 43.6 27.9L46.4 31.2H76Q81 31.2 81 36V72Q81 77 76 77H24Q19 77 19 72Z", &pb)?;
            pen.fill("M17.5 42Q17.5 38.5 21 38.5H79Q82.5 38.5 82.5 42L81 73Q80.8 78 76 78H24Q19.2 78 19 73Z", &pa)?;
        }
        Art::Mark => {
            let k = 57.0f32 / 834.0;
            let (tx, ty) = (50.0 - 512.0 * k, 50.0 - 515.0 * k);
            pen.with(Matrix3x2::scale(k, k) * Matrix3x2::translation(tx, ty), || {
                pen.fill(HAT, &s(fp.hat))?;
                pen.fill(BEARD, &pa)
            })?;
        }
    }
    Ok(())
}

/// The trash's three grooves cut into its body in the plate's color.
unsafe fn flat_grooves(pen: &Pen, top: f32, cut: &Fill) -> Result<()> {
    if pen.lod {
        return Ok(());
    }
    for x in [40.0f32, 50.0, 60.0] {
        let xb = 50.0 + (x - 50.0) * 0.84;
        pen.stroke(&format!("M{x} {top}L{xb} 74.5"), cut, 2.6, Ends::Round)?;
    }
    Ok(())
}

/// What goes on the plate: a picture, or (no picture for it) a line glyph
/// in the plate's ink, shadowed like the pictures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Content {
    Art(Art),
    Line(Glyph),
}

unsafe fn paint(dc: &ID2D1DeviceContext, f: &ID2D1Factory1, n: u32, what: Content, light: bool, flat: bool) -> Result<()> {
    let p = pal(light);
    let pen = Pen { dc, f, k: n as f32 / 100.0, m: std::cell::Cell::new(Matrix3x2::identity()), lod: n <= LOD_MAX_PX, p };
    let sq = squircle_path();
    if flat {
        // Flat: one matte plate, the picture in flat greys.
        let fp = flat_pal(light);
        pen.fill(&sq, &Fill::Solid(fp.plate, 1.0))?;
        return pen.clipped(&sq, || match what {
            Content::Art(a) => flat_art(&pen, a, fp),
            Content::Line(g) => {
                let s = n as f32;
                let fr = crate::glyphs::frame(s / 2.0, s / 2.0, s * crate::glyphs::TILE_GLYPH, 1.2);
                let rt = dc.cast()?;
                dc.SetTransform(&Matrix3x2::identity());
                crate::glyphs::draw_device(&rt, g, &fr, col(fp.a, 1.0))
            }
        });
    }
    // The plate: lit from above.
    pen.fill(&sq, &Fill::V(p.plate))?;
    pen.clipped(&sq, || {
        pen.stroke(&sq, &Fill::Stops(vec![(0.0, WHITE, p.hi), (0.28, WHITE, 0.0)], false), 2.6, Ends::Plain)?;
        pen.stroke(&sq, &Fill::Stops(vec![(0.62, BLACK, 0.0), (1.0, BLACK, p.lo)], false), 2.6, Ends::Plain)?;
        match what {
            Content::Art(a) => art(&pen, a),
            Content::Line(g) => {
                let s = n as f32;
                let fr = crate::glyphs::frame(s / 2.0, s / 2.0, s * crate::glyphs::TILE_GLYPH, 1.2);
                let rt = dc.cast()?;
                pen.shadowed(|| {
                    dc.SetTransform(&Matrix3x2::identity());
                    crate::glyphs::draw_device(&rt, g, &fr, col(plate_ink(light), 1.0))
                })
            }
        }
    })
}

// ---- device, drawing, cache ---------------------------------------------------------

struct Device {
    f: ID2D1Factory1,
    dc: ID2D1DeviceContext,
}

thread_local! {
    /// This thread's WARP device: made on first use, dropped after a
    /// failure (made again next time).
    static DEVICE: RefCell<Option<Device>> = const { RefCell::new(None) };
}

unsafe fn make_device() -> Result<Device> {
    let mut d3d = None;
    D3D11CreateDevice(None, D3D_DRIVER_TYPE_WARP, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut d3d), None, None)?;
    let dxgi: IDXGIDevice = d3d.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?.cast()?;
    let f: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let dc = f.CreateDevice(&dxgi)?.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
    Ok(Device { f, dc })
}

unsafe fn render(d: &Device, n: u32, what: Content, light: bool, flat: bool) -> Result<Vec<u8>> {
    let dc = &d.dc;
    let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
    let size = D2D_SIZE_U { width: n, height: n };
    let props = |o: D2D1_BITMAP_OPTIONS| D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: o, ..Default::default() };
    let target = dc.CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_TARGET))?;
    dc.SetTarget(&target);
    dc.SetDpi(96.0, 96.0);
    dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
    dc.BeginDraw();
    dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
    let drawn = paint(dc, &d.f, n, what, light, flat);
    dc.SetTransform(&Matrix3x2::identity());
    let ended = dc.EndDraw(None, None);
    dc.SetTarget(None::<&ID2D1Image>);
    drawn?;
    ended?;
    let read = dc.CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW))?;
    read.CopyFromBitmap(None, &target, None)?;
    let m = read.Map(D2D1_MAP_OPTIONS_READ)?;
    let mut out = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        out.extend_from_slice(std::slice::from_raw_parts(m.bits.add((y * m.pitch) as usize), (n * 4) as usize));
    }
    read.Unmap()?;
    Ok(out)
}

/// What a drawn tile is kept by: (side, content, light, flat).
type Key = (u32, Content, bool, bool);

/// Drawn tiles by Key: each size drawn once. Small (a few sizes of a dozen
/// tiles); emptied if it ever grows past `max`.
struct Cache {
    map: Mutex<Option<HashMap<Key, std::sync::Arc<Vec<u8>>>>>,
    max: usize,
}

impl Cache {
    const fn new(max: usize) -> Self {
        Cache { map: Mutex::new(None), max }
    }

    /// The pixels kept for `key`, else what `render` draws (kept when it
    /// draws; None: nothing kept, asked again next time).
    fn get_or_draw(&self, key: Key, render: impl FnOnce() -> Option<Vec<u8>>) -> Option<Vec<u8>> {
        if let Some(px) = self.map.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|c| c.get(&key).cloned()) {
            return Some((*px).clone());
        }
        // Drawn unlocked: another thread may draw the same tile meanwhile
        // (the same pixels; one of them is kept).
        let px = render()?;
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let c = map.get_or_insert_with(HashMap::new);
        if c.len() >= self.max {
            c.clear();
        }
        c.insert(key, std::sync::Arc::new(px.clone()));
        Some(px)
    }
}

static CACHE: Cache = Cache::new(96);

/// The tile for `g` `n` px a side in the `light` or dark theme, Flat's
/// matte picture (`flat`) or Glass's lit one, or None if Direct2D fails
/// here (the caller draws the line tile).
pub fn draw(n: u32, g: Glyph, light: bool, flat: bool) -> Option<Icon> {
    let what = Art::of(g).map(Content::Art).unwrap_or(Content::Line(g));
    let px = CACHE.get_or_draw((n, what, light, flat), || {
        DEVICE.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                match unsafe { make_device() } {
                    Ok(d) => *slot = Some(d),
                    Err(e) => {
                        crate::log!("tile_art: no device: {e}");
                        return None;
                    }
                }
            }
            let d = slot.as_ref()?;
            match unsafe { render(d, n, what, light, flat) } {
                Ok(px) => Some(px),
                Err(e) => {
                    crate::log!("tile_art: {what:?} at {n}: {e}");
                    *slot = None;
                    None
                }
            }
        })
    })?;
    Some(Icon { width: n, height: n, pixels: px })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mulberry_matches_the_mockup() {
        // The mockup's rng(7) and rng(21) in a browser.
        let mut r = rng(7);
        for want in [0.011704753153026104, 0.06195825757458806, 0.97690763277933] {
            assert!((r() - want).abs() < 1e-12);
        }
        assert!((rng(21)() - 0.4322912215720862).abs() < 1e-12);
    }

    #[test]
    fn path_data() {
        assert_eq!(parse("M1 2L3 4Z"), Some(vec![Seg::Move(1.0, 2.0), Seg::Line(3.0, 4.0), Seg::Close]));
        // T reflects the last Q's control point.
        let t = parse("M0 0Q5 5 10 0T20 0").unwrap();
        assert_eq!(t[2], Seg::Quad([15.0, -5.0, 20.0, 0.0]));
        // Relative commands and repeated numbers.
        assert_eq!(parse("m1 1 2 2h3v-1").unwrap(), vec![Seg::Move(1.0, 1.0), Seg::Line(3.0, 3.0), Seg::Line(6.0, 3.0), Seg::Line(6.0, 2.0)]);
        assert!(matches!(parse("M0 0A21.5 4.6 0 0 0 71.5 38").unwrap()[1], Seg::Arc { rx, ry, large: false, sweep: false, .. } if rx == 21.5 && ry == 4.6));
        assert_eq!(parse("L"), None);
        assert_eq!(parse("1 2"), None);
        // Every shape helper makes good path data.
        for d in [rect(1.0, 2.0, 30.0, 20.0, 4.0), rect(1.0, 2.0, 3.0, 4.0, 0.0), ellipse(5.0, 5.0, 3.0, 2.0), poly(&[(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)]), squircle_path()] {
            assert!(parse(&d).is_some(), "{d}");
        }
        assert!(parse(HAT).is_some() && parse(BEARD).is_some());
    }

    /// The glyph each picture is drawn for.
    fn glyph(a: Art) -> Glyph {
        [
            Glyph::Launchpad,
            Glyph::Trash,
            Glyph::TrashFull,
            Glyph::Download,
            Glyph::Document,
            Glyph::Image,
            Glyph::Music,
            Glyph::Play,
            Glyph::Monitor,
            Glyph::Folder,
            Glyph::Mark,
        ]
        .into_iter()
        .find(|g| Art::of(*g) == Some(a))
        .unwrap()
    }

    const SIZES: [u32; 7] = [16, 20, 32, 52, 64, 128, 256];

    fn alpha(i: &Icon, x: u32, y: u32) -> u8 {
        i.pixels[((y * i.width + x) * 4 + 3) as usize]
    }

    /// The plate's alpha is the squircle's: opaque where a pixel is wholly
    /// inside, clear where it is wholly outside.
    fn holds_the_squircle(i: &Icon, what: &str) {
        let n = i.width;
        let u = 100.0 / n as f32;
        for y in 0..n {
            for x in 0..n {
                let corners = [(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)].map(|(cx, cy)| in_squircle(cx as f32 * u, cy as f32 * u));
                let a = alpha(i, x, y);
                if corners.iter().all(|c| *c) {
                    assert!(a >= 250, "{what}: {x},{y} inside but {a}");
                } else if corners.iter().all(|c| !*c) {
                    assert!(a <= 2, "{what}: {x},{y} outside but {a}");
                }
            }
        }
    }

    /// The bare plate `n` px a side.
    fn plate_only(n: u32, light: bool) -> Vec<u8> {
        DEVICE.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = Some(unsafe { make_device() }.unwrap());
            }
            let d = slot.as_ref().unwrap();
            unsafe {
                let dc = &d.dc;
                let pf = D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED };
                let size = D2D_SIZE_U { width: n, height: n };
                let props = |o: D2D1_BITMAP_OPTIONS| D2D1_BITMAP_PROPERTIES1 { pixelFormat: pf, dpiX: 96.0, dpiY: 96.0, bitmapOptions: o, ..Default::default() };
                let target = dc.CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_TARGET)).unwrap();
                dc.SetTarget(&target);
                dc.BeginDraw();
                dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
                let p = pal(light);
                let pen = Pen { dc, f: &d.f, k: n as f32 / 100.0, m: std::cell::Cell::new(Matrix3x2::identity()), lod: false, p };
                pen.fill(&squircle_path(), &Fill::V(p.plate)).unwrap();
                dc.SetTransform(&Matrix3x2::identity());
                dc.EndDraw(None, None).unwrap();
                dc.SetTarget(None::<&ID2D1Image>);
                let read = dc.CreateBitmap(size, None, 0, &props(D2D1_BITMAP_OPTIONS_CPU_READ | D2D1_BITMAP_OPTIONS_CANNOT_DRAW)).unwrap();
                read.CopyFromBitmap(None, &target, None).unwrap();
                let m = read.Map(D2D1_MAP_OPTIONS_READ).unwrap();
                let mut out = Vec::new();
                for y in 0..n {
                    out.extend_from_slice(std::slice::from_raw_parts(m.bits.add((y * m.pitch) as usize), (n * 4) as usize));
                }
                read.Unmap().unwrap();
                out
            }
        })
    }

    /// Share of the middle's pixels far (> 40 in a channel) from the bare
    /// plate: how much picture there is.
    fn picture_share(i: &Icon, light: bool) -> f32 {
        let n = i.width;
        let plate = plate_only(n, light);
        let (lo, hi) = (n / 5, n - n / 5);
        let mut far = 0;
        for y in lo..hi {
            for x in lo..hi {
                let k = ((y * n + x) * 4) as usize;
                if (0..3).any(|c| (i.pixels[k + c] as i32 - plate[k + c] as i32).abs() > 40) {
                    far += 1;
                }
            }
        }
        far as f32 / ((hi - lo) * (hi - lo)) as f32
    }

    #[test]
    fn every_tile_at_every_size() {
        use crate::glyphs::{line_tile, tile, TileLook, LINE_TILE_MAX_PX};
        let hc = Some(([0, 0, 0], [255, 255, 0]));
        for a in Art::ALL {
            let g = glyph(a);
            for light in [false, true] {
                let look = TileLook::new(light, None);
                for n in SIZES {
                    let what = format!("{a:?} {n} light {light}");
                    let i = tile(n, g, look);
                    assert_eq!((i.width, i.height, i.pixels.len()), (n, n, (n * n * 4) as usize), "{what}");
                    assert!(i.pixels.chunks_exact(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]), "{what}: premultiplied");
                    assert_eq!(alpha(&i, 0, 0), 0, "{what}: the corner is clear");
                    assert_eq!(alpha(&i, n / 2, n / 2), 255, "{what}");
                    assert!(crate::icons::analyze(&i).coverage >= 0.9, "{what}: full-bleed for the dock's squircle");
                    if n <= LINE_TILE_MAX_PX {
                        // Small: the line glyph on the flat tile.
                        assert_eq!(i.pixels, line_tile(n, g, look).pixels, "{what}");
                    } else {
                        holds_the_squircle(&i, &what);
                        let share = picture_share(&i, light);
                        // (Documents at 32 px light, white paper on the near-white plate, is
                        // the least: about 6 %.)
                        assert!(share > 0.05, "{what}: a picture on the plate ({share})");
                    }
                    // High contrast: the line tile at every size.
                    let h = TileLook::new(light, hc);
                    assert_eq!(tile(n, g, h).pixels, line_tile(n, g, h).pixels, "{what}: high contrast");
                }
            }
        }
    }

    #[test]
    fn the_switch_to_line_tiles() {
        use crate::glyphs::{line_tile, tile, TileLook};
        let look = TileLook::new(false, None);
        assert_eq!(tile(20, Glyph::Trash, look).pixels, line_tile(20, Glyph::Trash, look).pixels);
        assert_ne!(tile(21, Glyph::Trash, look).pixels, line_tile(21, Glyph::Trash, look).pixels);
        // The kinds a tile is drawn as, by size (the dock draws each kind
        // apart: dock::items::step_source).
        use crate::glyphs::{tile_kind, TileKind};
        assert_eq!([16, 20, 21, 32, 33, 128].map(|n| tile_kind(n, &look)), [TileKind::Line, TileKind::Line, TileKind::Simple, TileKind::Simple, TileKind::Full, TileKind::Full]);
        assert_eq!(tile_kind(128, &TileLook::new(false, Some(([0, 0, 0], [255, 255, 0])))), TileKind::Line);
        // Glyphs without a picture of their own: the line glyph on the lit
        // plate (the design pictures' stand-in apps).
        let globe = tile(64, Glyph::Globe, look);
        assert_ne!(globe.pixels, line_tile(64, Glyph::Globe, look).pixels);
        holds_the_squircle(&globe, "globe");
    }

    #[test]
    fn tiles_tell_apart() {
        // Every picture differs from every other, light from dark.
        for light in [false, true] {
            let all: Vec<Vec<u8>> = Art::ALL.iter().map(|a| draw(64, glyph(*a), light, false).unwrap().pixels).collect();
            for i in 0..all.len() {
                for j in i + 1..all.len() {
                    assert_ne!(all[i], all[j], "{:?} vs {:?}", Art::ALL[i], Art::ALL[j]);
                }
            }
        }
        assert_ne!(draw(64, Glyph::Trash, true, false).unwrap().pixels, draw(64, Glyph::Trash, false, false).unwrap().pixels);
        // The plate is lit from above: lighter at the top than the bottom.
        for light in [false, true] {
            let i = draw(128, Glyph::Folder, light, false).unwrap();
            let at = |y: u32| i.pixels[((y * 128 + 64) * 4 + 1) as usize];
            assert!(at(8) > at(120), "light {light}: {} vs {}", at(8), at(120));
        }
        // The same pixels drawn again (what the cache keeps).
        assert_eq!(draw(52, Glyph::Music, true, false).unwrap().pixels, draw(52, Glyph::Music, true, false).unwrap().pixels);
    }

    #[test]
    fn each_tile_drawn_once() {
        // A cache of its own (the shared one is filled and emptied by
        // other tests at the same time), counting what it draws.
        let c = Cache::new(4);
        let drawn = std::cell::Cell::new(0);
        let get = |n: u32, g: Glyph, light: bool| {
            let what = Art::of(g).map(Content::Art).unwrap_or(Content::Line(g));
            c.get_or_draw((n, what, light, false), || {
                drawn.set(drawn.get() + 1);
                Some(vec![n as u8, light as u8, drawn.get() as u8])
            })
        };
        let first = get(52, Glyph::Music, true);
        assert_eq!(drawn.get(), 1);
        // Asked again (another dock, the trash asked again): not drawn.
        assert_eq!(get(52, Glyph::Music, true), first);
        assert_eq!(get(52, Glyph::Music, true), first);
        assert_eq!(drawn.get(), 1, "same size, picture and theme: from the cache");
        // Another size, picture or theme: drawn, once each.
        get(64, Glyph::Music, true);
        assert_eq!(drawn.get(), 2, "size");
        get(52, Glyph::Trash, true);
        assert_eq!(drawn.get(), 3, "picture");
        get(52, Glyph::Music, false);
        assert_eq!(drawn.get(), 4, "theme");
        for (n, g, light) in [(52, Glyph::Music, true), (64, Glyph::Music, true), (52, Glyph::Trash, true), (52, Glyph::Music, false)] {
            get(n, g, light);
        }
        assert_eq!(drawn.get(), 4, "all four kept");
        // A failed drawing is not kept: tried again.
        assert_eq!(c.get_or_draw((70, Content::Art(Art::Trash), true, false), || None), None);
        let again = c.get_or_draw((70, Content::Art(Art::Trash), true, false), || Some(vec![1]));
        assert_eq!(again, Some(vec![1]));
        // (That one came when the cache was full: it was emptied first.)
        get(52, Glyph::Music, true);
        assert_eq!(drawn.get(), 5, "emptied when full");
        get(52, Glyph::Music, true);
        assert_eq!(drawn.get(), 5, "kept again after that");
    }

    #[test]
    fn tints_are_restrained() {
        // Color, but pale: no opaque pixel is more saturated than the
        // palette's strongest (the download arrow's #5480B8: chroma 100).
        for a in Art::ALL {
            for light in [false, true] {
                let i = draw(128, glyph(a), light, false).unwrap();
                for p in i.pixels.chunks_exact(4).filter(|p| p[3] == 255) {
                    let (lo, hi) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
                    assert!(hi - lo <= 110, "{a:?} light {light}: {p:?}");
                }
            }
        }
    }

    /// Writes `target/harmony-shots/<light|dark>/dock-tiles.png` (Glass's
    /// pictures) and `dock-tiles-flat.png` (Flat's): every picture at 128
    /// px, under it the same at 64 52 32 24 20 16.
    #[test]
    #[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
    fn harmony_dock_tiles() {
        use crate::glyphs::{tile, TileLook};
        for ((folder, light), flat) in crate::gfx::promo_kit::LOOKS.into_iter().flat_map(|l| [(l, false), (l, true)]) {
            let small = [64u32, 52, 32, 24, 20, 16];
            let gap = 16u32;
            let col_w = 128 + gap;
            let w = gap + col_w * Art::ALL.len() as u32;
            let h = gap + 128 + gap + small.iter().map(|s| s + 8).sum::<u32>() + gap;
            let bg = if light { 0xE6u8 } else { 0x26 };
            let mut px: Vec<u8> = (0..w * h).flat_map(|_| [bg, bg, bg, 255]).collect();
            let mut put = |i: &Icon, x0: u32, y0: u32| {
                for y in 0..i.height {
                    for x in 0..i.width {
                        let s = ((y * i.width + x) * 4) as usize;
                        let d = (((y0 + y) * w + x0 + x) * 4) as usize;
                        let a = i.pixels[s + 3] as f32 / 255.0;
                        for c in 0..3 {
                            px[d + c] = (i.pixels[s + c] as f32 + px[d + c] as f32 * (1.0 - a)).round().min(255.0) as u8;
                        }
                    }
                }
            };
            for (k, a) in Art::ALL.iter().enumerate() {
                let x = gap + k as u32 * col_w;
                put(&tile(128, glyph(*a), TileLook::new(light, None).finish(flat)), x, gap);
                let mut y = gap + 128 + gap;
                for s in small {
                    put(&tile(s, glyph(*a), TileLook::new(light, None).finish(flat)), x + (128 - s) / 2, y);
                    y += s + 8;
                }
            }
            let path = crate::gfx::promo_kit::harmony_dir().join(folder).join(if flat { "dock-tiles-flat.png" } else { "dock-tiles.png" });
            crate::gfx::promo_kit::save_png(&px, w, h, &path);
        }
    }

    #[test]
    fn flat_tiles_are_matte_greys() {
        // Flat's pictures: no hue (the gnome's hat alone keeps the brand's
        // brick), one plate grey at the top and the bottom (no light from
        // above), and not Glass's pixels.
        for a in Art::ALL {
            for light in [false, true] {
                let what = format!("{a:?} light {light}");
                let i = draw(128, glyph(a), light, true).unwrap();
                let g = draw(128, glyph(a), light, false).unwrap();
                assert_ne!(i.pixels, g.pixels, "{what}: Flat is not Glass");
                holds_the_squircle(&i, &what);
                let hued = i.pixels.chunks_exact(4).filter(|p| p[3] == 255 && p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) > 6).count();
                if a == Art::Mark {
                    assert!(hued > 0, "{what}: the hat keeps its brick");
                } else {
                    assert_eq!(hued, 0, "{what}: greys only");
                }
                let at = |y: u32| &i.pixels[((y * 128 + 64) * 4) as usize..((y * 128 + 64) * 4 + 4) as usize];
                assert_eq!(at(4), at(123), "{what}: one plate grey");
            }
        }
        // Kept apart in the cache: asked one after the other, each its own.
        assert_ne!(draw(52, Glyph::Folder, true, true).unwrap().pixels, draw(52, Glyph::Folder, true, false).unwrap().pixels);
        // The dock's look carries it.
        use crate::glyphs::{tile, TileLook};
        let look = TileLook::new(false, None);
        assert_eq!(tile(64, Glyph::Trash, look.finish(true)).pixels, draw(64, Glyph::Trash, false, true).unwrap().pixels);
        assert_eq!(tile(64, Glyph::Trash, look).pixels, draw(64, Glyph::Trash, false, false).unwrap().pixels);
    }

    #[test]
    fn squircle_test() {
        assert!(in_squircle(50.0, 50.0) && in_squircle(1.0, 50.0) && in_squircle(50.0, 99.0));
        assert!(!in_squircle(1.0, 1.0) && !in_squircle(6.0, 6.0));
        assert!(in_squircle(10.0, 10.0), "a squircle's corner is fuller than a circle's");
    }
}
