//! An image piece drawn off screen for the promo film (tests only; see
//! crate::gfx::promo_kit): a made-up picture (soft grey hills, drawn here:
//! nothing is read from the screen or the disk) cut to the card's corners
//! the way `draw` shows a piece, and the × that shows on hover, as `draw`
//! paints it.

use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_BRUSH_PROPERTIES, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows_numerics::{Matrix3x2, Vector2};

use super::{close_center, CLOSE_D};
use crate::gfx::promo_kit::{self as kit, col, rect, Canvas};
use crate::glyphs::Glyph;
use crate::theme::Rgba;

/// The piece's size (DIP).
const W: f32 = 240.0;
const H: f32 = 160.0;

/// A quiet picture, `w` x `h` px (premultiplied BGRA, opaque): a pale sky
/// with a sun and three ranges of grey hills.
fn picture(w: u32, h: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    let (fw, fh) = (w as f32, h as f32);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 / fw, y as f32 / fh);
            let mut g = 236.0 - 34.0 * v;
            let (dx, dy) = ((u - 0.72) * fw / fh, v - 0.3);
            let d = (dx * dx + dy * dy).sqrt();
            g += 20.0 * (1.0 - ((d - 0.11) * fh / 1.5).clamp(0.0, 1.0));
            for (base, amp, freq, phase, tone) in [(0.56, 0.07, 5.5, 0.4, 176.0), (0.68, 0.06, 8.0, 2.1, 128.0), (0.82, 0.05, 11.0, 4.0, 84.0)] {
                let ridge = base + amp * (u * freq + phase).sin() + amp * 0.4 * (u * freq * 2.3 + phase * 1.7).sin();
                let cover = ((v - ridge) * fh / 1.5).clamp(0.0, 1.0);
                g = g * (1.0 - cover) + tone * cover;
            }
            let c = g.clamp(0.0, 255.0) as u8;
            px.extend_from_slice(&[c, c, c, 255]);
        }
    }
    px
}

#[test]
#[ignore = "writes the promo film's pictures: cargo test -p dock promo_ -- --ignored"]
fn promo_piece() {
    kit::each_lang(piece_pictures);
}

fn piece_pictures() {
    let s = kit::scale();
    let (pw, ph) = ((W * s) as u32, (H * s) as u32);
    let pixels = picture(pw, ph);
    let mut looks = serde_json::Map::new();
    for (folder, light) in kit::LOOKS {
        let theme = kit::theme(light);
        let rad = theme.radii().card;
        // The picture (pieces::draw): a bitmap brush filling the rounded card.
        let cv = Canvas::new(W, H);
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bmp = cv.dc.CreateBitmap(D2D_SIZE_U { width: pw, height: ph }, Some(pixels.as_ptr() as _), pw * 4, &props).unwrap();
            let bprops = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_INTERPOLATION_MODE_LINEAR };
            // The bitmap is in px, the canvas in DIPs.
            let brush_props = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: Matrix3x2 { M11: 1.0 / s, M12: 0.0, M21: 0.0, M22: 1.0 / s, M31: 0.0, M32: 0.0 } };
            let brush = cv.dc.CreateBitmapBrush(&bmp, Some(&bprops), Some(&brush_props)).unwrap();
            cv.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(0.0, 0.0, W, H), radiusX: rad, radiusY: rad }, &brush);
        }
        cv.save(&format!("{folder}/piece.png"));
        // The × (pieces::draw): a disc of the menus' surface, the glyph in ink.
        let cv = Canvas::new(CLOSE_D, CLOSE_D);
        unsafe {
            let b = cv.dc.CreateSolidColorBrush(&col(Rgba { a: 235, ..theme.popup_tint() }), None).unwrap();
            cv.dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: CLOSE_D / 2.0, Y: CLOSE_D / 2.0 }, radiusX: CLOSE_D / 2.0, radiusY: CLOSE_D / 2.0 }, &b);
            crate::glyphs::draw(&cv.dc, Glyph::Close, CLOSE_D / 2.0, CLOSE_D / 2.0, 9.0, col(Rgba { a: 255, ..theme.ink() })).unwrap();
        }
        cv.save(&format!("{folder}/piece-close.png"));
        looks.insert(folder.into(), serde_json::json!({ "radius": rad, "shadows": kit::shadows_json(&theme.elevation(crate::theme::Elevation::E2)) }));
    }
    let (cx, cy) = close_center(W, false);
    kit::save_json("piece.json", &serde_json::json!({ "scale": s, "w": W, "h": H, "close": { "cx": cx, "cy": cy, "d": CLOSE_D }, "looks": looks }));
    assert!(kit::out_dir().join("dark/piece.png").is_file());
}
