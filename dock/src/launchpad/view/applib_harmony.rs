//! The App Library's category tiles drawn off screen for the design audit
//! (tests only; see crate::gfx::promo_kit, "harmony"): three tiles by
//! paint_category_tile with the theme's look (Pad::look), their apps the
//! dock's own drawn tiles (no app of this PC), over the Launchpad's tint
//! (View::tint_color: the menus' surface, 55 %) on the audit's backdrop.
//! A full tile with a new app, one still loading (placeholders), and one
//! with four apps (no mini grid).
//!
//! Not in the picture: the names under the tiles and the hover (visuals).

use std::sync::Arc;

use super::*;
use crate::gfx::promo_kit::{Canvas, GlassWallpaper, Stage};
use crate::gfx::promo_kit as kit;
use crate::glyphs::{Glyph, TileLook};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD};

/// A tile's edge and the gap between two (DIP).
const SIDE: f32 = 168.0;
const GAP: f32 = 24.0;

pub(crate) fn paint_site_all_apps(stage: &Stage, cv: &Canvas, theme: &Theme, wall: &GlassWallpaper<'_>, s: f32, top: f32) {
    use crate::launchpad::grid;
    use crate::theme::{Level, Material};

    let (w, h) = (cv.w as f32 / s, cv.h as f32 / s);
    let regions = grid::regions(w, h - top - crate::dock::promo::reserve(), false);
    let transform = Matrix3x2 { M11: s, M22: s, ..Matrix3x2::identity() };
    let gfx = &stage.gfx;
    let family = &theme.font.family;
    let lines = super::super::lines_h(gfx.dwrite(), family, grid::LABEL_SIZE * crate::fonts::scale(), 2).unwrap();
    let grid = grid::grid_in(&regions, lines);
    let groups = [
        (Cat::Dev, &[Glyph::Terminal, Glyph::Document, Glyph::Folder, Glyph::Keyboard][..]),
        (Cat::Creative, &[Glyph::Image, Glyph::Edit, Glyph::Camera, Glyph::Music, Glyph::Play, Glyph::Sparkle][..]),
        (Cat::Office, &[Glyph::Document, Glyph::Calendar, Glyph::Calculator, Glyph::Timer][..]),
        (Cat::Utilities, &[Glyph::Gear, Glyph::Lock, Glyph::Search, Glyph::Timer, Glyph::Download][..]),
    ];
    let tiles = library::tiles(groups.len(), &grid, &regions.grid).on_pixels(s, (0.0, top));
    let look = look_of(theme);
    let ink = rgba(theme.text_primary());
    let dim = rgba(theme.text_secondary());
    let field = regions.field();
    let full = kit::rect(0.0, 0.0, w, h);
    unsafe {
        cv.dc.SetTransform(&transform);
        wall.draw(cv, full, 0.0, theme, Material::Panel);
        let plate = cv.dc.CreateSolidColorBrush(&super::super::plate_tint(theme), None).unwrap();
        cv.dc.FillRectangle(&full, &plate);

        let at = |x, y| cv.dc.SetTransform(&Matrix3x2 { M31: x * s, M32: y * s, ..transform });
        at(field.l, top + field.t);
        super::super::paint_field_plate(&cv.dc, field.w(), field.h(), theme, false, false, dim).unwrap();
        let (placeholder, _, th) = gfx.text_layout(t("launchpad.placeholder"), family, super::super::FIELD_TEXT, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
        let lead = super::super::FIELD_PAD + size::ICON_S + space::S3;
        draw_layout(&cv.dc, &placeholder, lead, (field.h() - th) / 2.0, rgba(theme.text_of(Level::Tertiary, State::Rest))).unwrap();

        for (k, (cat, glyphs)) in groups.into_iter().enumerate() {
            let icon = |glyph, r: Rect| {
                let tile = crate::glyphs::tile((r.w() * s).round() as u32, glyph, TileLook::new(theme.is_light, None));
                Some(Arc::new(crate::desklayer::backdrop::saturated(&tile, 0.0)))
            };
            let (big, _) = library::parts(glyphs.len());
            let side = tiles.edge();
            let art = TileArt {
                big: glyphs[..big].iter().enumerate().map(|(i, &g)| (icon(g, library::big_icon(i, side, false)), false)).collect(),
                mini: glyphs[big..].iter().enumerate().map(|(i, &g)| icon(g, library::mini_icon(i, side, false))).collect(),
            };
            let tile = tiles.tile(k, false);
            at(tile.l, top + tile.t);
            paint_category_tile(&cv.dc, side, &art, &look, false).unwrap();

            let title = tiles.name(k, false);
            let (label, lw, lh) = gfx.text_layout(t(cat.label_key()), family, library::NAME_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD).unwrap();
            let (count, nw, nh) = gfx.text_layout(&glyphs.len().to_string(), family, crate::theme::ramp::BODY.size, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
            let x = title.cx() - (size::ICON_S + 2.0 * space::S3 + lw + nw) / 2.0;
            at(x, top + title.cy());
            crate::glyphs::draw(&cv.dc, cat_glyph(cat), size::ICON_S / 2.0, 0.0, size::ICON_S, ink).unwrap();
            draw_layout(&cv.dc, &label, size::ICON_S + space::S3, -lh / 2.0, ink).unwrap();
            draw_layout(&cv.dc, &count, size::ICON_S + 2.0 * space::S3 + lw, -nh / 2.0, dim).unwrap();
        }

        cv.dc.SetTransform(&transform);
        let (pages, page) = (2, 1);
        let dots = regions.dots_row(pages, page, false);
        for p in dots.shown_pages() {
            let r = dots.shape(p).unwrap();
            let x = r.cx();
            if p == page {
                paint_library_dot(&cv.dc, x, top + r.cy(), dots.size(p), ink, true).unwrap();
            } else {
                let brush = cv.dc.CreateSolidColorBrush(&rgba(theme.text_disabled()), None).unwrap();
                cv.dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: x, Y: top + r.cy() }, radiusX: r.w() / 2.0, radiusY: r.h() / 2.0 }, &brush);
            }
        }
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_library_tile() {
    let stage = kit::stage();
    for (folder, light) in kit::LOOKS {
        let theme = kit::theme(light);
        let look = look_of(&theme);
        let icon = |g: Glyph| Some(Arc::new(crate::glyphs::tile(128, g, TileLook::new(light, None))));
        let tiles = [
            TileArt { big: vec![(icon(Glyph::Globe), false), (icon(Glyph::Music), true), (icon(Glyph::Image), false)], mini: vec![icon(Glyph::Terminal), icon(Glyph::Calculator), icon(Glyph::Calendar), icon(Glyph::Gear)] },
            TileArt { big: vec![(icon(Glyph::Document), false), (None, false), (icon(Glyph::Folder), false)], mini: vec![icon(Glyph::Camera), None, None] },
            TileArt { big: vec![(icon(Glyph::Gamepad), false), (icon(Glyph::Play), false), (icon(Glyph::Timer), true), (icon(Glyph::Headphones), false)], mini: Vec::new() },
        ];
        let (w, m) = (SIDE * 3.0 + GAP * 2.0, kit::HARMONY_M);
        let cv = kit::harmony_canvas(&stage, w, SIDE, light);
        // The Launchpad's tint over everything behind it.
        cv.fill_rr(kit::rect(0.0, 0.0, w + m * 2.0, SIDE + m * 2.0), 0.0, crate::theme::Rgba { a: 255, ..theme.popup_tint() });
        for (k, art) in tiles.iter().enumerate() {
            kit::harmony_at(&cv, m + k as f32 * (SIDE + GAP), m, |dc| unsafe { paint_category_tile(dc, SIDE, art, &look, false) }).expect("painted");
        }
        kit::harmony_save(cv, folder, "library-tile");
        assert!(kit::harmony_dir().join(folder).join("library-tile.png").is_file());
    }
}
