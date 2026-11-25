//! Launchpad's states drawn off screen for the design audit (tests only;
//! see crate::gfx::promo_kit, "harmony"), over its plate (plate_tint) on
//! the audit's backdrop:
//!
//! - the search field (paint_field_plate, the overlay's own paint): at
//!   rest with its placeholder, with the keyboard's ring, with text;
//! - an app's cell at rest, under the pointer (the ground), under the
//!   keyboard (the ring), pressed, and still loading (the placeholder);
//! - the quiet states (paint_quiet, the overlay's own): nothing found, the
//!   list not read (with its button), the list being read (the ring and
//!   its late line);
//! - the page dots.
//!
//! The ground and the ring are Composition shapes on screen (select.rs):
//! here they are drawn again from the same tokens. Apps are the dock's own
//! drawn tiles (no app of this PC). At 100 % and 150 %, Flat; Glass and
//! high contrast at 150 %.

use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
use windows_numerics::{Matrix3x2, Vector2};

use super::super::grid;
use super::{paint_field_plate, paint_quiet, plate_tint, rgba, FAILED_LINE, FIELD_PAD, FIELD_TEXT, G_SEARCH};
use crate::gfx::parts::Quiet;
use crate::gfx::promo_kit as kit;
use crate::glyphs::{Glyph, TileLook};
use crate::i18n::t;
use crate::syslook::{HighContrast, SysLook};
use crate::theme::metrics::{size, space};
use crate::theme::{ramp, Level, Rgba, Role, State, Theme};

/// The picture (DIP).
const W: f32 = 880.0;
const H: f32 = 560.0;

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
    raw.resolve_fx(&sys, true).windows_shell()
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

