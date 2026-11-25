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
use crate::gfx::promo_kit as kit;
use crate::glyphs::{Glyph, TileLook};

/// A tile's edge and the gap between two (DIP).
const SIDE: f32 = 168.0;
const GAP: f32 = 24.0;

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
