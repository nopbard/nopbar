//! The dock's name label drawn off screen for the design audit (tests
//! only; see crate::gfx::promo_kit, "harmony"). Its text is the label's
//! own paint (Gfx::label_art, gfx::paint_label, as label_brush has it
//! drawn: on a clear ground). Its pill is visuals on screen (style_label,
//! show_label), so it is put together here from the same numbers, as a
//! plate: label_colors' ground (material M4); label_radius; the E1 and
//! contact shadows (label_shadow); the rim (a border in high contrast
//! only). A short name and one
//! cut at the label's widest. The names are made up.

use super::*;
use crate::gfx::promo_kit as kit;

/// Between the two labels (DIP).
const GAP: f32 = 16.0;

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_tip_dock_label() {
    let _l = crate::i18n::test_lock("ko");
    labels(&["메모", "하모니 문서 편집기에서 연 아주 긴 이름의 분기 보고서 초안 문서"], "dock-label");
}

/// Labels for the font comparison (crate::font_compare): Korean and
/// English app names, one cut at the label's widest.
pub(crate) fn font_compare() {
    let _l = crate::i18n::test_lock("ko");
    labels(&["메모", "카카오톡", "Visual Studio Code", "Adobe Photoshop 2026", "설정 - Windows Settings", "하모니 문서 편집기에서 연 아주 긴 이름의 분기 보고서 초안 문서"], "dock-label");
}

/// `names` as labels, one under the other, light and dark, to
/// `<harmony_dir>/<look>/<file>.png`.
fn labels(names: &[&str], file: &str) {
    let stage = kit::stage();
    let s = kit::scale();
    for (folder, light) in kit::LOOKS {
        let theme = kit::theme(light);
        let (bg, fg) = theme.label_colors();
        let clear = crate::theme::Rgba { a: 0, ..bg };
        let arts: Vec<_> = names.iter().map(|n| stage.gfx.label_art(n, s).expect("the label")).collect();
        let w = arts.iter().map(|a| a.w / s).fold(0.0, f32::max);
        let h: f32 = arts.iter().map(|a| a.h / s).sum::<f32>() + GAP * (arts.len() - 1) as f32;
        let cv = kit::harmony_canvas(&stage, w, h, light);
        let m = kit::HARMONY_M;
        let mut y = m;
        for a in &arts {
            let (lw, lh) = (a.w / s, a.h / s);
            let plate = kit::Plate {
                radius: label_radius(&theme, lh),
                tint: bg,
                border: None,
                highlight: None,
                shadows: if crate::effects::items().shadows { theme.elevation(crate::theme::Elevation::E1) } else { Vec::new() },
                // Its rim (style_label, label::edge_stroke) and the contact
                // shadow under its edge (label_shadow).
                rim: theme.rim_of(crate::theme::Material::Tip),
                rim_w: theme.rim_width(crate::theme::Material::Tip, s),
                contact: theme.contact(),
                fall: theme.flat_fill(crate::theme::Material::Tip),
                grain: 0.0,
                // Its backdrop (label.rs: DWM's blur, no graph) under the tint.
                ground: kit::plate_ground(&theme, crate::theme::Material::Tip, bg.a == 255),
                spec: 0.0,
            };
            kit::draw_plate(&cv, m, y, lw, lh, &plate);
            kit::harmony_px(&cv, |dc| unsafe { crate::gfx::paint_label(dc, m * s, y * s, a, clear, fg, theme.radii().tip) }).expect("painted");
            y += lh + GAP;
        }
        kit::harmony_save(cv, folder, file);
        assert!(kit::harmony_dir().join(folder).join(format!("{file}.png")).is_file());
    }
}
