//! The snip overlay drawn off screen for the design audit (tests only;
//! see crate::gfx::promo_kit, "harmony"): render.rs' made-up desktop by
//! paint_frame, the dim around a dragged box and the box's edge in the
//! overlay's own shapes and colors (shapes, snip::DIM_ALPHA and
//! LINE_ALPHA; on screen they are composition rectangles), on the audit's
//! backdrop. The overlay covers a whole display: it has no plate.

use super::*;
use crate::gfx::promo_kit as kit;

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_snip_overlay() {
    let s = kit::scale();
    let f = desktop(640, 400);
    let sel = RECT { left: 140, top: 90, right: 470, bottom: 310 };
    let state = State::Dragging { from: (sel.left, sel.top), to: (sel.right, sel.bottom) };
    let line = snip::line_px(s);
    for (folder, light) in kit::LOOKS {
        let cv = kit::Canvas::new(f.w as f32 / s + kit::HARMONY_M * 2.0, f.h as f32 / s + kit::HARMONY_M * 2.0);
        let o = (kit::HARMONY_M * s) as i32;
        unsafe {
            cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
            kit::harmony_px(&cv, |dc| {
                paint_frame(dc, &f, o as f32, o as f32).unwrap();
                let (dims, lines) = shapes(state, f.w as i32, f.h as i32, line);
                let at = |r: &RECT| RECT { left: r.left + o, top: r.top + o, right: r.right + o, bottom: r.bottom + o };
                for r in &dims {
                    fill(dc, &at(r), snip::DIM_ALPHA, 0.0);
                }
                for r in &lines {
                    fill(dc, &at(r), snip::LINE_ALPHA, 1.0);
                }
            });
        }
        kit::harmony_save(cv, folder, "snip-overlay");
        assert!(kit::harmony_dir().join(folder).join("snip-overlay.png").is_file());
    }
}
