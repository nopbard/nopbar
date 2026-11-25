//! A stack's panel drawn off screen for the design audit (tests only; see
//! crate::gfx::promo_kit, "harmony"). No folder is read and no window is
//! made: the listing is made up (names, plain grey tiles for icons).
//!
//! The grid: its content by its own paint (grid_art, paint_grid) over the
//! plate popup::chrome gives it (drawn here). The fills under the pointer
//! and the keyboard's cursor, and the button's, are visuals on screen
//! (crate::wash, placed by spot_shape and Lights): drawn here from the
//! same numbers.
//!
//! The scenes (design system 12.4): the pointer on a tile (200, 100 and
//! 150 %), a tile held down, the keyboard's cursor on a tile and on the
//! footer; an empty folder; one that could not be opened (at rest, the
//! pointer on "Try again", Tab's ring on it); Glass; high contrast.
//!
//! The list: the rows `list` gives the menu (list_rows), as a menu
//! (crate::popup::harmony): a folder's files, an empty one, one that could
//! not be opened.

use super::*;
use crate::gfx::promo_kit as kit;
use crate::stacks::Entry;
use crate::wash::harmony as shots;

/// A plain rounded tile `px` a side, grey `v` (premultiplied BGRA): what
/// stands for a file's icon.
fn tile(px: u32, v: u8) -> (u32, Vec<u8>) {
    let (n, r) = (px as f32, px as f32 * 0.22);
    let mut out = Vec::with_capacity((px * px * 4) as usize);
    for y in 0..px {
        for x in 0..px {
            // Coverage of a rounded square a little inside the box.
            let inset = n * 0.08;
            let half = n / 2.0 - inset;
            let (dx, dy) = ((x as f32 + 0.5 - n / 2.0).abs() - (half - r), (y as f32 + 0.5 - n / 2.0).abs() - (half - r));
            let d = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0) - r;
            let a = (0.5 - d).clamp(0.0, 1.0);
            let c = (v as f32 * a).round() as u8;
            out.extend_from_slice(&[c, c, c, (255.0 * a).round() as u8]);
        }
    }
    (px, out)
}

/// A made-up folder of the first `n` names, its icons `px` a side.
fn listing(n: usize, px: u32, grid: bool) -> Listing {
    let names = ["분기 보고서.docx", "예산안 2026.xlsx", "로고 시안.png", "회의록.txt", "여행 사진", "설치 안내서 최종 수정본 (2).pdf", "음악 모음", "발표 자료.pptx", "영수증 11월.pdf", "스케치 03.png", "메모.txt", "보관함.zip"];
    let entries: Vec<Entry> = names.iter().take(n).map(|name| Entry { name: name.to_string(), path: format!("Z:\\harmony\\{name}"), time: 0, dir: !name.contains('.') }).collect();
    // Mid greys that keep 3:1 (a graphic's) on a light and a dark plate
    // alike (final audit 9: #A6 and #BC vanished on a light glass plate);
    // the fifth has no icon: the glyph the stack draws instead.
    let icons = (0..entries.len()).map(|k| (k != 4).then(|| tile(px, [0x76, 0x82, 0x6E, 0x7C][k % 4]))).collect();
    Listing { seq: 1, folder: "Z:\\harmony".into(), entries, icons, grid, ok: true, partial: false }
}

