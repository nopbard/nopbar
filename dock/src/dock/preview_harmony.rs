//! The dock preview's plate and its shadow window read back for the design
//! audit (tests only; see crate::gfx::promo_kit, "harmony"). The shadow
//! window's picture is the product's own (fill_shade: popup::shade_at at
//! E2 in an outer scaled and offset by the margin, as make_shade sets it
//! up); the plate is built as Preview::build stacks it (the menu
//! material's tint, popup::rim_visual, the menus' corners) with two cards'
//! thumbnails as flat stand-ins (a DWM thumbnail cannot be read back). A
//! dock bar sits the popup gap under it, so the shadow is seen falling on
//! it. Read off screen (GraphicsCaptureItem::CreateFromVisual: nothing of
//! the screen, no window shown).
//!
//! The scenes: at rest (100 and 150 %), part of the way open (the frame
//! motion::open and anim_step have 90 ms in: 0.96 growing, fading in, the
//! light coming up), and high contrast (no shadow: the outline).

use windows::core::Interface;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;

use super::*;
use crate::gfx::promo_kit as kit;
use crate::wash::harmony as shots;

/// Scene `name`: the cards (`w` x `h` DIP) and their shadow at `s`, `t`
/// ms into opening (None: at rest), in `theme_of`'s looks.
unsafe fn shoot(stage: &kit::Stage, name: &str, theme_of: fn(bool) -> Theme, s: f32, t: Option<f32>) {
    let c = &stage.gfx.compositor;
    let m = crate::theme::metrics::SHADOW_M;
    let gap = 8.0f32;
    let bar_h = 64.0f32;
    let cw = card_width(1920, 1080);
    let (w, h) = (OUTER * 2.0 + cw * 2.0 + GAP, OUTER * 2.0 + CARD_H);
    let (sw, sh) = (w + m * 2.0, h + m * 2.0 + gap + bar_h);
    let (w_px, h_px) = ((sw * s).ceil() as i32, (sh * s).ceil() as i32);
    let interop: ICompositorDesktopInterop = c.cast().unwrap();
    let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
    let root = c.CreateContainerVisual().unwrap();
    target.SetRoot(&root).unwrap();
    // Where it grows from: its near edge over the icon (the middle of the
    // bottom edge, the dock under it).
    let origin = crate::motion::origin((0.0, 0.0, w, h), Some((w / 2.0, h + gap, 0.0, 0.0)));
    let (k, o) = match t {
        Some(t) => {
            let (k, o, _) = open_frame(open_plan(crate::motion::Mode::Full, 1.0), t);
            (k, o)
        }
        None => (1.0, 1.0),
    };
    // The light's strength: the spring's share of the way (motion::open
    // brings it up on the same keys as the size).
    let light = (k - crate::motion::OPEN_SCALE) / (1.0 - crate::motion::OPEN_SCALE);
    for (folder, is_light) in kit::LOOKS {
        let theme = theme_of(is_light);
        // DWM's blur stands in as the picture's backdrop (glass::StandIn).
        let _ground = crate::glass::StandIn::new(kit::harmony_backdrop(is_light));
        root.Children().unwrap().RemoveAll().unwrap();
        let scene = c.CreateContainerVisual().unwrap();
        scene.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
        root.Children().unwrap().InsertAtTop(&scene).unwrap();
        let bg = kit::harmony_backdrop(is_light);
        let back = c.CreateSpriteVisual().unwrap();
        back.SetBrush(&c.CreateColorBrushWithColor(Color { A: 255, R: bg.r, G: bg.g, B: bg.b }).unwrap()).unwrap();
        back.SetSize(Vector2 { X: w_px as f32, Y: h_px as f32 }).unwrap();
        scene.Children().unwrap().InsertAtTop(&back).unwrap();
        // A dock bar the gap under the cards (the dock's own plate).
        let dock = c.CreateContainerVisual().unwrap();
        dock.SetScale(Vector3 { X: s, Y: s, Z: 1.0 }).unwrap();
        dock.SetOffset(Vector3 { X: (m * 0.5 * s).round(), Y: ((m + h + gap) * s).round(), Z: 0.0 }).unwrap();
        scene.Children().unwrap().InsertAtTop(&dock).unwrap();
        let bar_tint = theme.material(crate::theme::Material::Panel);
        dock.Children().unwrap().InsertAtTop(&rounded(c, (0.0, 0.0, w + m, bar_h), theme.radii().panel, Some(Rgba { a: 255, ..bar_tint }), None).unwrap()).unwrap();
        for i in 0..4 {
            let x = 24.0 + i as f32 * 56.0;
            dock.Children().unwrap().InsertAtTop(&rounded(c, (x, 8.0, 48.0, 48.0), 12.0, Some(Rgba { r: 140, g: 140, b: 140, a: 255 }), None).unwrap()).unwrap();
        }
        // The shadow window's picture: the product's.
        let outer = c.CreateContainerVisual().unwrap();
        outer.SetScale(Vector3 { X: s, Y: s, Z: 1.0 }).unwrap();
        outer.SetOffset(Vector3 { X: (m * s).ceil(), Y: (m * s).ceil(), Z: 0.0 }).unwrap();
        scene.Children().unwrap().InsertAtTop(&outer).unwrap();
        fill_shade(&stage.gfx, &theme, &outer, w, h, s).expect("the shadow");
        // The plate, as Preview::build stacks it (solid: the default
        // finish's menus).
        let plate = c.CreateContainerVisual().unwrap();
        plate.SetScale(Vector3 { X: s, Y: s, Z: 1.0 }).unwrap();
        plate.SetOffset(Vector3 { X: (m * s).ceil(), Y: (m * s).ceil(), Z: 0.0 }).unwrap();
        scene.Children().unwrap().InsertAtTop(&plate).unwrap();
        let r = theme.radii().menu;
        let clip = c.CreateRoundedRectangleGeometry().unwrap();
        clip.SetSize(Vector2 { X: w, Y: h }).unwrap();
        clip.SetCornerRadius(Vector2 { X: r, Y: r }).unwrap();
        plate.SetClip(&c.CreateGeometricClipWithGeometry(&clip).unwrap()).unwrap();
        let tint = theme.material(crate::theme::Material::Menu);
        plate.Children().unwrap().InsertAtTop(&rounded(c, (0.0, 0.0, w, h), 0.0, Some(Rgba { a: 255, ..tint }), None).unwrap()).unwrap();
        for i in 0..2 {
            let x = OUTER + i as f32 * (cw + GAP) + CARD_PAD;
            let shade = if is_light { 205 } else { 70 };
            plate.Children().unwrap().InsertAtTop(&rounded(c, (x, OUTER + TITLE_H, cw - CARD_PAD * 2.0, THUMB_H), 4.0, Some(Rgba { r: shade, g: shade, b: shade, a: 255 }), None).unwrap()).unwrap();
        }
        let rim = crate::popup::rim_visual(&stage.gfx, &theme, crate::theme::Material::Menu, w, h, s, r).unwrap();
        plate.Children().unwrap().InsertAtTop(&rim).unwrap();
        // Part of the way open: both scaled about the same point, the
        // plate faded, the light (the rim, the shade) at its strength.
        if t.is_some() {
            for v in [&outer, &plate] {
                let off = v.Offset().unwrap();
                v.SetCenterPoint(Vector3 { X: origin.0, Y: origin.1, Z: 0.0 }).unwrap();
                // (As motion::open: the rest shift taken back.)
                v.SetOffset(Vector3 { X: off.X - origin.0 * (1.0 - s), Y: off.Y - origin.1 * (1.0 - s), Z: 0.0 }).unwrap();
                v.SetScale(Vector3 { X: s * k, Y: s * k, Z: 1.0 }).unwrap();
            }
            plate.SetOpacity(o).unwrap();
            outer.SetOpacity(o).unwrap();
            rim.SetOpacity(light).unwrap();
            for v in outer.Children().unwrap() {
                v.SetOpacity(light).unwrap();
            }
        }
        let px = crate::popup::chrome_harmony::read_back(&scene, w_px, h_px).unwrap_or_else(|| panic!("{name}: the visual could not be read back: not drawn"));
        let file = shots::file(name, s);
        kit::save_png(&px, w_px as u32, h_px as u32, &kit::harmony_dir().join(folder).join(format!("{file}.png")));
        if t.is_none() && theme.hc.is_none() {
            // The shadow is there: under the plate's bottom edge, over the
            // gap, darker than the backdrop beside the dock.
            let at = |x: i32, y: i32| px[((y * w_px + x) * 4 + 1) as usize] as i32;
            let below = ((m + h + 3.0) * s) as i32;
            let mid = (((m + w / 2.0) * s) as i32).min(w_px - 1);
            assert!(at(mid, below) < at(2, 2), "{name} {folder}: a shadow under the cards ({} vs {})", at(mid, below), at(2, 2));
        }
    }
    drop(target);
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_dock_preview_shade() {
    let stage = kit::stage();
    unsafe {
        shoot(&stage, "preview-shade", shots::plain, 1.5, None);
        shoot(&stage, "preview-shade", shots::plain, 1.0, None);
        shoot(&stage, "preview-shade-opening", shots::plain, 1.5, Some(90.0));
        shoot(&stage, "preview-shade-hc", shots::hc, 1.5, None);
    }
}