fn sheet(stage: &kit::Stage, look: &str, light: bool, s: f32) -> kit::Canvas {
    let theme = look_theme(look, light);
    let gfx = &stage.gfx;
    let fam = theme.font.family.clone();
    let cv = kit::Canvas::of(gfx.d2d_factory().clone(), W, H, s);
    let ink = |l: Level| rgba(theme.text_of(l, State::Rest));
    unsafe {
        // The desktop (the audit's backdrop), then the plate over it.
        cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
        let plate = cv.dc.CreateSolidColorBrush(&plate_tint(&theme), None).unwrap();
        cv.dc.FillRectangle(&kit::rect(0.0, 0.0, W, H), &plate);
        let text = |dc: &ID2D1DeviceContext, what: &str, x: f32, y: f32, c: D2D1_COLOR_F| {
            let (l, tw, th) = gfx.text_style(what, &fam, ramp::BODY, 1.0).unwrap();
            dc.DrawTextLayout(Vector2 { X: x, Y: y - th / 2.0 }, &l, &dc.CreateSolidColorBrush(&c, None).unwrap(), D2D1_DRAW_TEXT_OPTIONS_NONE);
            tw
        };

        // ---- the field: at rest, the keyboard's ring, with text ----
        let (fw, fh) = (260.0, grid::FIELD_H);
        let lead = FIELD_PAD + size::ICON_S + space::S3;
        for (k, (ring, typed)) in [(false, None), (true, None), (false, Some("메모"))].into_iter().enumerate() {
            at(&cv, s, space::S6 + k as f32 * (fw + space::S6), space::S6, |dc| {
                paint_field_plate(dc, fw, fh, &theme, ring, false, ink(Level::Secondary)).unwrap();
                match typed {
                    Some(q) => {
                        let tw = text(dc, q, lead, fh / 2.0, ink(Level::Primary));
                        // (The caret after it.)
                        let half = ramp::BODY.line / 2.0;
                        dc.FillRectangle(&kit::rect(lead + tw, fh / 2.0 - half + 2.0, lead + tw + 1.5, fh / 2.0 + half - 2.0), &dc.CreateSolidColorBrush(&ink(Level::Primary), None).unwrap());
                    }
                    None => {
                        text(dc, t("launchpad.placeholder"), lead, fh / 2.0, ink(Level::Tertiary));
                    }
                }
            });
        }
        let _ = FIELD_TEXT;

        // ---- an app's cell: rest, pointer, keyboard, pressed, loading ----
        let (side, top) = (grid::ITEM_SIDE, 96.0);
        let glyphs = [Glyph::Folder, Glyph::Globe, Glyph::Music, Glyph::Image, Glyph::Document];
        let names = ["파일", "브라우저", "음악", "사진", "문서"];
        for k in 0..5usize {
            let x = space::S6 + k as f32 * (side + space::S6);
            let (ix, iy) = (x + (side - grid::ICON) / 2.0, top + grid::ITEM_PAD);
            // Under the pointer: the ground behind the cell.
            if k == 1 {
                let r = kit::rect(x, top, x + side, top + side);
                match theme.hc {
                    Some(h) => cv.stroke_rr(kit::rect(x + 1.0, top + 1.0, x + side - 1.0, top + side - 1.0), theme.radii().card - 1.0, h.highlight, 2.0),
                    None => cv.fill_rr(r, theme.radii().card, theme.fill_of(Role::Raised, State::Hover)),
                }
            }
            let px = (grid::ICON * s).round() as u32;
            if k == 4 {
                // Still loading: the placeholder (Pad::placeholder).
                match theme.outline_of(Role::Raised, State::Rest) {
                    Some((c, w)) => crate::gfx::parts::paint_squircle_ring(&cv.dc, &cv.factory, ix, iy, grid::ICON, w, c).unwrap(),
                    // (Raised: Flat v2's card light round it, inside its place.)
                    None => super::arrange::raised_squircle(&cv.dc, ix, iy, grid::ICON, &theme, None, 1.0).unwrap(),
                }
            } else {
                let icon = crate::glyphs::tile(px, glyphs[k], TileLook::new(light, None));
                // Pressed: dimmed (not in high contrast).
                let dim = if k == 3 && theme.hc.is_none() { crate::motion::PRESS_DIM } else { 1.0 };
                cv.icon_at(&icon, ix, iy, grid::ICON, true, dim);
            }
            // Under the keyboard: the ring round the icon.
            if k == 2 {
                let out = super::select::ring_out();
                let rr = D2D1_ROUNDED_RECT { rect: kit::rect(ix - out, iy - out, ix + grid::ICON + out, iy + grid::ICON + out), radiusX: grid::ICON * crate::theme::SQUIRCLE_ARC + out, radiusY: grid::ICON * crate::theme::SQUIRCLE_ARC + out };
                cv.dc.DrawRoundedRectangle(&rr, &cv.dc.CreateSolidColorBrush(&rgba(crate::gfx::parts::focus_color(&theme)), None).unwrap(), size::FOCUS_W, None);
            }
            let lw = side - grid::ITEM_PAD * 2.0;
            if let Ok((l, _)) = gfx.text_layout_wrapped(names[k], &fam, grid::LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL, lw, 2) {
                let b = cv.dc.CreateSolidColorBrush(&ink(Level::Primary), None).unwrap();
                cv.dc.DrawTextLayout(Vector2 { X: x + grid::ITEM_PAD, Y: iy + grid::ICON + grid::LABEL_GAP }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }

        // ---- the quiet states ----
        let (qw, qy) = ((W - space::S6 * 4.0) / 3.0, 264.0);
        let blocks: [(Quiet, Glyph, &str, Option<&str>); 2] =
            [(Quiet::Empty, G_SEARCH, t("launchpad.no_results"), None), (Quiet::Error, Glyph::Warning, t(FAILED_LINE), Some(t("launch.action.retry")))];
        for (k, (kind, glyph, line, button)) in blocks.into_iter().enumerate() {
            at(&cv, s, space::S6 + k as f32 * (qw + space::S6), qy, |dc| paint_quiet(gfx, dc, qw, kind, glyph, line, button, &theme).unwrap());
        }
        // The list being read, late: the ring (a quarter turn in) and its line.
        at(&cv, s, space::S6 + 2.0 * (qw + space::S6), qy, |dc| {
            gfx.quiet(dc, D2D_RECT_F { left: 0.0, top: 0.0, right: qw, bottom: size::QUIET_MIN_H }, Quiet::Loading, G_SEARCH, t("launchpad.loading"), &theme, 1.0, 0.25).unwrap()
        });

        // ---- the page dots (View::draw_dots: the page shown, the others) ----
        for p in 0..3 {
            let c = if p == 0 || theme.hc.is_some() { ink(Level::Primary) } else { rgba(theme.text_disabled()) };
            let e = D2D1_ELLIPSE { point: Vector2 { X: W / 2.0 + (p as f32 - 1.0) * grid::DOT_GAP, Y: H - space::S7 }, radiusX: grid::DOT / 2.0, radiusY: grid::DOT / 2.0 };
            let b = cv.dc.CreateSolidColorBrush(&c, None).unwrap();
            if p == 0 || theme.hc.is_none() {
                cv.dc.FillEllipse(&e, &b);
            } else {
                cv.dc.DrawEllipse(&e, &b, 1.0, None);
            }
        }
    }
    cv
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_launchpad_states() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    for (folder, light) in kit::LOOKS {
        for (look, s, name) in [("flat", 1.0, "launchpad-states-100"), ("flat", 1.5, "launchpad-states-150"), ("glass", 1.5, "launchpad-states-glass-150"), ("hc", 1.5, "launchpad-states-hc-150")] {
            kit::harmony_save(sheet(&stage, look, light, s), folder, name);
            assert!(kit::harmony_dir().join(folder).join(format!("{name}.png")).is_file());
        }
    }
}
