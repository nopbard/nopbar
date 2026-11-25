//! Launchpad's folder panel and edit mode drawn off screen for the design
//! audit (tests only; see crate::gfx::promo_kit, "harmony"), over its plate
//! (plate_tint) on the audit's backdrop. Everything arrange.rs draws is
//! drawn by its own paint functions:
//!
//! - the open folder's panel (paint_panel), five times: a folder as it
//!   opens (an app under the pointer), one whose border is lit by an app
//!   dragged near it, one being renamed from the keyboard (the field, the
//!   caret and the ring; an app under the keyboard), an empty one (the
//!   quiet block), and one scrolled down a row (the scroll edge under its
//!   name, gfx::scroll_fade: the band, high contrast's line; on screen
//!   popup::parts::Edges does it, FolderEdges);
//! - Done (paint_done): at rest, under the pointer, pressed, and with the
//!   keyboard's ring;
//! - the plate behind an icon a dragged one is held over (paint_drop_plate):
//!   waiting, and ready to make the folder;
//! - a folder's tile (paint_folder_tile) at its size and twice that, one of
//!   its apps not loaded yet;
//! - a picked icon, and an icon picked up over the faint copy that marks
//!   where it would go;
//! - edit mode's plate (paint_edit_plate) and the lit side edge (paint_edge).
//!
//! The pointer's ground, the keyboard's ring, a picked icon's ground and a
//! lifted icon's shadows are Composition visuals on screen (select.rs,
//! organize.rs, Pad::show_dragged): here they are drawn again from the same
//! tokens. Apps are the dock's own drawn tiles (no app of this PC), names
//! are made up. At 100 % and 150 %, Flat; Glass and high contrast at 150 %.

use std::sync::Arc;

use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_WEIGHT_NORMAL};
use windows_numerics::{Matrix3x2, Vector2};

use super::super::super::{edit, folders, grid};
use super::super::plate_tint;
use super::*;
use crate::gfx::promo_kit as kit;
use crate::glyphs::{Glyph, TileLook};
use crate::syslook::{HighContrast, SysLook};

/// The work area the panels are laid out on, and the room between things.
const WORK: (f32, f32) = (1280.0, 720.0);
const GAP: f32 = space::S7 + space::S5;
/// The last row's height (the folder's tile at twice its size is the
/// tallest thing in it).
const LAST_ROW: f32 = grid::ITEM_SIDE + space::S5;

/// The theme of `look` ("flat", "glass", "hc") in the palette (high
/// contrast: a black window, white text, a sky-blue Highlight).
fn look_theme(look: &str, light: bool) -> Theme {
    let grey = if light { 0x3A } else { 0xE6 };
    let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
    let hc = (look == "hc").then_some(HighContrast { window: black, text: white, highlight: Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 }, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT });
    let sys = SysLook { light, accent: Rgba { r: grey, g: grey, b: grey, a: 255 }, transparency: true, high_contrast: hc, animations: true, ui_font: 0 };
    let mut raw = Theme::graphite();
    // The run's finish for the sheet's own look (kit::look_finish).
    raw.finish = crate::gfx::promo_kit::look_finish(look);
    raw.resolve_fx(&sys, true)
}

/// Runs `f` with the canvas in DIPs from (`x`, `y`) at `s` px a DIP.
fn at<R>(cv: &kit::Canvas, s: f32, x: f32, y: f32, f: impl FnOnce(&ID2D1DeviceContext) -> R) -> R {
    unsafe {
        cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: x * s, M32: y * s });
        let r = f(&cv.dc);
        cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 });
        r
    }
}

/// What lights an app's cell.
#[derive(Clone, Copy, PartialEq)]
enum Lit {
    None,
    Pointer,
    Keyboard,
    Picked,
}

struct Scene<'a> {
    cv: &'a kit::Canvas,
    gfx: &'a Gfx,
    theme: &'a Theme,
    light: bool,
    s: f32,
}

