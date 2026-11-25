//! The top bar drawn off screen for the design audit (tests only; see
//! crate::gfx::promo_kit, "harmony"). On screen the bar's ground, its
//! fills and its cells are compositor visuals, so each is put together
//! here from what the bar computes them with: the ground from look::ground
//! (the style, the theme, the wallpaper's brightness), the cells' places
//! as TopBar::layout has them (EDGE_PAD, cell_width, content_x), the fill
//! from hover_pill and capsule::look (the ground's mark), the text on the
//! bar's one baseline (baseline::line), the glyphs in their 16 box
//! (glyphs::fitted), the drawn cells with their own paint (paint_battery,
//! input::paint_input, desktops::paint_dots, gfx::paint_start) and clear's
//! halo (look::Halo, draw_halo here: a Gaussian shadow of the cells'
//! content). The words
//! are made up.
//!
//! Two sheets. States: one bar a row on the theme's material, a text cell
//! and a glyph cell of it in the row's state (rest, hover, pressed, open,
//! the keyboard's focus, disabled). Styles: clear, frost, recipe and solid
//! each over a bright and a dark wallpaper (made here: no photo), a menu
//! and a glyph cell open. The blur under frost and a glass recipe is drawn
//! as the wallpaper strip box-blurred (the DWM backdrop is not there off
//! screen) through glass v3's graph (glass::model). At 100 % and 150 %,
//! light and dark.

use windows::core::Interface;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_numerics::{Matrix3x2, Vector2};

use super::look::{self, Ground};
use super::*;
use crate::gfx::promo_kit::{self as kit, col, Canvas};
use crate::syslook::{HighContrast, SysLook};
use crate::theme::{Rgba, State};

/// The picture's bar (DIP) and the room around and between bars.
pub(in crate::topbar) const BAR_W: f32 = 720.0;

/// The width paint() draws the bar at.
pub(in crate::topbar) fn bar_w() -> f32 {
    BAR_W
}
const GAP: f32 = 16.0;
const ROOM: f32 = 24.0;
/// Styles sheet: the wallpaper shows this far above and below each bar.
const BAND: f32 = 14.0;

