//! The dock drawn off screen for the design audit (tests only; see
//! crate::gfx::promo_kit, "harmony"). On screen the bar, the icons, the
//! dots, the label and the rings are compositor visuals, so each is put
//! together here from what the dock computes them with: the bar from
//! layout::bar_box and Look::radius, the magnification from
//! layout::magnified, the dots from layout::indicator_v, the label from
//! label_colors / label_radius / label::edge_stroke and Frame::label_at,
//! the rings from gfx::parts::icon_ring. The tiles are the film's
//! (dock::promo: glyphs::tile, cut to the squircle); their names are made
//! up. What is checked by eye: the same padding on every side, corners
//! concentric with the icons' at every icon size, the rim, the label, and
//! the states (hover, pressed, the keyboard's focus), in each look, on each
//! edge, at 100 % and 150 %.

use windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE;
use windows_numerics::Matrix3x2;

use super::*;
use crate::config::Position;
use crate::gfx::promo_kit::{self as kit, col, rect, Canvas};
use crate::glyphs::TileLook;
use crate::syslook::{HighContrast, SysLook};
use crate::theme::{Elevation, Rgba, Theme};

/// Between two docks of a sheet, and around it (DIP).
const GAP: f32 = 24.0;
/// Room past the bar's ends for its shadow (DIP).
const ROOM: f32 = 56.0;

/// One dock of a sheet.
#[derive(Clone, Copy)]
struct Scene {
    pos: Position,
    icon: f32,
    /// How many of the film's items it shows.
    count: usize,
    /// The pointer rests on this item: magnified, named.
    hover: Option<usize>,
    pressed: Option<usize>,
    /// The keyboard's focus is on this item (magnified and named too).
    focus: Option<usize>,
    /// The site's dot indicators and neutral fixtures.
    site: bool,
}

impl Scene {
    fn rest(icon: f32) -> Scene {
        Scene { pos: Position::Bottom, icon, count: usize::MAX, hover: None, pressed: None, focus: None, site: false }
    }
}

/// The theme of `look` ("flat", "glass", "hc") in the palette (as
/// gfx::parts' token sheets resolve it).
fn look_theme(look: &str, light: bool) -> Theme {
    let grey = if light { 0x3A } else { 0xE6 };
    let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
    let hc = (look == "hc").then_some(HighContrast { window: black, text: white, highlight: Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 }, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT });
    let sys = SysLook { light, accent: Rgba { r: grey, g: grey, b: grey, a: 255 }, transparency: true, high_contrast: hc, animations: true, ui_font: 0 };
    let mut raw = Theme::graphite();
    // The run's finish for the sheet's own look (kit::look_finish).
    raw.finish = crate::gfx::promo_kit::look_finish(look);
    raw.resolve_fx(&sys, true).windows_shell()
}

fn look_of(sc: &Scene) -> Look {
    let mut cfg = crate::config::Config::default().dock;
    cfg.icon_size = sc.icon;
    if sc.site {
        cfg.indicator = Indicator::Dot;
        cfg.max_scale = 1.45;
    }
    Look::from_cfg(&cfg, true)
}

fn list(sc: &Scene) -> Vec<Option<(&'static str, crate::glyphs::Glyph, &'static str, bool, bool)>> {
    let all: Vec<_> = promo::items().into_iter().filter(|it| !matches!(it, Some(("trash", ..))) || crate::config::Config::default().dock.show_trash).collect();
    if sc.count >= all.len() {
        return all;
    }
    // A short dock keeps the separator and what follows it.
    let mut v: Vec<_> = all.iter().take(sc.count.saturating_sub(2)).cloned().collect();
    v.extend(all.iter().skip(all.len() - 3).cloned());
    v
}

