//! The dock's shadow drawn off screen for the design audit (tests only;
//! see crate::gfx::promo_kit, "harmony"): `picture`'s nine-grid for the
//! theme's dock shadows, stretched along a bar as the shadow window's
//! brush stretches it (its middle column; BarShadow::show_picture), with
//! the empty bar over it (Dock::build's tint and border, as
//! dock/promo.rs draws them).

use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_INTERPOLATION_MODE_LINEAR};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use super::*;
use crate::gfx::promo_kit as kit;

/// The bar in the picture (DIP wide) and the room around it (the dock's
/// shadow reaches further than a panel's).
const BAR_W: f32 = 420.0;
const ROOM: f32 = 64.0;

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_dock_shadow() {
    let s = kit::scale();
    let look = super::super::Look::from_cfg(&crate::config::Config::default().dock, true);
    let bar_h = look.icon + super::super::PAD_CROSS * 2.0;
    for (folder, light) in kit::LOOKS {
        let theme = kit::theme(light);
        let layers = theme.dock_shadows(false).into_iter().map(|(ox, oy, blur, color)| Layer { ox, oy, blur, color }).collect();
        let spec = Spec { layers, radius: look.radius, thick: bar_h, contact: Some(theme.contact()) };
        let p = picture(&spec, s, false).expect("the shadow");
        let cv = kit::Canvas::new(BAR_W + ROOM * 2.0, bar_h + ROOM * 2.0);
        unsafe { cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light)))) };
        // The nine-grid, in px: its ends as they are, its middle column
        // stretched along the bar.
        kit::harmony_px(&cv, |dc| unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bmp = dc.CreateBitmap(D2D_SIZE_U { width: p.w, height: p.h }, Some(p.px.as_ptr() as _), p.w * 4, &props).unwrap();
            let (l, r) = (p.insets[0] as f32, p.insets[2] as f32);
            let (x0, y0) = (ROOM * s - p.pad[0] as f32, ROOM * s - p.pad[1] as f32);
            let x1 = (ROOM + BAR_W) * s + p.pad[2] as f32;
            let (w, h) = (p.w as f32, p.h as f32);
            for (dst, src) in [((x0, x0 + l), (0.0, l)), ((x0 + l, x1 - r), (l, w - r)), ((x1 - r, x1), (w - r, w))] {
                let d = D2D_RECT_F { left: dst.0, top: y0, right: dst.1, bottom: y0 + h };
                let sr = D2D_RECT_F { left: src.0, top: 0.0, right: src.1, bottom: h };
                dc.DrawBitmap(&bmp, Some(&d), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, Some(&sr), None);
            }
        });
        // The bar: the tint and the hairline border.
        let (bx, by, radius) = (ROOM, ROOM, look.radius);
        cv.fill_rr(kit::rect(bx, by, bx + BAR_W, by + bar_h), radius, theme.dock_tint());
        cv.stroke_rr(kit::rect(bx + 0.5, by + 0.5, bx + BAR_W - 0.5, by + bar_h - 0.5), radius - 0.5, theme.dock_border(), 1.0);
        kit::harmony_save(cv, folder, "dock-shadow");
        assert!(kit::harmony_dir().join(folder).join("dock-shadow.png").is_file());
    }
}
