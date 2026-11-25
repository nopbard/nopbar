//! The tooltip chip drawn off screen for the design audit (tests only; see
//! crate::gfx::promo_kit, "harmony"): the label `show_tip` draws (tip_art,
//! paint_tip: its body on material M4, its E1 and contact shadows, its rim
//! and its text are its own paint; no window is made), once on one line
//! and once wrapped as a long window title is (show_tip_wrapped). At the
//! kit's scale (`tip.png`) and at 150 % and 100 % (the rim is one device
//! pixel at each). The words are made up.

use windows_numerics::Matrix3x2;

use super::*;
use crate::gfx::promo_kit::{self as kit, col, Canvas};

/// A wrapped tip's widest text in the picture (DIP).
const WRAP_W: f32 = 220.0;
/// Between the two chips (DIP).
const GAP: f32 = 16.0;

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_tip() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    for (name, s) in [("tip", kit::scale()), ("tip-150", 1.5), ("tip-100", 1.0)] {
        for (folder, light) in kit::LOOKS {
            let theme = kit::theme(light);
            let one = tip_art(&stage.gfx, &theme, s, "데스크톱 2", None).expect("the tip");
            let long = tip_art(&stage.gfx, &theme, s, "분기 보고서 초안 - 하모니 문서 편집기 (읽기 전용, 마지막 저장 오전 9:41)", Some(WRAP_W * s)).expect("the wrapped tip");
            // Each chip in its window's box (the label and its shadow margin).
            let side = |a: &TipArt| (((a.b.w + 2 * a.b.margin) as f32) / s, ((a.b.h + 2 * a.b.margin) as f32) / s);
            let ((w0, h0), (w1, h1)) = (side(&one), side(&long));
            let m = kit::HARMONY_M;
            let cv = Canvas::of(stage.gfx.d2d_factory().clone(), w0.max(w1) + m * 2.0, h0 + GAP + h1 + m * 2.0, s);
            unsafe {
                cv.dc.Clear(Some(&col(kit::harmony_backdrop(light))));
                // In pixels, as the tip's own surface is drawn.
                cv.dc.SetTransform(&Matrix3x2::identity());
                paint_tip(&cv.dc, (m * s).round(), (m * s).round(), &one).expect("painted");
                paint_tip(&cv.dc, (m * s).round(), ((m + h0 + GAP) * s).round(), &long).expect("painted");
            }
            kit::harmony_save(cv, folder, name);
            assert!(kit::harmony_dir().join(folder).join(format!("{name}.png")).is_file());
        }
    }
}
