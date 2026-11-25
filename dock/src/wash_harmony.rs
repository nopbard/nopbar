//! What the panels' design-audit pictures share (tests only; see
//! crate::gfx::promo_kit, "harmony"): the looks every screen is drawn in
//! (plain, Glass, high contrast: design system 12.4), a canvas at any scale
//! with the panel's plate on it, and crate::wash's fills drawn from the
//! same numbers (on screen they are visuals under the picture).

use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows_numerics::Matrix3x2;

use super::{Lit, Shape};
use crate::gfx::promo_kit as kit;
use crate::syslook::{HighContrast, SysLook};
use crate::theme::{Elevation, Rgba, Role, State, Theme};

/// Graphite as shipped (Flat).
pub fn plain(light: bool) -> Theme {
    kit::theme(light)
}

/// Glass: the tint's opacity over the backdrop (the blur itself is the
/// compositor's).
pub fn glass(light: bool) -> Theme {
    let mut raw = Theme::graphite();
    raw.finish = crate::theme::Finish::Glass;
    raw.resolve_fx(&SysLook { light, transparency: true, animations: true, ..Default::default() }, true)
}

/// High contrast, the system colors fixed: black, white, a sky blue
/// Highlight.
pub fn hc(_light: bool) -> Theme {
    let (white, black, sky) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 }, Rgba { r: 26, g: 235, b: 255, a: 255 });
    Theme::graphite().resolve(&SysLook { high_contrast: Some(HighContrast { window: black, text: white, highlight: sky, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() })
}

/// `name` at `scale`: as is at kit::scale(), "-100" / "-150" otherwise.
pub fn file(name: &str, scale: f32) -> String {
    if scale == kit::scale() {
        name.to_string()
    } else {
        format!("{name}-{}", (scale * 100.0).round() as u32)
    }
}

/// A canvas at `scale` for a surface `w` x `h` DIP: the backdrop and the
/// surface's plate (popup::chrome_at's at elevation `e`, corners `radius`)
/// at (HARMONY_M, HARMONY_M).
pub unsafe fn canvas(stage: &kit::Stage, theme: &Theme, light: bool, w: f32, h: f32, scale: f32, radius: f32, e: Elevation) -> kit::Canvas {
    let m = kit::HARMONY_M;
    let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), w + 2.0 * m, h + 2.0 * m, scale);
    cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
    let plate = kit::popup_plate(theme, radius, e);
    let rim = plate.rim;
    kit::draw_plate(&cv, m, m, w, h, &kit::Plate { rim: None, ..plate });
    if let Some(stops) = rim {
        kit::draw_rim_px(&cv, m, m, w, h, radius, stops, theme.rim_width(e.material(), scale));
    }
    cv
}

/// Runs `f` with the canvas in the surface's own DIPs.
pub unsafe fn on<R>(cv: &kit::Canvas, scale: f32, f: impl FnOnce(&ID2D1DeviceContext) -> R) -> R {
    let m = kit::HARMONY_M;
    cv.dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: m * scale, M32: m * scale });
    let r = f(&cv.dc);
    cv.dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
    r
}

/// Written to `target/harmony-shots/<folder>/<name>.png`.
pub fn save(cv: kit::Canvas, folder: &str, name: &str) {
    kit::harmony_save(cv, folder, name);
    assert!(kit::harmony_dir().join(folder).join(format!("{name}.png")).is_file(), "{name}");
}

fn rr(cv: &kit::Canvas, s: Shape, k: f32, c: Rgba) {
    let m = kit::HARMONY_M;
    let (cx, cy) = (m + s.x + s.w / 2.0, m + s.y + s.h / 2.0);
    let (hw, hh) = (s.w * k / 2.0, s.h * k / 2.0);
    cv.fill_rr(kit::rect(cx - hw, cy - hh, cx + hw, cy + hh), (s.rad * k).min(hw).min(hh), c);
}

/// wash::Hover's fill as it rests on `lit` (`shrink`: a capsule, smaller
/// pressed). Nothing in high contrast (the picture draws it).
pub fn hover<T>(cv: &kit::Canvas, theme: &Theme, lit: &Lit<T>, shrink: bool) {
    if theme.hc.is_some() {
        return;
    }
    let k = if lit.down && shrink { crate::motion::press_scale(lit.shape.w, lit.shape.h) } else { 1.0 };
    rr(cv, lit.shape, k, theme.fill_of(Role::Row, if lit.down { State::Pressed } else { State::Hover }));
}

/// wash::Mark's fill on `shape`.
pub fn mark(cv: &kit::Canvas, theme: &Theme, shape: Shape) {
    if theme.hc.is_none() {
        rr(cv, shape, 1.0, theme.fill_of(Role::Row, State::Cursor));
    }
}

/// wash::Raised's fill on `shape` in state `s`.
pub fn raised(cv: &kit::Canvas, theme: &Theme, shape: Shape, s: State) {
    if theme.hc.is_some() {
        return;
    }
    let k = if s == State::Pressed { crate::motion::press_scale(shape.w, shape.h) } else { 1.0 };
    // With Flat v2's card light round it (wash::CardLight: the drop under
    // it, the light along its top, gone pressed), scaled with it.
    let m = kit::HARMONY_M;
    let (cx, cy) = (m + shape.x + shape.w / 2.0, m + shape.y + shape.h / 2.0);
    let (hw, hh) = (shape.w * k / 2.0, shape.h * k / 2.0);
    let (r, rad) = (kit::rect(cx - hw, cy - hh, cx + hw, cy + hh), (shape.rad * k).min(hw).min(hh));
    unsafe { crate::gfx::parts::card(&cv.dc, r, rad, theme, s, theme.fill_of(Role::Raised, s), 1.0, 1.0).unwrap() };
}