/// What a cell of the picture's bar shows.
#[derive(Clone, Copy)]
enum What {
    /// The Start mark (gfx::paint_start, START_PX).
    Start,
    Text(&'static str, bool),
    Clock(&'static str),
    /// A glyph at this size (GLYPH_SIZE, or the chevron's).
    Glyph(Glyph, f32),
    /// The input indicator: its label, Caps Lock on.
    Input(&'static str, bool),
    /// Percent, charging.
    Battery(u8, bool),
    /// Desktops, the current one.
    Dots(usize, usize),
}

impl What {
    /// The bar's cell it stands for (its pads: super::pads).
    fn cell(self) -> Cell {
        match self {
            What::Start => Cell::Start,
            What::Text(_, true) => Cell::App,
            What::Text(_, false) => Cell::Menu(0),
            What::Clock(_) => Cell::Clock,
            What::Glyph(_, em) if em < GLYPH_SIZE => Cell::Chevron,
            What::Glyph(..) => Cell::Control,
            What::Input(..) => Cell::Input,
            What::Battery(..) => Cell::Battery,
            What::Dots(..) => Cell::Desktops,
        }
    }
}

/// A row: its state, and whether the keyboard's focus is on its cells.
#[derive(Clone, Copy)]
pub(in crate::topbar) struct Row {
    pub(in crate::topbar) state: State,
    pub(in crate::topbar) focus: bool,
}

const ROWS: [Row; 6] = [
    Row { state: State::Rest, focus: false },
    Row { state: State::Hover, focus: false },
    Row { state: State::Pressed, focus: false },
    Row { state: State::Open, focus: false },
    Row { state: State::Rest, focus: true },
    Row { state: State::Disabled, focus: false },
];

/// The cells of the bar, left side then right side (in slot order), and
/// which two take the row's state: a title and a glyph button.
fn cells() -> (Vec<What>, Vec<What>, (usize, usize)) {
    let left = vec![
        What::Start,
        What::Text(kit::say("메모", "Notes"), true),
        What::Text(kit::say("파일", "File"), false),
        What::Text(kit::say("편집", "Edit"), false),
        What::Text(kit::say("보기", "View"), false),
        What::Text(kit::say("창", "Window"), false),
    ];
    let right = vec![
        What::Dots(3, 1),
        What::Glyph(Glyph::ChevronLeft, CHEVRON_SIZE),
        What::Input(kit::say("가", "A"), true),
        What::Battery(72, false),
        What::Battery(100, true),
        What::Glyph(Glyph::Search, GLYPH_SIZE),
        What::Glyph(Glyph::Sliders, GLYPH_SIZE),
        What::Clock(kit::say("11월 9일 (월) 오후 2:30", "Mon, Nov 9  2:30 PM")),
    ];
    (left, right, (3, 6))
}

/// The theme of `look` ("flat", "glass", "hc") in the palette (as
/// gfx::parts' token sheets and the dock's pictures resolve it).
pub(in crate::topbar) fn look_theme(look: &str, light: bool) -> Theme {
    let grey = if light { 0x3A } else { 0xE6 };
    let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
    let hc = (look == "hc").then_some(HighContrast { window: black, text: white, highlight: Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 }, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT });
    let sys = SysLook { light, accent: Rgba { r: grey, g: grey, b: grey, a: 255 }, transparency: true, high_contrast: hc, animations: true, ui_font: 0 };
    let mut raw = Theme::graphite();
    // The run's finish for the sheet's own look (kit::look_finish).
    raw.finish = kit::look_finish(look);
    raw.resolve_fx(&sys, true).windows_shell()
}

fn d2d(c: Rgba) -> D2D1_COLOR_F {
    col(c)
}

/// The bar's height (the default: config::TOPBAR_HEIGHT).
pub(in crate::topbar) fn bar_h() -> f32 {
    crate::config::Config::default().topbar.height.clamp(20.0, 48.0)
}

/// A cell's content: how wide and tall it is (DIP), and its paint at
/// (left, top) px.
struct Drawn<'a> {
    w: f32,
    h: f32,
    paint: Box<dyn Fn(&Canvas, f32, f32) + 'a>,
}

/// Lays out what cell `what` shows in `ink` at `s` px per DIP, as the
/// bar's own surfaces are made: its text and its glyphs, the Start mark
/// and the dots.
fn content<'a>(gfx: &'a crate::gfx::Gfx, theme: &'a Theme, what: What, ink: D2D1_COLOR_F, s: f32) -> Drawn<'a> {
    let family = theme.font.family.clone();
    let size = theme.font.size.clamp(10.0, 18.0);
    let h = bar_h();
    match what {
        What::Start => {
            let px = (START_PX * s).round();
            Drawn { w: px / s, h: px / s, paint: Box::new(move |cv, x, y| unsafe { crate::gfx::paint_start(&cv.dc, x, y, px, (ink.r, ink.g, ink.b, ink.a)).unwrap() }) }
        }
        What::Text(..) | What::Clock(_) => {
            let (l, tw, th) = match what {
                What::Text(t, _) => gfx.text_layout(t, &family, size * s, DWRITE_FONT_WEIGHT_NORMAL),
                What::Clock(t) => gfx.text_layout_tabular(t, &family, size * s, DWRITE_FONT_WEIGHT_NORMAL),
                _ => unreachable!(),
            }
            .expect("the text");
            // A text cell is as tall as the bar, its text on the bar's one
            // baseline (TopBar::bar_line, on_bar_line).
            let metrics = gfx.font_metrics(&family, DWRITE_FONT_WEIGHT_NORMAL).map(|m| baseline::Metrics::of(m.designUnitsPerEm, m.descent, m.capHeight)).unwrap_or(baseline::Metrics::SEGOE);
            let line = baseline::line(h * s, &metrics, size * s * crate::fonts::scale());
            let dy = baseline::origin_y(line.baseline, crate::gfx::first_baseline(&l, th));
            Drawn {
                w: tw / s,
                h,
                paint: Box::new(move |cv, x, y| unsafe {
                    let b = cv.dc.CreateSolidColorBrush(&ink, None).unwrap();
                    crate::gfx::draw_text(&cv.dc, Vector2 { X: x, Y: y + dy }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }),
            }
        }
        What::Glyph(g, em) => {
            let side = (GLYPH_BOX * s).round().max(1.0);
            Drawn {
                w: side / s,
                h: side / s,
                paint: Box::new(move |cv, x, y| unsafe {
                    super::paint_glyph_cell(&cv.dc, g, x, y, side, em * s, ink).unwrap();
                }),
            }
        }
        What::Input(text, caps) => {
            let art = input::input_art(gfx, theme, text, caps, false, ink, s).expect("the indicator");
            let (w_px, h_px) = art.size_px();
            Drawn { w: w_px as f32 / s, h: h_px as f32 / s, paint: Box::new(move |cv, x, y| unsafe { input::paint_input(&cv.dc, x, y, &art).unwrap() }) }
        }
        What::Battery(p, charging) => {
            let art = battery_art(gfx, theme, ink, p, charging, s);
            Drawn { w: BATTERY_W, h: BATTERY_H, paint: Box::new(move |cv, x, y| unsafe { paint_battery(&cv.dc, x, y, &art).unwrap() }) }
        }
        What::Dots(n, current) => {
            let (w_px, h_px) = ((desktops::dots_width(n) * s).ceil().max(1.0), (GLYPH_BOX * s).round().max(1.0));
            let ring = D2D1_COLOR_F { a: ink.a * if theme.high_contrast { 1.0 } else { 0.8 }, ..ink };
            Drawn { w: w_px / s, h: h_px / s, paint: Box::new(move |cv, x, y| unsafe { desktops::paint_dots(&cv.dc, x, y, h_px, (n, Some(current), false), ink, ring, s).unwrap() }) }
        }
    }
}

/// Where the cells go on a bar BAR_W wide (x, width; DIP), as
/// TopBar::layout places them: the left side from EDGE_PAD, the right side
/// back from the other end, each as wide as cell_width makes it.
fn places(width: f32, left: &[(Cell, f32)], right: &[(Cell, f32)]) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    let mut x = EDGE_PAD;
    for &(c, w) in left {
        let w = cell_width(c, w);
        out.push((x, w));
        x += w;
    }
    let widths: Vec<f32> = right.iter().map(|&(c, w)| cell_width(c, w)).collect();
    let mut x = width - EDGE_PAD - widths.iter().sum::<f32>();
    for w in widths {
        out.push((x, w));
        x += w;
    }
    out
}

/// The styles sheet's third wallpaper (the 2026-11-11 review's case):
/// white, with fine black and white stripes under the left menus and a
/// black block under the right end (the clock and its neighbors).
fn wallpaper_mixed(w: u32, h: u32) -> crate::icons::Icon {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..h {
        for x in 0..w {
            let f = x as f32 / w.max(1) as f32;
            let black = f >= 0.72 || ((0.12..0.30).contains(&f) && (x / 3) % 2 == 0);
            let v = if black { 8 } else { 250 };
            px.extend([v, v, v, 255]);
        }
    }
    crate::icons::Icon { width: w, height: h, pixels: px }
}

/// A wallpaper of the styles sheet (made here, no photo): soft color
/// fields like the mock-up's "밝은 종이" (`bright`) and "블룸".
fn wallpaper(bright: bool, w: u32, h: u32) -> crate::icons::Icon {
    // (center x, center y, radius, color) in the picture's fractions,
    // over a vertical base from `top` to `bottom`.
    type Blob = (f32, f32, f32, [f32; 3]);
    let (top, bottom, blobs): ([f32; 3], [f32; 3], [Blob; 3]) = if bright {
        ([246.0, 243.0, 238.0], [238.0, 242.0, 248.0], [(0.25, 0.3, 0.45, [255.0, 217.0, 230.0]), (0.75, 0.65, 0.5, [212.0, 232.0, 255.0]), (0.55, 0.15, 0.35, [255.0, 241.0, 201.0])])
    } else {
        ([11.0, 16.0, 48.0], [6.0, 33.0, 58.0], [(0.3, 0.7, 0.55, [61.0, 107.0, 255.0]), (0.75, 0.35, 0.5, [177.0, 77.0, 255.0]), (0.6, 0.85, 0.4, [20.0, 200.0, 255.0])])
    };
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w.max(1) as f32, y as f32 / h.max(1) as f32);
            let mut c = [0.0f32; 3];
            for k in 0..3 {
                c[k] = top[k] + (bottom[k] - top[k]) * fy;
            }
            for (bx, by, r, bc) in blobs {
                // The bands are wide: the blobs are round in the picture's
                // own aspect (x counts 4 times less).
                let d = (((fx - bx) * 0.25).powi(2) + (fy - by).powi(2)).sqrt() / r;
                let k = (1.0 - d).clamp(0.0, 1.0).powi(2) * if bright { 0.9 } else { 0.75 };
                for i in 0..3 {
                    c[i] += (bc[i] - c[i]) * k;
                }
            }
            px.extend([c[2].round() as u8, c[1].round() as u8, c[0].round() as u8, 255]);
        }
    }
    crate::icons::Icon { width: w, height: h, pixels: px }
}