/// The dock's window for a scene, DIP: (along, depth). Room for the
/// largest icons and the label on the far side.
fn window(sc: &Scene) -> (f32, f32) {
    let look = look_of(sc);
    let items = list(sc);
    let total: f32 = items.iter().map(|it| if it.is_some() { look.icon } else { SEP_W } + look.gap).sum::<f32>() - look.gap;
    let grown = if sc.hover.or(sc.focus).is_some() { look.icon * (look.max_scale - 1.0) * 2.5 } else { 0.0 };
    let head = match (sc.hover.or(sc.focus).is_some(), sc.pos) {
        (false, _) => 24.0,
        (true, Position::Bottom) => look.icon * (look.max_scale - 1.0) + 48.0,
        (true, _) => look.icon * (look.max_scale - 1.0) + 96.0,
    };
    (total + PAD_AXIS * 2.0 + grown + ROOM * 2.0, look.icon + PAD_CROSS * 2.0 + EDGE_GAP + head)
}

/// A sheet's canvas transform: DIPs at `s`.
fn dips(cv: &Canvas, s: f32) {
    unsafe { cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 }) };
}

/// A plate at (`x`, `y`): shadows, the contact ring, the tint, the rim or
/// high contrast's outline (the order the dock and its label stack them).
#[allow(clippy::too_many_arguments)]
fn plate(cv: &Canvas, s: f32, theme: &Theme, m: crate::theme::Material, x: f32, y: f32, w: f32, h: f32, radius: f32, tint: Rgba, shadows: &[(f32, f32, f32, Rgba)], outline: Option<Rgba>, wallpaper: Option<&kit::GlassWallpaper<'_>>) {
    let r = rect(x, y, x + w, y + h);
    unsafe { crate::gfx::soft_shadows(&cv.dc, r, radius, shadows).unwrap() };
    let contact = theme.contact();
    if contact.a > 0 {
        let k = crate::theme::CONTACT_W;
        cv.stroke_rr(rect(x - k / 2.0, y - k / 2.0, x + w + k / 2.0, y + h + k / 2.0), radius + k / 2.0, contact, k);
    }
    if let Some(wall) = wallpaper {
        wall.draw(cv, r, radius, theme, m);
    }
    // Flat v2: its fall and its grain (the dock's tint layer, glass::
    // top_light).
    unsafe { crate::gfx::parts::plate_fill(&cv.dc, r, radius, theme.flat_fill(m), tint).unwrap() };
    let (spec, grain) = if wallpaper.is_some() { crate::glass::light_of(theme, m) } else { (0.0, theme.flat_grain(m)) };
    if spec > 0.0 {
        kit::draw_top_light(cv, r, radius, spec);
    }
    if grain > 0.0 {
        kit::draw_grain(cv, r, radius, grain, s);
    }
    if let Some(stops) = theme.rim_of(m) {
        kit::draw_rim_px(cv, x, y, w, h, radius, stops, theme.rim_width(m, s));
    }
    if let Some(o) = outline.filter(|o| o.a > 0) {
        cv.stroke_rr(rect(x + 0.5, y + 0.5, x + w - 0.5, y + h - 0.5), (radius - 0.5).max(0.0), o, 1.0);
    }
}

/// Draws one dock with its window's top left at (`ox`, `oy`) DIP.
fn paint(cv: &Canvas, stage: &kit::Stage, s: f32, theme: &Theme, light: bool, ox: f32, oy: f32, sc: &Scene) {
    paint_on(cv, stage, s, theme, light, ox, oy, sc, None);
}

