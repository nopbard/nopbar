//! The Launchpad pin's dock icon, drawn (no image file): eight pale tiles
//! 3-3-2 on NopBar's lit plate, like the dock's trash and stacks
//! (crate::glyphs::tile, crate::tile_art), in the dock's light or dark look
//! (small sizes and high contrast: the line glyph on a flat tile in its
//! colors). Full-bleed, so the dock clips it to its squircle like other app
//! icons; drawn at the size asked, so it is sharp at every DPI.

use crate::glyphs::Glyph;
use crate::icons::{Appearance, Icon};

/// The icon at `px` x `px` (premultiplied BGRA) for a dock that looks
/// `look` (light / dark, high contrast).
pub fn icon(px: u32, look: Appearance) -> Icon {
    crate::glyphs::tile(px.clamp(8, 1024), Glyph::Launchpad, look.tile())
}

/// The icon for `key` if it is the Launchpad pin (else None: a real file).
pub fn pin_icon(key: &str, size: i32, look: Appearance) -> Option<Icon> {
    super::is_pin(key).then(|| icon(size.max(8) as u32, look))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_icon() {
        let (dark, light) = (Appearance { light: false, hc: None, flat: false }, Appearance { light: true, hc: None, flat: false });
        let i = icon(64, dark);
        assert_eq!((i.width, i.height, i.pixels.len()), (64, 64, 64 * 64 * 4));
        let px = |i: &Icon, x: u32, y: u32| {
            let k = ((y * 64 + x) * 4) as usize;
            [i.pixels[k], i.pixels[k + 1], i.pixels[k + 2], i.pixels[k + 3]]
        };
        assert_eq!(px(&i, 0, 0)[3], 0, "squircle corner: clear");
        // The plate: neutral grey, dark at the top in dark mode (#4D4D50
        // to #252527), near-white in light mode.
        let edge = px(&i, 32, 4);
        assert_eq!(edge[3], 255);
        let (lo, hi) = (edge[0].min(edge[1]).min(edge[2]), edge[0].max(edge[1]).max(edge[2]));
        assert!(hi - lo <= 8 && hi < 0x70, "{edge:?}");
        // Grey tiles on it, no hue: the middle one (row 2, column 2) is
        // much lighter than the plate.
        let mid = px(&i, 32, 32);
        let (mlo, mhi) = (mid[0].min(mid[1]).min(mid[2]), mid[0].max(mid[1]).max(mid[2]));
        assert!(mhi - mlo <= 8 && mid[1] > edge[1] + 60, "{mid:?} vs {edge:?}");
        // Light docks get a light plate; the tiles are darker than it.
        let l = icon(64, light);
        assert!(px(&l, 32, 4)[0] > 230 && px(&l, 32, 32)[2] < px(&l, 32, 4)[2]);
        // High contrast: the line glyph in the system's colors.
        let hc = Appearance { light: false, hc: Some(([0, 0, 0], [255, 255, 0])), flat: false };
        let h = icon(64, hc);
        assert_eq!(h.pixels, crate::glyphs::line_tile(64, Glyph::Launchpad, hc.tile()).pixels);
        // Full-bleed enough for the dock's squircle clip.
        assert!(crate::icons::analyze(&i).coverage >= 0.9);
        assert!(pin_icon(super::super::PIN, 48, dark).is_some_and(|i| i.width == 48));
        assert!(pin_icon(r"C:\Windows\notepad.exe", 48, dark).is_none());
    }
}