/// `icon` drawn at (`x`, `y`) px, its own size.
fn draw_px(cv: &Canvas, icon: &crate::icons::Icon, x: f32, y: f32) {
    draw_px_to(cv, icon, kit::rect(x, y, x + icon.width as f32, y + icon.height as f32));
}

fn draw_px_to(cv: &Canvas, icon: &crate::icons::Icon, dest: D2D_RECT_F) {
    unsafe {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            ..Default::default()
        };
        let bmp = cv.dc.CreateBitmap(D2D_SIZE_U { width: icon.width, height: icon.height }, Some(icon.pixels.as_ptr() as _), icon.width * 4, &props).unwrap();
        cv.dc.DrawBitmap(&bmp, Some(&dest), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
    }
}

/// Rows `y0`..`y0 + rows` of `icon`.
fn rows_of(icon: &crate::icons::Icon, y0: u32, rows: u32) -> crate::icons::Icon {
    let stride = (icon.width * 4) as usize;
    let a = y0 as usize * stride;
    crate::icons::Icon { width: icon.width, height: rows, pixels: icon.pixels[a..a + rows as usize * stride].to_vec() }
}

/// Draws one bar with its top left at (`ox`, `oy`) DIP on a canvas in
/// pixels at `s`, standing on `ground`; `under`: the wallpaper's strip
/// right under it (px), blurred for a backdrop; `cols`: the wallpaper
/// under the bar column by column (clear: each cell's own look,
/// look::cell_look, as TopBar::cell_looks_now has it).
/// Returns the cells' places on the bar (x, width; DIP from its left).
#[allow(clippy::too_many_arguments)]
pub(in crate::topbar) fn paint(cv: &Canvas, gfx: &crate::gfx::Gfx, theme: &Theme, ground: &Ground, s: f32, ox: f32, oy: f32, row: Row, under: Option<&crate::icons::Icon>, cols: Option<&[look::Col]>) -> Vec<(f32, f32)> {
    paint_bar(cv, gfx, theme, ground, s, (ox, oy, bar_w()), row, under, cols)
}