#[allow(clippy::too_many_arguments)]
fn paint_on(cv: &Canvas, stage: &kit::Stage, s: f32, theme: &Theme, light: bool, ox: f32, oy: f32, sc: &Scene, wallpaper: Option<&kit::GlassWallpaper<'_>>) {
    let look = look_of(sc);
    let items = list(sc);
    let (along, depth) = window(sc);
    let f = Frame { pos: sc.pos, depth };
    let at = |r: Rect| {
        let w = f.to_win(r);
        Rect { x: w.x + ox, y: w.y + oy, ..w }
    };
    // At rest: where each item's center is (Dock::base_center).
    let base = |it: &Option<_>| if it.is_some() { look.icon } else { SEP_W };
    let rest_total: f32 = items.iter().map(|it| base(it) + look.gap).sum::<f32>() - look.gap;
    let mut centers = Vec::new();
    let mut u = (along - rest_total) / 2.0;
    for it in &items {
        centers.push(u + base(it) / 2.0);
        u += base(it) + look.gap;
    }
    // The pointer (or the keyboard's focus, shown the same way) on one.
    let lit = sc.hover.or(sc.focus);
    let widths: Vec<f32> = items
        .iter()
        .enumerate()
        .map(|(i, it)| match (it, lit) {
            (Some(_), Some(h)) => look.icon * layout::magnified((centers[h] - centers[i]).abs() / look.range, look.max_scale),
            (Some(_), None) => look.icon,
            (None, _) => SEP_W,
        })
        .collect();
    let total: f32 = widths.iter().map(|w| w + look.gap).sum::<f32>() - look.gap;
    let bar = layout::bar_box(total, look.icon, along, depth);
    let bw = at(bar);
    // Dock::build's bar: its shadows, tint, rim (high contrast: its border).
    let shadows = if theme.high_contrast { Vec::new() } else { theme.dock_shadows(false) };
    plate(cv, s, theme, crate::theme::Material::Dock, bw.x, bw.y, bw.w, bw.h, look.radius, theme.dock_tint(), &shadows, Some(theme.dock_border()), wallpaper);
    let baseline = bar.y + PAD_CROSS + look.icon;
    let hc_pair = theme.hc.filter(|_| theme.high_contrast).map(|h| ([h.window.r, h.window.g, h.window.b], [h.text.r, h.text.g, h.text.b]));
    let mut u = bar.x + PAD_AXIS;
    let mut named = None;
    for (i, (it, w)) in items.iter().zip(&widths).enumerate() {
        match it {
            Some((_, g, name, running, two)) => {
                let r = at(Rect { x: u, y: baseline - w, w: *w, h: *w });
                let px = (w * s).round().max(8.0) as u32;
                // Pressed: the icon shrinks around its center.
                let press = layout::press_scale(sc.pressed == Some(i), None, (0.0, crate::motion::EASE_OUT));
                let active = *running && *two;
                cv.fill_rr(rect(r.x - 3.0, r.y - 3.0, r.x + r.w + 3.0, r.y + r.h + 3.0), 4.0, layout::tile_color(theme, sc.hover == Some(i), active));
                let inset = r.w * (1.0 - press) / 2.0;
                let tile = crate::glyphs::tile(px, *g, TileLook::new(light, hc_pair).finish(!theme.glass()));
                let tile = if sc.site && *g != crate::glyphs::Glyph::Mark {
                    crate::desklayer::backdrop::saturated(&tile, 0.0)
                } else {
                    tile
                };
                cv.icon_at(&tile, r.x + inset, r.y + inset, r.w * press, look.squircle, 1.0);
                if sc.pressed == Some(i) {
                    if let Some(hl) = theme.focus_outline() {
                        unsafe { crate::gfx::parts::paint_squircle_ring(&cv.dc, &cv.factory, r.x, r.y, r.w, 2.0, hl).unwrap() };
                    }
                }
                if sc.focus == Some(i) {
                    let (out, stroke) = crate::gfx::parts::icon_ring();
                    let (x, y, side) = ring::ring_box(r.x, r.y, r.w, out);
                    unsafe { crate::gfx::parts::paint_squircle_ring(&cv.dc, &cv.factory, x, y, side, stroke, crate::gfx::parts::focus_color(theme)).unwrap() };
                }
                if *running {
                    let ind = indicator_size(look.indicator, active, *two);
                    let d = at(Rect { x: u + w / 2.0 - ind.X / 2.0, y: layout::indicator_v(bar, ind.Y), w: ind.X, h: ind.Y });
                    if look.indicator == Indicator::Bar {
                        cv.fill_rr(rect(d.x, d.y, d.x + d.w, d.y + d.h), 1.5, theme.indicator_color());
                    } else { unsafe {
                        let b = cv.dc.CreateSolidColorBrush(&col(theme.indicator_color()), None).unwrap();
                        for k in 0..if *two { 2 } else { 1 } {
                            let step = k as f32 * (DOT + DOT_GAP);
                            let (cx, cy) = if f.vertical() { (d.x + DOT / 2.0, d.y + DOT / 2.0 + step) } else { (d.x + DOT / 2.0 + step, d.y + DOT / 2.0) };
                            cv.dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: DOT / 2.0, radiusY: DOT / 2.0 }, &b);
                        }
                    } }
                }
                if lit == Some(i) {
                    named = Some((*name, u + w / 2.0, baseline - w));
                }
            }
            None => {
                // Dock::layout's separator: one device pixel, half an icon long.
                let (top, h) = dividers::line_span(baseline, look.icon);
                let px = 1.0 / s;
                let l = at(Rect { x: ((u + w / 2.0) * s).round() / s - px / 2.0, y: top, w: px, h });
                cv.fill_rr(rect(l.x, l.y, l.x + l.w, l.y + l.h), 0.0, theme.dock_separator());
            }
        }
        u += w + look.gap;
    }
    // The label (label.rs show_label): beyond the icon's far edge.
    if let Some((name, u, top)) = named {
        let a = stage.gfx.label_art(name, s).expect("the label");
        let (w, h) = (a.w / s, a.h / s);
        let (x, y) = f.label_at(u, top, w, h, label::LABEL_GAP);
        let (x, y) = label::snap_px(x + ox, y + oy, s);
        let (bg, fg) = theme.label_colors();
        let shadows = if theme.high_contrast { Vec::new() } else { theme.elevation(Elevation::E1) };
        plate(cv, s, theme, crate::theme::Material::Tip, x, y, w, h, label::label_radius(theme, h), bg, &shadows, theme.high_contrast.then_some(fg), None);
        unsafe {
            cv.dc.SetTransform(&Matrix3x2::identity());
            crate::gfx::paint_label(&cv.dc, x * s, y * s, &a, Rgba { a: 0, ..bg }, fg, theme.radii().tip).expect("painted");
        }
        dips(cv, s);
    }
}