impl Scene<'_> {
    fn icon(&self, g: Glyph, e: f32) -> crate::icons::Icon {
        crate::glyphs::tile((e * self.s).round() as u32, g, TileLook::new(self.light, None))
    }

    /// An app's cell as the grid lays it out: `item` its square, `icon`
    /// and `label` in it; what lights it, drawn from the tokens.
    unsafe fn cell(&self, item: Rect, icon: Rect, label: Rect, g: Glyph, name: &str, lit: Lit) {
        let (cv, theme) = (self.cv, self.theme);
        match lit {
            Lit::Pointer => match theme.outline_of(Role::Raised, State::Hover) {
                Some((c, lw)) => stroke_rr(&cv.dc, rgba(c), &inner(d2r(item), theme.radii().card, lw), lw).unwrap(),
                None => cv.fill_rr(d2r(item), theme.radii().card, super::super::select::ground_fill(theme, true)),
            },
            Lit::Picked => match theme.hc {
                // (organize.rs: Highlight behind the icon alone.)
                Some(_) => {
                    let out = crate::gfx::parts::icon_ring().0;
                    let r = Rect::new(icon.l - out, icon.t - out, icon.r + out, icon.b + out);
                    cv.fill_rr(d2r(r), r.w() * crate::theme::SQUIRCLE_ARC, theme.fill_of(Role::Row, State::Selected));
                }
                None => cv.fill_rr(d2r(item), theme.radii().card, theme.fill_of(Role::Row, State::Selected)),
            },
            _ => {}
        }
        cv.icon_at(&self.icon(g, icon.w()), icon.l, icon.t, icon.w(), true, 1.0);
        if lit == Lit::Keyboard {
            let out = super::super::select::ring_out();
            let rad = icon.w() * crate::theme::SQUIRCLE_ARC + out;
            let rr = D2D1_ROUNDED_RECT { rect: kit::rect(icon.l - out, icon.t - out, icon.r + out, icon.b + out), radiusX: rad, radiusY: rad };
            cv.dc.DrawRoundedRectangle(&rr, &cv.dc.CreateSolidColorBrush(&rgba(crate::gfx::parts::focus_color(theme)), None).unwrap(), size::FOCUS_W, None);
        }
        if let Ok((l, _)) = self.gfx.text_layout_wrapped(name, &theme.font.family, grid::LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL, label.w(), 2) {
            let b = cv.dc.CreateSolidColorBrush(&rgba(theme.text_of(Level::Primary, State::Rest)), None).unwrap();
            cv.dc.DrawTextLayout(Vector2 { X: label.l, Y: label.t }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
    }
}

fn sheet(stage: &kit::Stage, look: &str, light: bool, s: f32) -> kit::Canvas {
    let theme = look_theme(look, light);
    let gfx = &*stage.gfx;
    let dw: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap() };
    // The panels' grid first: the picture is as big as what it shows.
    let g = grid::grid(WORK.0, WORK.1, grid::LABEL_H);
    let pg = folders::panel_grid(3, &g, WORK.0, WORK.1);
    let pr = folders::panel_rect(&pg);
    let (pw, ph) = (pr.w(), pr.h());
    let (gap, x0) = (GAP, GAP);
    let (w, h) = (pw * 2.0 + gap * 3.0, gap + (ph + gap) * 3.0 + size::CONTROL_H + gap + LAST_ROW + gap);
    let cv = kit::Canvas::of(gfx.d2d_factory().clone(), w, h, s);
    let sc = Scene { cv: &cv, gfx, theme: &theme, light, s };
    let glyphs = [Glyph::Document, Glyph::Globe, Glyph::Music, Glyph::Image, Glyph::Terminal, Glyph::Calendar];
    let names = ["문서", "브라우저", "음악", "사진", "터미널", "달력"];
    unsafe {
        // The desktop (the audit's backdrop), then the plate over it.
        cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
        cv.dc.FillRectangle(&kit::rect(0.0, 0.0, w, h), &cv.dc.CreateSolidColorBrush(&plate_tint(&theme), None).unwrap());

        // ---- the open folder's panel, five times ----
        // (name, being renamed, the border lit, apps, which is lit and how)
        let panels: [(&str, bool, bool, usize, Option<(usize, Lit)>); 5] = [
            ("작업 도구", false, false, 3, Some((1, Lit::Pointer))),
            ("작업 도구", false, true, 3, None),
            ("새 폴더", true, false, 2, Some((0, Lit::Keyboard))),
            ("빈 폴더", false, false, 0, None),
            ("스크롤된 폴더", false, false, 3, Some((0, Lit::Pointer))),
        ];
        for (k, (name, typing, lit, n, lit_app)) in panels.into_iter().enumerate() {
            let (x, y) = (x0 + (k % 2) as f32 * (pw + gap), gap + (k / 2) as f32 * (ph + gap));
            let laid = name_layout(gfx, &dw, &theme, name, pw - 2.0 * folders::PANEL_INK, false);
            let row = match typing {
                // (The caret after the name's last character.)
                true => NameRow::Typing { caret: laid.as_ref().map(|(l, _)| Pad::x_at(l, name.encode_utf16().count() as u32)), ring: true },
                false => NameRow::Plain,
            };
            let empty = (n == 0).then(|| crate::i18n::t("dock.stack.empty"));
            let panel = PanelLook { name: laid.as_ref().map(|(l, h)| (l, *h)), row, lit, empty, shadows: true, coat: panel_coat(&theme, s) };
            at(&cv, s, x, y, |dc| paint_panel(gfx, dc, pw, ph, &theme, &panel)).unwrap();
            let (dx, dy) = (x - pr.l, y - pr.t);
            let cells = || {
                for slot in 0..n {
                    let how = lit_app.filter(|(a, _)| *a == slot).map_or(Lit::None, |(_, l)| l);
                    // (Scrolled: the row after the first.)
                    let g = if k == 4 { glyphs[3 + slot % 3] } else { glyphs[slot] };
                    let name = if k == 4 { names[3 + slot % 3] } else { names[slot] };
                    sc.cell(pg.item(slot, false).offset(dx, dy), pg.icon(slot, false).offset(dx, dy), pg.label(slot, false).offset(dx, dy), g, name, how);
                }
                Ok(())
            };
            if k == 4 {
                // A row scrolled away above: the band under the name.
                crate::gfx::scroll_fade(&cv.dc, d2r(folder_scroll_area(&pg).offset(dx, dy)), true, false, &theme, 1.0, cells).unwrap();
            } else {
                cells().unwrap();
            }
        }

        // ---- Done: at rest, under the pointer, pressed, the keyboard's ring ----
        let y2 = gap + (ph + gap) * 3.0;
        let (l, tw, th) = gfx.text_style(crate::i18n::t("launchpad.edit.done"), &theme.font.family, ramp::BODY_STRONG, 1.0).unwrap();
        let (dw_, dh) = done_size(tw);
        for (k, (state, focus)) in [(State::Rest, false), (State::Hover, false), (State::Pressed, false), (State::Rest, true)].into_iter().enumerate() {
            at(&cv, s, x0 + k as f32 * (dw_ + space::S6), y2, |dc| paint_done(dc, dw_, dh, (&l, tw, th), &theme, state, true, focus)).unwrap();
        }

        // ---- the plate behind a target: waiting, ready ----
        let e = grid::ICON;
        let side = e * edit::PLATE;
        let y3 = y2 + dh + gap;
        for (k, ready) in [false, true].into_iter().enumerate() {
            let x = x0 + k as f32 * (side + space::S6);
            at(&cv, s, x, y3, |dc| paint_drop_plate(dc, side, &theme, ready)).unwrap();
            let t = e * edit::TARGET_SCALE;
            cv.icon_at(&sc.icon(glyphs[k], t), x + (side - t) / 2.0, y3 + (side - t) / 2.0, t, true, 1.0);
        }

        // ---- a folder's tile: at its size, and twice that ----
        let minis: Vec<Option<Arc<crate::icons::Icon>>> = (0..5).map(|k| (k != 3).then(|| Arc::new(sc.icon(glyphs[k], e)))).collect();
        let mut x = x0 + (side + space::S6) * 2.0 + space::S6;
        for te in [e, e * 2.0] {
            at(&cv, s, x, y3, |dc| paint_folder_tile(dc, te, &minis, &theme, false)).unwrap();
            x += te + space::S6;
        }

        // ---- a picked icon ----
        x += space::S6;
        let item = Rect::new(x, y3, x + grid::ITEM_SIDE, y3 + grid::ITEM_SIDE);
        let icon = Rect::new(item.cx() - e / 2.0, item.t + grid::ITEM_PAD, item.cx() + e / 2.0, item.t + grid::ITEM_PAD + e);
        let label = Rect::new(item.l + grid::ITEM_PAD, icon.b + grid::LABEL_GAP, item.r - grid::ITEM_PAD, item.b);
        sc.cell(item, icon, label, Glyph::Camera, "카메라", Lit::Picked);

        // ---- an icon picked up: the faint copy where it would go, the
        // icon LIFT_SCALE bigger with the shadows of elevation E3 ----
        x = item.r + space::S7;
        cv.icon_at(&sc.icon(Glyph::Timer, e), x, y3, e, true, edit::GHOST_OPACITY);
        let le = e * edit::LIFT_SCALE;
        let (lx, ly) = (x + e * 0.75, y3 + space::S5);
        if crate::effects::items().shadows {
            crate::gfx::soft_shadows(&cv.dc, kit::rect(lx, ly, lx + le, ly + le), le * crate::theme::SQUIRCLE_ARC, &theme.elevation(Elevation::E3)).unwrap();
        }
        cv.icon_at(&sc.icon(Glyph::Timer, le), lx, ly, le, true, 1.0);

        // ---- edit mode's plate and the lit side edge ----
        let (ew, eh) = (220.0, 132.0);
        x = lx + le + space::S7;
        at(&cv, s, x, y3, |dc| paint_edit_plate(dc, ew, eh, &theme)).unwrap();
        for k in 0..2 {
            cv.icon_at(&sc.icon(glyphs[4 + k], e), x + edit::PANEL_MARGIN + space::S5 + k as f32 * (e + space::S7), y3 + (eh - e) / 2.0, e, true, 1.0);
        }
        at(&cv, s, w - edit::EDGE, y3 - space::S3, |dc| paint_edge(dc, edit::EDGE, eh + space::S5, &theme)).unwrap();
    }
    cv
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_launchpad_folder() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    for (folder, light) in kit::LOOKS {
        for (look, s, name) in [("flat", 1.0, "launchpad-folder-100"), ("flat", 1.5, "launchpad-folder-150"), ("glass", 1.5, "launchpad-folder-glass-150"), ("hc", 1.5, "launchpad-folder-hc-150")] {
            kit::harmony_save(sheet(&stage, look, light, s), folder, name);
            assert!(kit::harmony_dir().join(folder).join(format!("{name}.png")).is_file());
        }
    }
}