#[allow(clippy::too_many_arguments)]
fn paint_bar(cv: &Canvas, gfx: &crate::gfx::Gfx, theme: &Theme, ground: &Ground, s: f32, (ox, oy, width): (f32, f32, f32), row: Row, under: Option<&crate::icons::Icon>, cols: Option<&[look::Col]>) -> Vec<(f32, f32)> {
    let h = bar_h();
    let px = |v: f32| baseline::snap_px(v, s) * s;
    let (bx, by) = (px(ox), px(oy));
    // The ground: the blur (when it has one), the tint, high contrast's
    // line along the edge towards the desktop. No rim, no shadow.
    let bar = D2D_RECT_F { left: bx, top: by, right: bx + width * s, bottom: by + h * s };
    unsafe {
        // The blur through glass v3's graph (glass::model, as the screen
        // draws it): the recipe's (the glass's tokens) or Frost's own
        // saturation alone; the tint laid in with it. (The recipe's top
        // light and grain are not drawn here: glass_harmony's sheets.)
        let graph = match ground.style {
            look::Style::Recipe => theme.glass_fx(crate::theme::Material::Bar),
            look::Style::Frost => Some(crate::theme::GlassFx { sat: look::FROST_SAT, lum: Rgba { a: 0, ..ground.tint }, spec: 0.0, noise: 0.0 }),
            _ => None,
        };
        let mut tinted = false;
        if let (true, Some(u)) = (ground.backdrop, under) {
            if ground.style == look::Style::Glass {
                draw_px_to(cv, u, bar);
            } else {
                let mut b = crate::icons::Icon { width: u.width, height: u.height, pixels: u.pixels.clone() };
                crate::wallpaper::blur_bgra(&mut b.pixels, b.width as usize, b.height as usize, (12.0 * s) as usize, 3);
                if let Some(fx) = graph {
                    for p in b.pixels.as_chunks_mut::<4>().0 {
                        let c = crate::glass::model([p[2], p[1], p[0]].map(|v| v as f32 / 255.0), &fx, ground.laid(ground.tint)).map(|v| (v * 255.0).round() as u8);
                        p[..3].copy_from_slice(&[c[2], c[1], c[0]]);
                    }
                    tinted = true;
                }
                draw_px(cv, &b, bx, by);
            }
        }
        if ground.tint.a > 0 && !tinted {
            // Flat v2's band falls from top to bottom, its grain over it
            // (TopBar::apply_theme: glass::fall_brush, top_light).
            crate::gfx::parts::plate_fill(&cv.dc, bar, 0.0, ground.fall, ground.tint).unwrap();
            if ground.grain > 0.0 {
                kit::draw_grain(cv, bar, 0.0, ground.grain, 1.0);
            }
        }
        if let Some((e, w)) = ground.edge {
            let w = if ground.style == look::Style::Glass { w } else { w * s };
            cv.dc.FillRectangle(&D2D_RECT_F { top: bar.bottom - w, ..bar }, &cv.dc.CreateSolidColorBrush(&d2d(e), None).unwrap());
        }
    }
    let (left, right, (lit_text, lit_glyph)) = cells();
    let lit = [lit_text, left.len() + lit_glyph];
    let all: Vec<What> = left.iter().chain(right.iter()).copied().collect();
    let (inset, ph, rad) = hover_pill(h);
    let mode = crate::motion::Mode::Full;
    // Each cell's look first (its content's color comes from it), then
    // its content, then the places.
    let state_of = |i: usize| if lit.contains(&i) { row.state } else { State::Rest };
    let inks: Vec<Rgba> = (0..all.len()).map(|i| capsule::look(theme, ground.mark, state_of(i), ICON_CELL, ph, mode).ink.unwrap_or(ground.ink)).collect();
    let mut drawn: Vec<Drawn> = all.iter().zip(&inks).map(|(w, ink)| content(gfx, theme, *w, d2d(*ink), s)).collect();
    let lw: Vec<(Cell, f32)> = drawn[..left.len()].iter().zip(&left).map(|(d, w)| (w.cell(), d.w)).collect();
    let rw: Vec<(Cell, f32)> = drawn[left.len()..].iter().zip(&right).map(|(d, w)| (w.cell(), d.w)).collect();
    let at = places(width, &lw, &rw);
    // Clear over a known wallpaper: each cell's own look (its ink, halo,
    // open fill and guard plate), from the columns under it.
    let cell_looks: Vec<Option<look::CellLook>> = match cols {
        Some(c) if ground.style == look::Style::Clear => at
            .iter()
            .zip(&all)
            .map(|(&(x, w), what)| look::cell_look(&look::extremes(look::cols_under(c, width, x, w)), (!one_color(what.cell())).then_some(ground.ink == look::INK_ON_LIGHT), ground.ink))
            .collect(),
        _ => vec![None; all.len()],
    };
    for (i, l) in cell_looks.iter().enumerate() {
        if let Some(l) = l.filter(|l| l.ink != ground.ink && inks[i] == ground.ink) {
            drawn[i] = content(gfx, theme, all[i], d2d(l.ink), s);
        }
    }
    for (i, &(x, w)) in at.iter().enumerate() {
        // The guard plate under it (clear, a mixed wallpaper there).
        if let Some(g) = cell_looks[i].and_then(|l| l.guard) {
            let r = D2D_RECT_F { left: bx + px(x), top: by + px(inset), right: bx + px(x) + w * s, bottom: by + px(inset) + ph * s };
            unsafe { cv.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad * s, radiusY: rad * s }, &cv.dc.CreateSolidColorBrush(&d2d(g), None).unwrap()) };
        }
        // The fill behind it (Capsules: placed on whole pixels, scaled
        // about its center while pressed).
        let mark = cell_looks[i].map(|l| l.mark).or(ground.mark);
        let look = capsule::look(theme, mark, state_of(i), w, ph, mode);
        let (cx0, cy0) = (bx + px(x), by + px(inset));
        let (cw, ch) = (w * s, ph * s);
        let k = look.scale;
        let cap = D2D_RECT_F { left: cx0 + cw * (1.0 - k) / 2.0, top: cy0 + ch * (1.0 - k) / 2.0, right: cx0 + cw * (1.0 + k) / 2.0, bottom: cy0 + ch * (1.0 + k) / 2.0 };
        let r = rad * s * k;
        unsafe {
            if look.fill.a > 0 {
                cv.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: cap, radiusX: r, radiusY: r }, &cv.dc.CreateSolidColorBrush(&d2d(look.fill), None).unwrap());
            }
            if let Some((oc, ow)) = look.outline {
                let t = ow * s * k;
                let inner = D2D_RECT_F { left: cap.left + t / 2.0, top: cap.top + t / 2.0, right: cap.right - t / 2.0, bottom: cap.bottom - t / 2.0 };
                cv.dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: inner, radiusX: (r - t / 2.0).max(0.0), radiusY: (r - t / 2.0).max(0.0) }, &cv.dc.CreateSolidColorBrush(&d2d(oc), None).unwrap(), t, None);
            }
            if row.focus && lit.contains(&i) {
                // The keyboard's focus (capsule::focus_ring), at once.
                let ((rx, ry, rw_, rh), rr, t) = capsule::focus_ring((x, inset, w, ph), rad);
                let ring = D2D_RECT_F { left: bx + rx * s, top: by + ry * s, right: bx + (rx + rw_) * s, bottom: by + (ry + rh) * s };
                cv.dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: ring, radiusX: rr * s, radiusY: rr * s }, &cv.dc.CreateSolidColorBrush(&d2d(crate::gfx::focus_color(theme)), None).unwrap(), t * s, None);
            }
        }
    }
    // The contents, centered on whole pixels across and down (layout).
    let spots: Vec<(f32, f32)> = at.iter().zip(&drawn).zip(&all).map(|((&(x, w), d), what)| (bx + px(content_x(what.cell(), x, w, d.w, false)), by + baseline::cell_top(h, d.h, s) * s)).collect();
    let paint_all = |cv: &Canvas, only: Option<look::Halo>| {
        for (i, (d, &(x, y))) in drawn.iter().zip(&spots).enumerate() {
            if only.is_none_or(|h| cell_looks[i].map(|l| l.halo).or(ground.halo) == Some(h)) {
                (d.paint)(cv, x, y);
            }
        }
    };
    // Clear's halos (each cell's own, or the bar's).
    let mut halos: Vec<look::Halo> = Vec::new();
    for h in cell_looks.iter().filter_map(|l| l.map(|l| l.halo).or(ground.halo)) {
        if !halos.contains(&h) && ground.halo.is_some() {
            halos.push(h);
        }
    }
    for halo in halos {
        // Clear's halo: a Gaussian shadow of the contents (draw_halo,
        // as the compositor's DropShadow from the visuals' content).
        unsafe {
            let list = cv.dc.CreateCommandList().unwrap();
            let old = cv.dc.GetTarget().unwrap();
            cv.dc.SetTarget(&list);
            paint_all(cv, Some(halo));
            cv.dc.SetTarget(&old);
            list.Close().unwrap();
            draw_halo(&cv.dc, &list.cast().unwrap(), Vector2 { X: 0.0, Y: 0.0 }, look::Halo { dy: halo.dy * s, blur: halo.blur * s, ..halo }).unwrap();
        }
    }
    paint_all(cv, None);
    at
}