pub(crate) fn paint_site_dock(
    cv: &Canvas,
    stage: &kit::Stage,
    theme: &Theme,
    wall: &kit::GlassWallpaper<'_>,
    s: f32,
    hovered: bool,
) -> RECT {
    let scene = Scene { hover: hovered.then_some(4), site: true, ..Scene::rest(44.0) };
    let (w, h) = window(&scene);
    let x = (cv.w as f32 / s - w) / 2.0;
    let y = cv.h as f32 / s - h - 12.0;
    dips(cv, s);
    paint_on(cv, stage, s, theme, theme.is_light, x, y, &scene, Some(wall));
    RECT { left: (x * s).floor() as i32, top: ((y - GAP) * s).floor() as i32, right: ((x + w) * s).ceil() as i32, bottom: cv.h as i32 }
}

/// A sheet of docks in `look` at scale `s`: bottom docks one under the
/// other, side docks side by side.
fn sheet(name: &str, look: &str, s: f32, scenes: &[Scene]) {
    let stage = kit::stage();
    let side = scenes.iter().any(|sc| sc.pos != Position::Bottom);
    for (folder, light) in kit::LOOKS {
        let theme = look_theme(look, light);
        // Each scene's window on the sheet: (x, y, w, h).
        let mut boxes = Vec::new();
        let (mut x, mut y, mut w, mut h) = (GAP, GAP, 0.0f32, 0.0f32);
        for sc in scenes {
            let (along, depth) = window(sc);
            let (bw, bh) = if sc.pos == Position::Bottom { (along, depth) } else { (depth, along) };
            boxes.push((x, y));
            if side {
                x += bw + GAP;
                h = h.max(bh);
                w = x;
            } else {
                y += bh + GAP;
                w = w.max(bw + GAP * 2.0);
                h = y;
            }
        }
        if side {
            h += GAP * 2.0;
        }
        let cv = Canvas::of(stage.gfx.d2d_factory().clone(), w, h, s);
        unsafe { cv.dc.Clear(Some(&col(kit::harmony_backdrop(light)))) };
        for (sc, (bx, by)) in scenes.iter().zip(boxes) {
            // (Bottom docks share the sheet's middle.)
            let bx = if side { bx } else { (w - window(sc).0) / 2.0 };
            paint(&cv, &stage, s, &theme, light, bx, by, sc);
        }
        kit::harmony_save(cv, folder, name);
        assert!(kit::harmony_dir().join(folder).join(format!("{name}.png")).is_file());
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_dock() {
    let _l = crate::i18n::test_lock("ko");
    let b = Scene::rest(52.0);
    let states = [
        Scene { hover: Some(4), ..b },
        Scene { pressed: Some(2), ..b },
        Scene { focus: Some(7), ..b },
    ];
    let sides = [
        Scene { pos: Position::Left, count: 7, hover: Some(2), ..b },
        Scene { pos: Position::Left, count: 7, ..b },
        Scene { pos: Position::Right, count: 7, ..b },
        Scene { pos: Position::Right, count: 7, hover: Some(2), ..b },
    ];
    for (tag, s) in [("100", 1.0f32), ("150", 1.5)] {
        // Every icon size: the same padding, corners concentric.
        sheet(&format!("dock-rest-{tag}"), "flat", s, &[Scene::rest(36.0), b, Scene::rest(96.0)]);
        sheet(&format!("dock-states-{tag}"), "flat", s, &states);
        sheet(&format!("dock-side-{tag}"), "flat", s, &sides);
    }
    // The other looks (12.4): Glass (tint and overlap only: no blur off
    // screen), high contrast.
    sheet("dock-glass-150", "glass", 1.5, &[b, states[0]]);
    sheet("dock-hc-150", "hc", 1.5, &[b, states[0], states[1], states[2]]);
    // Close up: the bar's corners around the first and last icons, at the
    // smallest, the default and a large icon size, and the largest.
    let near = |icon: f32| Scene { count: 3, ..Scene::rest(icon) };
    sheet("dock-corners", "flat", 4.0, &[near(24.0), near(36.0), near(52.0), near(96.0), near(128.0)]);
}

#[test]
#[ignore = "Windows feel offscreen dock states; no capture or input"]
fn windows_feel_dock() {
    let _l = crate::i18n::test_lock("ko");
    let base = Scene::rest(44.0);
    sheet("windows-dock", "glass", 1.0, &[base, Scene { hover: Some(4), ..base }, Scene { pressed: Some(2), ..base }]);
}

pub(crate) fn render_readme_dock(path: &std::path::Path) {
    use crate::desklayer::graphite_snapshots as snapshots;
    use windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR;

    let stage = kit::stage();
    let scene = Scene { hover: Some(4), ..Scene::rest(44.0) };
    let (w, h) = window(&scene);
    let cv = Canvas::of(stage.gfx.d2d_factory().clone(), w + GAP * 2.0, h + GAP * 2.0, 1.0);
    let wall = snapshots::wall(snapshots::Wall::Bright, cv.w, cv.h, 1.0);
    unsafe {
        let bitmap = snapshots::bitmap(&cv.dc, &wall);
        cv.dc.DrawBitmap(&bitmap, None, 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
    }
    paint(&cv, &stage, 1.0, &look_theme("glass", true), true, GAP, GAP, &scene);
    let (pixels, w, h) = cv.finish();
    kit::save_png(&pixels, w, h, path);
}