/// The Composition animations arrange.rs builds start (their keyframe
/// expressions parse, their properties exist) on a real compositor: what
/// the pictures cannot show. Nothing is drawn.
#[test]
#[ignore = "needs a compositor: cargo test -p dock harmony_ -- --ignored"]
fn harmony_launchpad_animations_start() {
    let stage = kit::stage();
    let c = &stage.gfx.compositor;
    let v = c.CreateSpriteVisual().unwrap();
    let at = |x: f32| Vector3 { X: x, Y: 0.0, Z: 0.0 };
    // A place or a size on GLIDE, from a known start and from where it is.
    glide(c, &v, "Offset", Some(at(0.0)), at(40.0)).expect("offset from a start");
    glide(c, &v, "Offset", None, at(80.0)).expect("offset from where it is");
    glide(c, &v, "Scale", Some(v3(1.0)), v3(edit::LIFT_SCALE)).expect("scale from a start");
    glide(c, &v, "Scale", None, v3(edit::EDIT_SCALE)).expect("scale from where it is");
    glide_opacity(c, &v, Some(0.0), 1.0).expect("opacity from a start");
    glide_opacity(c, &v, None, 0.0).expect("opacity from where it is");
    // A tile bound to another cell: gliding, and (not animating) fading in.
    slide(c, &v, at(0.0), at(10.0), true).expect("glides");
    slide(c, &v, at(0.0), at(10.0), false).expect("comes in in place");
    // Fades as crate::motion plans them, and a plan of no length.
    assert!(fade_v(c, &v, None, 0.0, crate::motion::fade_plan(crate::motion::Mode::Full, crate::motion::FADE_OUT_MS, crate::motion::EASE_IN, 1.0)));
    assert!(fade_v(c, &v, Some(0.0), 1.0, crate::motion::fade_plan(crate::motion::Mode::Reduced, crate::motion::FADE_IN_MS, crate::motion::EASE_OUT, 1.0)));
    assert!(!fade_v(c, &v, None, 1.0, (0, crate::motion::LINEAR)), "no length: set at once");
    assert_eq!(v.Opacity().unwrap(), 1.0);
    // A fill fading between two colors.
    let brush = c.CreateColorBrushWithColor(Color { A: 0, R: 255, G: 255, B: 255 }).unwrap();
    let white = Color { A: 255, R: 255, G: 255, B: 255 };
    color_to(c, &brush, Some(Color { A: 0, ..white }), white, (90, crate::motion::EASE_OUT));
    color_to(c, &brush, None, white, (0, crate::motion::LINEAR));
    assert_eq!(brush.Color().unwrap(), white);
    // A scrolling folder's edge: its cells' view goes into Edges (made in
    // and out of high contrast), placed over its area; the bands follow.
    for look in ["flat", "hc"] {
        let theme = look_theme(look, true);
        let cells = c.CreateContainerVisual().unwrap();
        let area = Rect::new(120.0, 200.0, 760.0, 540.0);
        let mut e = folder_edges(&stage.gfx, &theme, &cells, area, 1.5, (false, false)).expect(look);
        assert_eq!(e.edges.stage.Offset().unwrap(), Vector3 { X: 120.0, Y: 200.0, Z: 0.0 });
        assert_eq!(cells.Offset().unwrap(), Vector3 { X: -120.0, Y: -200.0, Z: 0.0 });
        e.edges.set(c, (true, false), theme.high_contrast);
        e.edges.set(c, (false, false), theme.high_contrast);
    }
}