/// A canvas `w` x `h` DIP at `s`, in pixels, the audit's backdrop filled.
fn canvas(stage: &kit::Stage, w: f32, h: f32, s: f32, light: bool) -> Canvas {
    let cv = Canvas::of(stage.gfx.d2d_factory().clone(), w, h, s);
    unsafe {
        cv.dc.Clear(Some(&col(kit::harmony_backdrop(light))));
        // In pixels: the bar's surfaces are drawn in pixels.
        cv.dc.SetTransform(&Matrix3x2::identity());
    }
    cv
}

/// One states sheet: the six rows in `look` on its material (recipe), at
/// `s`.
fn sheet(name: &str, look: &str, s: f32) {
    let stage = kit::stage();
    let h = bar_h();
    let (w, total) = (BAR_W + ROOM * 2.0, ROOM * 2.0 + ROWS.len() as f32 * h + (ROWS.len() - 1) as f32 * GAP);
    for (folder, light) in kit::LOOKS {
        let theme = look_theme(look, light);
        let ground = look::ground(&theme, look::Style::Recipe, look::Env::default(), &look::Wall::Pending);
        let cv = canvas(&stage, w, total, s, light);
        for (k, row) in ROWS.iter().enumerate() {
            paint(&cv, &stage.gfx, &theme, &ground, s, ROOM, ROOM + k as f32 * (h + GAP), *row, None, None);
        }
        kit::harmony_save(cv, folder, name);
        assert!(kit::harmony_dir().join(folder).join(format!("{name}.png")).is_file());
    }
}