/// The grid for the first `n` made-up files (`ok` false: a folder that
/// could not be opened) showing `lights`, to `<name>.png` in both looks at
/// `scales`.
unsafe fn shoot_grid(name: &str, n: usize, ok: bool, theme_of: fn(bool) -> Theme, scales: &[f32], lights: Lights) {
    let stage = kit::stage();
    for (folder, light) in kit::LOOKS {
        let theme = theme_of(light);
        for &s in scales {
            let l = if ok { listing(n, (GRID_ICON * s).round() as u32, true) } else { Listing::failed(1, "Z:\\harmony") };
            let lay = grid_layout(l.entries.len(), !l.ok);
            let art = grid_art(&stage.gfx, &theme, "다운로드", &l, &lay, (lay.width * s).ceil() as i32, s, false);
            let retry = art.retry.as_ref().map(|(_, r)| *r);
            let cv = shots::canvas(&stage, &theme, light, lay.width, lay.height, s, theme.radii().panel, crate::theme::Elevation::E3);
            let shape = |spot: Spot| spot_shape(&lay, retry, &theme.radii(), spot);
            if let Some(shape) = lights.mark().and_then(shape) {
                shots::mark(&cv, &theme, shape);
            }
            if let Some(lit) = lights.pointer().and_then(|(t, down)| shape(t).map(|shape| Lit { t, down, shape })) {
                shots::hover(&cv, &theme, &lit, false);
            }
            if let Some(b) = shape(Spot::Retry) {
                shots::raised(&cv, &theme, b, raised_state(lights.hover == Some(Spot::Retry), lights.pressed == Some(Spot::Retry)));
            }
            let m = kit::HARMONY_M;
            kit::harmony_px(&cv, |dc| paint_grid(dc, m * s, m * s, &art, &stage.gfx, &theme, lights.drawn(theme.hc.is_some()))).expect("painted");
            // (harmony_px leaves kit::scale()'s transform: this canvas's own.)
            cv.dc.SetTransform(&windows_numerics::Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 });
            shots::save(cv, folder, &shots::file(name, s));
        }
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_stack() {
    let _l = crate::i18n::test_lock("ko");
    let two = [kit::scale()];
    let on = |s: Spot| Lights { hover: Some(s), ..Default::default() };
    let key = |s: Spot| Lights { cursor: Some(s), ring: true, ..Default::default() };
    unsafe {
        // The pointer on a tile; holding it down; the keyboard's cursor
        // on a tile (the pointer elsewhere), and on the footer.
        shoot_grid("stack-grid", 7, true, shots::plain, &[kit::scale(), 1.0, 1.5], on(Spot::Tile(1)));
        shoot_grid("stack-grid-pressed", 7, true, shots::plain, &two, Lights { hover: Some(Spot::Tile(1)), pressed: Some(Spot::Tile(1)), cursor: None, ring: false });
        shoot_grid("stack-grid-cursor", 7, true, shots::plain, &two, Lights { hover: Some(Spot::Tile(1)), cursor: Some(Spot::Tile(5)), pressed: None, ring: true });
        shoot_grid("stack-grid-footer", 7, true, shots::plain, &two, key(Spot::Footer));
        // Quiet states: an empty folder; one that could not be opened (at
        // rest, the pointer on its button, Tab's ring on it).
        shoot_grid("stack-empty", 0, true, shots::plain, &[kit::scale(), 1.0], Lights::default());
        shoot_grid("stack-failed", 0, false, shots::plain, &[kit::scale(), 1.0, 1.5], Lights::default());
        shoot_grid("stack-failed-hover", 0, false, shots::plain, &two, on(Spot::Retry));
        shoot_grid("stack-failed-focus", 0, false, shots::plain, &two, key(Spot::Retry));
        // Glass; high contrast (the pointer's outline on a tile, the
        // keyboard's cursor inverted on another; the button pressed).
        shoot_grid("stack-glass", 7, true, shots::glass, &[1.5], on(Spot::Tile(1)));
        shoot_grid("stack-hc", 7, true, shots::hc, &[1.5], Lights { hover: Some(Spot::Tile(1)), cursor: Some(Spot::Tile(5)), pressed: None, ring: true });
        shoot_grid("stack-hc-failed", 0, false, shots::hc, &[1.5], Lights { hover: Some(Spot::Retry), pressed: Some(Spot::Retry), cursor: Some(Spot::Retry), ring: true });
    }
    let l = listing(12, (16.0 * kit::scale()).round() as u32, false);
    crate::popup::harmony::shoot("stack-list", &list_rows("다운로드", &l), &crate::popup::harmony::Shot::hover("로고 시안.png"));
    crate::popup::harmony::shoot("stack-list-empty", &list_rows("다운로드", &listing(0, 32, false)), &Default::default());
    crate::popup::harmony::shoot("stack-list-failed", &list_rows("다운로드", &Listing::failed(1, "Z:\\harmony")), &Default::default());
}