/// The styles sheet: each style over a bright, a dark and a mixed
/// wallpaper, in `look`'s theme, at `s`. Clear's ink is read from the
/// bar's own rectangle of the wallpaper the way the bar reads it
/// (look::columns, look::cell_look).
fn styles_sheet(name: &str, look_name: &str, s: f32) {
    let stage = kit::stage();
    let h = bar_h();
    let styles = [look::Style::Clear, look::Style::Frost, look::Style::Recipe, look::Style::Solid];
    let band = h + BAND * 2.0;
    let rows = styles.len() * 3;
    let (w, total) = (BAR_W + ROOM * 2.0, ROOM * 2.0 + rows as f32 * band + (rows - 1) as f32 * GAP);
    let (wp_w, band_px) = (((BAR_W + ROOM * 2.0) * s).round() as u32, (band * s).round() as u32);
    let walls = [wallpaper(true, wp_w, band_px), wallpaper(false, wp_w, band_px), wallpaper_mixed(wp_w, band_px)];
    for (folder, light) in kit::LOOKS {
        let theme = look_theme(look_name, light);
        let cv = canvas(&stage, w, total, s, light);
        for (k, (style, wall)) in styles.iter().flat_map(|st| walls.iter().map(move |wl| (st, wl))).enumerate() {
            let top = ROOM + k as f32 * (band + GAP);
            let (y_px, bar_px) = ((top * s).round(), (BAND * s).round() as u32);
            draw_px(&cv, wall, 0.0, y_px);
            let strip = rows_of(wall, bar_px, (h * s).round() as u32);
            // The bar's own rectangle of it (from the bar's left edge).
            let x0 = (ROOM * s).round() as u32;
            let cols = look::columns(&strip.pixels, strip.width, strip.height, (x0, 0, x0 + (BAR_W * s).round() as u32, strip.height));
            let wall = look::Wall::Known(cols.clone());
            let ground = look::ground(&theme, *style, look::Env::default(), &wall);
            let under = crate::icons::Icon {
                width: (BAR_W * s).round() as u32,
                height: strip.height,
                pixels: (0..strip.height)
                    .flat_map(|r| {
                        let a = ((r * strip.width + x0) * 4) as usize;
                        strip.pixels[a..a + (BAR_W * s).round() as usize * 4].to_vec()
                    })
                    .collect(),
            };
            paint(&cv, &stage.gfx, &theme, &ground, s, ROOM, top + BAND, Row { state: State::Open, focus: false }, Some(&under), Some(&cols));
        }
        kit::harmony_save(cv, folder, name);
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_topbar_states() {
    let _l = crate::i18n::test_lock("ko");
    // Flat (the default finish) at 100 % and 150 %; Glass and high
    // contrast at 150 % (design system 12.4).
    for (tag, s) in [("100", 1.0f32), ("150", 1.5)] {
        sheet(&format!("topbar-states-{tag}"), "flat", s);
    }
    sheet("topbar-glass-150", "glass", 1.5);
    sheet("topbar-hc-150", "hc", 1.5);
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_topbar_styles() {
    let _l = crate::i18n::test_lock("ko");
    // Clear, frost, recipe and solid over a bright and a dark wallpaper:
    // Flat at 100 % and 150 %, Glass (its recipe differs) at 150 %.
    for (tag, s) in [("100", 1.0f32), ("150", 1.5)] {
        styles_sheet(&format!("topbar-styles-{tag}"), "flat", s);
    }
    styles_sheet("topbar-styles-glass-150", "glass", 1.5);
    assert!(kit::harmony_dir().join("dark").join("topbar-styles-150.png").is_file());
}

fn paint_glass_desktop(stage: &kit::Stage, cv: &Canvas, theme: &Theme, wall: &crate::icons::Icon, s: f32) {
    use crate::desklayer::{backdrop, card, graphite_snapshots as widgets};

    let mon = RECT { left: 0, top: 0, right: cv.w as i32, bottom: cv.h as i32 };
    draw_px(cv, wall, 0.0, 0.0);
    let mica = widgets::mica_with_radius(wall, crate::wallpaper::mica_radius(s));
    let bar = RECT { bottom: (bar_h() * s).round() as i32, ..mon };
    let (l, strip) = look::mica_strip(&mica, mon, bar).expect("bar strip");
    let g = look::ground(theme, look::Style::Glass, look::Env::default(), &look::Wall::Glass(crate::theme::AdaptiveGlass::new(l, None)));
    paint_bar(cv, &stage.gfx, theme, &g, s, (0., 0., 1280.0), ROWS[0], Some(&strip), None);
    backdrop::set_test(backdrop::grey_of_mica(mon, &mica));
    for (kind, [x, y, w, h]) in [
        (card::Kind::Note, [804., 40., 300., 400.]),
        (card::Kind::Battery, [1116., 40., 150., 196.]),
    ] {
        let px = |v: f32| (v * s).round() as i32;
        let r = RECT { left: px(x), top: px(y), right: px(x + w), bottom: px(y + h) };
        unsafe {
            cv.dc.SetTransform(&Matrix3x2 { M11: s, M22: s, M31: r.left as f32, M32: r.top as f32, ..Matrix3x2::identity() });
            widgets::draw_card(&cv.dc, theme, kind, true, r, s, 0., false);
        }
    }
    backdrop::set_test(None);
}

#[test]
#[ignore = "writes the three English README PNGs to the harmony output directory"]
fn readme_screenshots() {
    let _lang = crate::i18n::test_lock("en");
    let out = kit::harmony_dir().join("readme");
    crate::dock::render_readme_dock(&out.join("dock.png"));
    crate::desklayer::render_readme_widgets(&out.join("widgets.png"));

    let stage = kit::stage();
    let cv = canvas(&stage, 1280.0, 720.0, 1.0, false);
    let wall = wallpaper(true, cv.w, cv.h);
    paint_glass_desktop(&stage, &cv, &look_theme("glass", false), &wall, 1.0);
    let (pixels, w, h) = cv.finish();
    kit::save_png(&pixels, w, h, &out.join("top-bar.png"));

    for name in ["dock.png", "widgets.png", "top-bar.png"] {
        let size = std::fs::metadata(out.join(name)).unwrap().len();
        assert!(size < 1_000_000, "{name} is {size} bytes");
    }
}

#[test]
#[ignore = "renders glass over gradients, available Windows wallpapers, and an optional custom image"]
fn harmony_glass_c() {
    use std::path::PathBuf;

    let _l = crate::i18n::test_lock("ko");
    let out = PathBuf::from(std::env::var_os("NOPBAR_GLASS_C_RENDER_DIR").expect("set the offscreen render directory"));
    let mut walls = vec![("light", None), ("dark", None)];
    if let Some(windows) = std::env::var_os("WINDIR") {
        let wallpapers = PathBuf::from(windows).join("Web/Wallpaper");
        let samples = [
            ("win-light", "Windows/img0.jpg"),
            ("win-dark", "Windows/img19.jpg"),
            ("vivid", "ThemeB/img25.jpg"),
            ("sky", "ThemeC/img31.jpg"),
            ("pastel", "ThemeD/img34.jpg"),
        ];
        walls.extend(samples.into_iter().filter_map(|(name, file)| {
            let path = wallpapers.join(file);
            path.is_file().then_some((name, Some(path)))
        }));
    }
    if let Some(path) = std::env::var_os("NOPBAR_GLASS_C_WALLPAPER") {
        walls.push(("custom", Some(PathBuf::from(path))));
    }
    let stage = kit::stage();
    let theme = look_theme("glass", false);
    for (name, path) in walls {
        let desk = about::Desk { path, lost: false, placement: Some(crate::wallpaper::Placement::Fill), back: None };
        for (s, scale) in [(1.0, 100), (1.5, 150)] {
            let cv = canvas(&stage, 1280.0, 720.0, s, false);
            let mon = RECT { left: 0, top: 0, right: cv.w as i32, bottom: cv.h as i32 };
            let wall = if desk.path.is_some() {
                about::compose(&desk, mon, mon, cv.w).expect("wallpaper image")
            } else {
                wallpaper(name == "light", cv.w, cv.h)
            };
            paint_glass_desktop(&stage, &cv, &theme, &wall, s);
            let (pixels, w, h) = cv.finish();
            kit::save_png(&pixels, w, h, &out.join(format!("nopbar-{name}-{scale}.png")));
        }
    }
}

#[test]
#[ignore = "offscreen comparison of the glass host fallback and ready mica"]
fn harmony_glass_c_pending_review() {
    let _lang = crate::i18n::test_lock("ko");
    let out = std::path::PathBuf::from(std::env::var_os("NOPBAR_GLASS_C_RENDER_DIR").unwrap());
    let stage = kit::stage();
    for (s, scale) in [(1., 100), (1.5, 150)] {
        for (light, l, v, name) in [(false, 0.65, 224, "dark-system"), (true, 0.07, 42, "light-system")] {
            let theme = look_theme("glass", light);
            let ready = look::Wall::Glass(crate::theme::AdaptiveGlass::new(l, None));
            let cv = canvas(&stage, BAR_W, 76., s, light);
            // A uniform stand-in for the host on the first row, followed
            // by the real ready-state tint painted over its mica sample.
            let backdrop = crate::icons::Icon { width: 2, height: 2, pixels: [v, v, v, 255].repeat(4) };
            for (y, wall) in [(6., &look::Wall::Pending), (44., &ready)] {
                let g = look::ground(&theme, look::Style::Glass, look::Env::default(), wall);
                paint(&cv, &stage.gfx, &theme, &g, s, 0., y, ROWS[0], Some(&backdrop), None);
            }
            let (px, w, h) = cv.finish();
            kit::save_png(&px, w, h, &out.join(format!("review-pending-{name}-{scale}.png")));
        }
    }
}

/// The halo `h` (in the target's units) of `shape` drawn at `at`: D2D's
/// Shadow of it, `h.dy` below: the top bar's off-screen pictures'
/// (bar_harmony: the clear cells' content as the compositor's shadow,
/// halo_shadow, takes it).
unsafe fn draw_halo(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, shape: &windows::Win32::Graphics::Direct2D::ID2D1Image, at: Vector2, h: look::Halo) -> Result<()> {
    use windows::Win32::Graphics::Direct2D::Common::D2D1_COMPOSITE_MODE_SOURCE_OVER;
    use windows::Win32::Graphics::Direct2D::{CLSID_D2D1Shadow, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_PROPERTY_TYPE_FLOAT, D2D1_PROPERTY_TYPE_VECTOR4, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION, D2D1_SHADOW_PROP_COLOR};
    let fx = dc.CreateEffect(&CLSID_D2D1Shadow)?;
    fx.SetInput(0, shape, true);
    fx.SetValue(D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32, D2D1_PROPERTY_TYPE_FLOAT, &(h.blur / 2.0).to_le_bytes())?;
    let c = h.color;
    let v: Vec<u8> = [c.r, c.g, c.b, c.a].iter().flat_map(|&k| (k as f32 / 255.0).to_le_bytes()).collect();
    fx.SetValue(D2D1_SHADOW_PROP_COLOR.0 as u32, D2D1_PROPERTY_TYPE_VECTOR4, &v)?;
    dc.DrawImage(&fx.GetOutput()?, Some(&Vector2 { X: at.X, Y: at.Y + h.dy }), None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
    Ok(())
}

/// `icon` `k` times larger, pixel for pixel (to see the hinting).
fn zoomed(icon: &crate::icons::Icon, k: u32) -> crate::icons::Icon {
    let (w, h) = (icon.width * k, icon.height * k);
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let s = (((y / k) * icon.width + x / k) * 4) as usize;
            let d = ((y * w + x) * 4) as usize;
            px[d..d + 4].copy_from_slice(&icon.pixels[s..s + 4]);
        }
    }
    crate::icons::Icon { width: w, height: h, pixels: px }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_topbar_own_mark() {
    // NopBar's own tray icon as a bar shows it (tray_surface: own_mark, or
    // high contrast's one color, tint_premultiplied) at the tray sizes of
    // 100 to 200 %, on a light bar and a dark one: 1:1, then 4 times.
    let stage = kit::stage();
    let _ = &stage;
    for (folder, light) in kit::LOOKS {
        let (ink, back) = if light { (look::INK_ON_LIGHT, Rgba { r: 0xEC, g: 0xEC, b: 0xEC, a: 255 }) } else { (look::INK_ON_DARK, Rgba { r: 0x26, g: 0x26, b: 0x26, a: 255 }) };
        let sizes = [16u32, 20, 24, 32];
        let w = 12.0 + sizes.iter().map(|n| (n * 4 + 12) as f32).sum::<f32>();
        let cv = Canvas::of(stage.gfx.d2d_factory().clone(), w, 12.0 + 32.0 + 8.0 + 32.0 + 12.0 + 128.0 + 12.0, 1.0);
        unsafe { cv.dc.Clear(Some(&col(back))) };
        let mut x = 12.0;
        for n in sizes {
            let mark = own_mark(light, n).expect("the tray mark decodes");
            let icon = crate::icons::Icon { width: n, height: n, pixels: mark.clone() };
            draw_px(&cv, &icon, x, 12.0);
            let hc = crate::icons::Icon { width: n, height: n, pixels: tint_premultiplied(&mark, ink) };
            draw_px(&cv, &hc, x, 52.0);
            draw_px(&cv, &zoomed(&icon, 4), x, 96.0);
            x += (n * 4 + 12) as f32;
        }
        kit::harmony_save(cv, folder, "topbar-own-mark");
        assert!(kit::harmony_dir().join(folder).join("topbar-own-mark.png").is_file());
    }
}
