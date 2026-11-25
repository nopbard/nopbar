//! Glass v3 read back and drawn off screen (tests only; see
//! crate::gfx::promo_kit, "harmony").
//!
//! No off-screen picture has DWM's backdrop (it is the screen's), so:
//! - glass_graph_on_the_compositor runs the plate's real effect graph on
//!   the compositor over a picture of colored stripes in the backdrop's
//!   place and reads that visual tree back (GraphicsCaptureItem::
//!   CreateFromVisual: nothing of the screen), next to the graph's
//!   formulas on the CPU (glass::model): they must agree, and the Blend
//!   mode that gives the Luminosity layer is the one that does.
//! - harmony_glass_sheets draws the mockup's scene (the bar, a menu, a
//!   panel, the dock and its label) over the mockup's wallpapers with
//!   each plate computed by glass::model from the blurred wallpaper (the
//!   recipe's blur standing in for DWM's), the tokens of each finish, the
//!   top light and the grain, the rim: today's glass and the three new ones side by
//!   side, to hold against the mockup. Written to
//!   `target/harmony-shots/glass-v3/<wallpaper>-<light|dark>.png`.
//! - harmony_glass_mockup_sheets puts each glass as the mockup draws it
//!   (its recipe's tints over the blur, no Luminosity layer) next to the
//!   same glass in our tokens, the same scene and the same CPU stand-in:
//!   `target/harmony-shots/glass-v3/compare-<wallpaper>-<light|dark>.png`.

use std::mem::ManuallyDrop;

use windows::core::{Interface, HSTRING};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::UI::Composition::CompositionStretch;
use windows_numerics::{Matrix3x2, Vector2};

use super::*;
use crate::gfx::promo_kit as kit;
use crate::syslook::SysLook;
use crate::theme::{ramp, Finish, Rgba, Theme};

fn col(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

fn f3(c: (u8, u8, u8)) -> [f32; 3] {
    [c.0 as f32 / 255.0, c.1 as f32 / 255.0, c.2 as f32 / 255.0]
}

/// The stripes standing in for the backdrop: a warm red, a blue, a mid
/// grey, a pale warm white, a deep green.
const STRIPES: [(u8, u8, u8); 5] = [(200, 60, 40), (40, 120, 220), (128, 128, 128), (240, 230, 220), (20, 110, 60)];
const STRIPE: f32 = 32.0;

#[test]
#[ignore = "needs a compositor: cargo test -p dock glass_ -- --ignored --nocapture"]
fn glass_graph_on_the_compositor() {
    let stage = kit::stage();
    let gfx = &stage.gfx;
    let c = &gfx.compositor;
    let (w, row) = (STRIPE * STRIPES.len() as f32, STRIPE);
    let mut px = Vec::new();
    for _y in 0..STRIPE as u32 {
        for s in STRIPES {
            for _x in 0..STRIPE as u32 {
                px.extend_from_slice(&[s.2, s.1, s.0, 255]);
            }
        }
    }
    let src = c.CreateSurfaceBrushWithSurface(&gfx.surface_from_bgra(w as u32, STRIPE as u32, &px).unwrap()).unwrap();
    src.SetStretch(CompositionStretch::Fill).unwrap();
    let grey = Rgba { r: 0x26, g: 0x26, b: 0x26, a: 153 };
    // (what, Blend mode (None: saturation alone), saturation, layer)
    let rows: [(&str, Option<u32>, f32, Rgba); 6] = [
        ("saturation 1", None, 1.0, grey),
        ("saturation 2", None, 2.0, grey),
        ("saturation 1.3", None, 1.3, grey),
        ("blend 22 (COLOR)", Some(22), 1.0, grey),
        ("blend 23 (LUMINOSITY)", Some(23), 1.0, grey),
        ("blend LUM_MODE, saturation 2", Some(LUM_MODE), 2.0, grey),
    ];
    let h = row * rows.len() as f32;
    let px = unsafe {
        let interop: ICompositorDesktopInterop = c.cast().unwrap();
        let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
        let root = c.CreateContainerVisual().unwrap();
        target.SetRoot(&root).unwrap();
        let scene = c.CreateContainerVisual().unwrap();
        scene.SetSize(Vector2 { X: w, Y: h }).unwrap();
        root.Children().unwrap().InsertAtTop(&scene).unwrap();
        for (i, (what, mode, sat, lum)) in rows.iter().enumerate() {
            let f = plate_factory(c, *mode).unwrap_or_else(|e| panic!("{what}: {e}"));
            let b = f.CreateBrush().unwrap();
            b.SetSourceParameter(&HSTRING::from(BACKDROP), &src).unwrap();
            tune(&b, if mode.is_some() { Level::Full } else { Level::Sat }, *sat, *lum).unwrap();
            let v = c.CreateSpriteVisual().unwrap();
            v.SetBrush(&b).unwrap();
            v.SetSize(Vector2 { X: w, Y: row }).unwrap();
            v.SetOffset(windows_numerics::Vector3 { X: 0.0, Y: row * i as f32, Z: 0.0 }).unwrap();
            scene.Children().unwrap().InsertAtTop(&v).unwrap();
        }
        let got = crate::popup::chrome_harmony::read_back(&scene, w as i32, h as i32).expect("a frame");
        drop(target);
        got
    };
    let at = |x: usize, y: usize| {
        let i = (y * w as usize + x) * 4;
        [px[i + 2], px[i + 1], px[i]]
    };
    let to8 = |c: [f32; 3]| c.map(|v| (v * 255.0).round() as u8);
    let off = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).map(|(x, y)| (*x as i32 - y as i32).abs()).max().unwrap();
    let mut worst = [0i32; 6];
    for (i, (what, mode, sat, lum)) in rows.iter().enumerate() {
        let fx = GlassFx { sat: *sat, lum: if mode.is_some() { *lum } else { Rgba { a: 0, ..*lum } }, spec: 0.0, noise: 0.0 };
        for (k, s) in STRIPES.iter().enumerate() {
            let got = at((k as f32 * STRIPE + STRIPE / 2.0) as usize, (i as f32 * row + row / 2.0) as usize);
            let want = to8(model(f3(*s), &fx, Rgba { a: 0, ..*lum }));
            println!("{what:<24} {s:?} -> got {got:?} model {want:?}");
            worst[i] = worst[i].max(off(got, want));
        }
    }
    println!("worst off by row: {worst:?}");
    // Saturation is D2D's matrix, above 1 too (the property is set).
    assert!(worst[0] <= 3 && worst[1] <= 4 && worst[2] <= 4, "{worst:?}");
    // The mode glass uses gives the Luminosity layer, saturation under it
    // (the names are flipped: 22, "COLOR", is the luminosity blend).
    assert_eq!(LUM_MODE, 22);
    assert!(worst[3] <= 6 && worst[5] <= 6, "{worst:?}");
    // And 23 is not: it washes the color out.
    assert!(worst[4] > 40, "{worst:?}");
    // The kit the app uses: the full graph made once, the grain's Border
    // (wrap) graph and its tile, a top light for every glass.
    assert!(has_graph(c));
    assert!(grain(gfx).is_some(), "the grain");
    for light in [false, true] {
        let mut t = Theme::graphite();
        t.finish = Finish::Glass;
        let t = t.resolve_fx(&SysLook { light, ..Default::default() }, true);
        assert!(top_light(gfx, &t, Material::Panel, Some(Vector2 { X: 100.0, Y: 60.0 })).unwrap().is_some(), "light {light}");
        let b = backdrop(gfx, &t, Material::Panel).unwrap();
        assert!(b.cast::<windows::UI::Composition::CompositionEffectBrush>().is_ok());
        assert_eq!(level_of(&b), Level::Full, "nothing failed: the full graph");
    }
}

/// Glass v3 review 1: a brush that fails at any step of its making (its
/// factory, CreateBrush, its source, its properties) on the compositor
/// falls one level down, the UI is still made, and the tint over it is
/// the one for the brush it really has.
#[test]
#[ignore = "needs a compositor: cargo test -p dock glass_ -- --ignored --nocapture"]
fn glass_falls_back_on_the_compositor() {
    use crate::theme::Elevation;
    use windows::UI::Composition::{CompositionBackdropBrush, CompositionEffectBrush};
    let stage = kit::stage();
    let gfx = &stage.gfx;
    let c = &gfx.compositor;
    let theme = |f: Finish| {
        let mut t = Theme::graphite();
        t.finish = f;
        t.resolve_fx(&SysLook::default(), true)
    };
    let t = theme(Finish::Glass);
    let m = Material::Panel;
    let cases: [(&[(Level, Step)], Level); 9] = [
        (&[], Level::Full),
        (&[(Level::Full, Step::Factory)], Level::Sat),
        (&[(Level::Full, Step::Create)], Level::Sat),
        (&[(Level::Full, Step::Source)], Level::Sat),
        (&[(Level::Full, Step::Tune)], Level::Sat),
        (&[(Level::Sat, Step::Create)], Level::Full),
        (&[(Level::Full, Step::Tune), (Level::Sat, Step::Create)], Level::Plain),
        (&[(Level::Full, Step::Source), (Level::Sat, Step::Tune)], Level::Plain),
        (&[(Level::Full, Step::Factory), (Level::Sat, Step::Factory)], Level::Plain),
    ];
    for (faults, want) in cases {
        FAULTS.with(|f| *f.borrow_mut() = faults.to_vec());
        // The backdrop is made whatever fails, at the level that worked.
        let b = backdrop(gfx, &t, m).unwrap_or_else(|e| panic!("{faults:?}: {e}"));
        assert_eq!(level_of(&b), want, "{faults:?}");
        if want == Level::Plain {
            assert!(b.cast::<CompositionBackdropBrush>().is_ok(), "the host backdrop as it is");
        } else {
            assert!(b.cast::<CompositionEffectBrush>().is_ok());
        }
        // Its tint is the one for that level (folded below the full graph).
        assert_eq!(tint_for(&t, m, Some(&b)), tint_at(&t, m, want), "{faults:?}");
        // A panel's plate is made (popup::chrome_at), and a menu's.
        for e in [Elevation::E3, Elevation::E2] {
            let outer = c.CreateContainerVisual().unwrap();
            crate::popup::chrome_at(gfx, &t, &outer, 200.0, 120.0, 1.0, 20.0, e).unwrap_or_else(|e| panic!("{faults:?}: the plate: {e}"));
        }
        // A surface's backdrop refreshed: the brush it gets and the tint
        // it is told to lay over agree.
        let v = c.CreateSpriteVisual().unwrap();
        let tint = refresh_backdrop(gfx, &v, &t, m);
        let got = v.Brush().unwrap();
        assert_eq!((level_of(&got), tint), (want, tint_at(&t, m, want)), "{faults:?}");
        // The faults gone: a brush that fell back is kept (retuned, not
        // made again: DWM would set the backdrop up again) and its tint
        // still agrees with it; the other palette retunes it at its level.
        FAULTS.with(|f| f.borrow_mut().clear());
        let other = {
            let mut t = Theme::graphite();
            t.finish = Finish::Glass;
            t.resolve_fx(&SysLook { light: true, ..Default::default() }, true)
        };
        let tint = refresh_backdrop(gfx, &v, &other, m);
        let kept = v.Brush().unwrap();
        if want == Level::Plain {
            // The plain backdrop is not kept while a graph can be had.
            assert_eq!(level_of(&kept), Level::Full);
        } else {
            assert_eq!(kept, got, "{faults:?}: kept");
        }
        assert_eq!(tint, tint_at(&other, m, level_of(&kept)), "{faults:?}");
    }
    FAULTS.with(|f| f.borrow_mut().clear());
    // Flat: the plain backdrop, the tint as it is.
    let flat = theme(Finish::Flat);
    let b = backdrop(gfx, &flat, m).unwrap();
    assert_eq!((level_of(&b), tint_for(&flat, m, Some(&b))), (Level::Plain, flat.material(m)));
}

/// Glass v3 review 4: the grain's tile after a device recovery: the same
/// surface brush (the grain already placed uses it) gets a new surface
/// with the pixels, read back off screen.
#[test]
#[ignore = "needs a compositor: cargo test -p dock glass_ -- --ignored --nocapture"]
fn glass_grain_survives_a_device_recovery() {
    let stage = kit::stage();
    let gfx = &stage.gfx;
    let c = &gfx.compositor;
    let tile = || with_kit(c, |k| k.grain.as_ref().and_then(|g| g.made.as_ref().map(|(_, t)| (g.generation, t.clone(), t.Surface().unwrap()))));
    let old_brush = grain(gfx).expect("the grain");
    let (gen0, brush0, surface0) = tile().expect("made");
    assert_eq!(gen0, gfx.generation());
    // The same device: used as it is.
    let _ = grain(gfx).expect("the grain");
    assert_eq!(tile().unwrap().2, surface0);
    // A recovery (a new rendering device under the same composition
    // device): the tile is filled again on the next use.
    gfx.recover(true).expect("a new device");
    assert_eq!(gfx.generation(), gen0.wrapping_add(1));
    let new_brush = grain(gfx).expect("the grain after the recovery");
    let (gen1, brush1, surface1) = tile().unwrap();
    assert_eq!(gen1, gfx.generation());
    assert_eq!(brush1, brush0, "the same tile brush");
    assert_ne!(surface1, surface0, "a new surface with the pixels");
    // Both grains, the one placed before and the new one, show the tile.
    let side = TILE as f32;
    let px = unsafe {
        let interop: ICompositorDesktopInterop = c.cast().unwrap();
        let target = interop.CreateDesktopWindowTarget(stage.hwnd, false).unwrap();
        let root = c.CreateContainerVisual().unwrap();
        target.SetRoot(&root).unwrap();
        let scene = c.CreateContainerVisual().unwrap();
        scene.SetSize(Vector2 { X: side * 2.0, Y: side }).unwrap();
        root.Children().unwrap().InsertAtTop(&scene).unwrap();
        for (i, b) in [old_brush, new_brush].iter().enumerate() {
            let v = c.CreateSpriteVisual().unwrap();
            v.SetBrush(b).unwrap();
            v.SetSize(Vector2 { X: side, Y: side }).unwrap();
            v.SetOffset(windows_numerics::Vector3 { X: side * i as f32, Y: 0.0, Z: 0.0 }).unwrap();
            scene.Children().unwrap().InsertAtTop(&v).unwrap();
        }
        let got = crate::popup::chrome_harmony::read_back(&scene, side as i32 * 2, side as i32).expect("a frame");
        drop(target);
        got
    };
    for half in 0..2 {
        let (mut sum, mut lo, mut hi, mut n) = (0u64, 255u8, 0u8, 0u64);
        for y in 0..TILE as usize {
            for x in 0..TILE as usize {
                let a = px[(y * TILE as usize * 2 + half * TILE as usize + x) * 4 + 3];
                sum += a as u64;
                lo = lo.min(a);
                hi = hi.max(a);
                n += 1;
            }
        }
        let mean = sum as f32 / n as f32;
        println!("grain {half}: mean alpha {mean:.1}, {lo}..{hi}");
        assert!((100.0..155.0).contains(&mean) && hi - lo > 200, "grain {half} is there: mean {mean}, {lo}..{hi}");
    }
}

// ---- the sheets ----------------------------------------------------------------

/// The mockup's scene (DIP).
const W: f32 = 760.0;
const H: f32 = 440.0;

/// A radial blob (the mockup's CSS: center x, y and radii x, y as shares,
/// its color, the stop where it is gone).
type Blob = (f32, f32, f32, f32, (u8, u8, u8), f32);

/// A wallpaper: radial blobs (top first) over a vertical base gradient.
struct Wall {
    name: &'static str,
    blobs: &'static [Blob],
    base: [(u8, u8, u8); 3],
}

const WALLS: [Wall; 3] = [
    Wall { name: "bloom", blobs: &[(0.30, 0.70, 0.60, 0.70, (0x3d, 0x6b, 0xff), 0.60), (0.75, 0.35, 0.50, 0.60, (0xb1, 0x4d, 0xff), 0.65), (0.60, 0.85, 0.40, 0.50, (0x14, 0xc8, 0xff), 0.60)], base: [(0x0b, 0x10, 0x30), (0x1a, 0x14, 0x50), (0x06, 0x21, 0x3a)] },
    Wall { name: "sunset", blobs: &[(0.50, 1.00, 0.70, 0.60, (0xff, 0x7a, 0x3d), 0.65), (0.20, 0.30, 0.60, 0.60, (0xff, 0x4f, 0x8b), 0.60), (0.85, 0.20, 0.50, 0.50, (0xff, 0xc2, 0x4d), 0.60)], base: [(0x5b, 0x2a, 0x86), (0xe0, 0x50, 0x7a), (0xff, 0xb3, 0x6b)] },
    Wall { name: "paper", blobs: &[(0.25, 0.30, 0.55, 0.55, (0xff, 0xd9, 0xe6), 0.60), (0.75, 0.65, 0.55, 0.55, (0xd4, 0xe8, 0xff), 0.60), (0.55, 0.20, 0.40, 0.40, (0xff, 0xf1, 0xc9), 0.55)], base: [(0xf6, 0xf3, 0xee), (0xf1, 0xf2, 0xf3), (0xee, 0xf2, 0xf8)] },
];

/// The wallpaper at `w` x `h` px (sRGB 0..1). CSS draws the first
/// gradient on top: the blobs are laid bottom (last) to top (first).
fn wallpaper(wall: &Wall, w: usize, h: usize) -> Vec<[f32; 3]> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let t = y as f32 / (h - 1) as f32;
            let (a, b, k) = if t < 0.6 { (wall.base[0], wall.base[1], t / 0.6) } else { (wall.base[1], wall.base[2], (t - 0.6) / 0.4) };
            let (a, b) = (f3(a), f3(b));
            let mut c: [f32; 3] = std::array::from_fn(|i| a[i] + (b[i] - a[i]) * k);
            for &(cx, cy, rx, ry, color, stop) in wall.blobs.iter().rev() {
                let dx = (x as f32 / w as f32 - cx) / rx;
                let dy = (y as f32 / h as f32 - cy) / ry;
                let al = (1.0 - (dx * dx + dy * dy).sqrt() / stop).max(0.0);
                let col = f3(color);
                c = std::array::from_fn(|i| c[i] * (1.0 - al) + col[i] * al);
            }
            out.push(c);
        }
    }
    out
}

/// A Gaussian blur of standard deviation `sigma` px: three box passes
/// each way.
fn blur(img: &[[f32; 3]], w: usize, h: usize, sigma: f32) -> Vec<[f32; 3]> {
    let r = ((((12.0 * sigma * sigma / 3.0) + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0) as isize;
    let mut a = img.to_vec();
    let mut b = vec![[0.0f32; 3]; w * h];
    for _ in 0..3 {
        box_pass(&a, &mut b, w, h, r, true);
        box_pass(&b, &mut a, w, h, r, false);
    }
    a
}

/// One box pass of radius `r` along rows (`rows`) or columns, the edges
/// extended.
fn box_pass(src: &[[f32; 3]], dst: &mut [[f32; 3]], w: usize, h: usize, r: isize, rows: bool) {
    let (along, across, stride, step) = if rows { (w, h, w, 1) } else { (h, w, 1, w) };
    let n = (2 * r + 1) as f32;
    for j in 0..across {
        let base = j * stride;
        let get = |i: isize| src[base + (i.clamp(0, along as isize - 1) as usize) * step];
        let mut sum = [0.0f32; 3];
        for i in -r..=r {
            let p = get(i);
            sum = std::array::from_fn(|c| sum[c] + p[c]);
        }
        for i in 0..along as isize {
            dst[base + i as usize * step] = sum.map(|v| v / n);
            let (add, sub) = (get(i + r + 1), get(i - r));
            sum = std::array::from_fn(|c| sum[c] + add[c] - sub[c]);
        }
    }
}

/// One finish of a sheet: what it is called, its theme, its blur (the
/// recipe's CSS blur, DIP), and (today's glass, the mockup's recipes) its
/// fixed plates.
struct Look {
    name: String,
    theme: Theme,
    blur: f32,
    fixed: Option<Fixed>,
}

/// Plates not of a theme's glass: a tint per material (bar, dock, panel,
/// menu, name label) over the blur through `fx` (no Luminosity layer),
/// the blur brightened by `bright` first (the mockup's CSS brightness);
/// the name label a glass too (`label_glass`, the mockup's) or its tint
/// over the desktop; the rim today's (`rim_today`) or the theme's.
struct Fixed {
    tints: [Rgba; 5],
    fx: GlassFx,
    bright: f32,
    label_glass: bool,
    rim_today: bool,
}

/// The mockup's recipe for `finish` (its R table: the plate's tint for
/// the bar, the dock, the panel and the label, the "strong" one for the
/// menu; saturation, brightness, top light, grain) as fixed plates.
fn mockup(finish: Finish, light: bool) -> Fixed {
    // (tint grey, alpha), (strong grey, alpha), sat, brightness, spec, noise
    let r = match (finish, light) {
        (Finish::Glass, false) => ((31, 0.52), (38, 0.66), 2.0, 1.0, 0.05, 0.02),
        (Finish::Glass, true) => ((250, 0.50), (252, 0.66), 1.9, 1.08, 0.12, 0.02),
        _ => unreachable!("the glass"),
    };
    let grey = |(v, a): (u8, f32)| Rgba { r: v, g: v, b: v, a: (a * 255.0).round() as u8 };
    let (t, m) = (grey(r.0), grey(r.1));
    Fixed { tints: [t, t, t, m, t], fx: GlassFx { sat: r.2, lum: Rgba { r: 0, g: 0, b: 0, a: 0 }, spec: r.4, noise: r.5 }, bright: r.3, label_glass: true, rim_today: false }
}

fn looks(light: bool) -> Vec<Look> {
    // Windows' accent as the audit's grey (kit::theme's): Windows' default
    // blue filled the PC's bars (final audit 10; the design is achromatic).
    let grey_accent = if light { 0x3A } else { 0xE6 };
    let theme = |f: Finish| {
        let mut t = Theme::graphite();
        t.finish = f;
        t.resolve_fx(&SysLook { light, accent: Rgba { r: grey_accent, g: grey_accent, b: grey_accent, a: 255 }, ..Default::default() }, true)
    };
    // 0.1.62's glass: the grey ladder at .74 to .95, alpha alone.
    let grey = |v: u8, a: f32| Rgba { r: v, g: v, b: v, a: (a * 255.0).round() as u8 };
    let old = if light {
        [grey(0xF5, 0.74), grey(0xF6, 0.78), grey(0xF8, 0.80), grey(0xFA, 0.88), grey(0xFC, 0.95)]
    } else {
        [grey(0x1E, 0.74), grey(0x22, 0.80), grey(0x26, 0.84), grey(0x2A, 0.88), grey(0x30, 0.94)]
    };
    let today = Fixed { tints: old, fx: GlassFx { sat: 1.0, lum: Rgba { r: 0, g: 0, b: 0, a: 0 }, spec: 0.0, noise: 0.0 }, bright: 1.0, label_glass: false, rim_today: true };
    vec![
        Look { name: "0.1.62 유리".into(), theme: theme(Finish::Glass), blur: 30.0, fixed: Some(today) },
        Look { name: "플랫".into(), theme: theme(Finish::Flat), blur: 44.0, fixed: None },
        Look { name: "글래스 (기본)".into(), theme: theme(Finish::Glass), blur: 44.0, fixed: None },
    ]
}

/// Coverage of a rounded rectangle at pixel center (`px`, `py`).
fn cover(px: f32, py: f32, (x, y, w, h): (f32, f32, f32, f32), r: f32) -> f32 {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let (qx, qy) = ((px - cx).abs() - (w / 2.0 - r), (py - cy).abs() - (h / 2.0 - r));
    let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
}

struct Plate {
    m: Material,
    rect: (f32, f32, f32, f32),
    radius: f32,
}

fn plates(t: &Theme) -> Vec<Plate> {
    let r = t.radii();
    vec![
        Plate { m: Material::Bar, rect: (0.0, 0.0, W, 32.0), radius: 0.0 },
        Plate { m: Material::Menu, rect: (118.0, 38.0, 236.0, 196.0), radius: r.menu },
        Plate { m: Material::Panel, rect: (W - 312.0, 38.0, 300.0, 268.0), radius: r.panel },
        Plate { m: Material::Dock, rect: (W / 2.0 - 262.0, H - 80.0, 524.0, 70.0), radius: 22.0 },
        Plate { m: Material::Tip, rect: (W / 2.0 - 176.0, H - 114.0, 76.0, 26.0), radius: r.tip },
    ]
}

/// The scene's plates over the wallpaper `wp` (its blurred copy `bl`),
/// as `look` makes them: premultiplied BGRA, W x H px.
fn compose(look: &Look, wp: &[[f32; 3]], bl: &[[f32; 3]]) -> Vec<u8> {
    let (w, h) = (W as usize, H as usize);
    let tile = noise_tile();
    let mut img = wp.to_vec();
    for p in plates(&look.theme) {
        let (tint, fx, bright) = match &look.fixed {
            Some(f) => (f.tints[p.m as usize], (p.m != Material::Tip || f.label_glass).then_some(f.fx), f.bright),
            None => (look.theme.material(p.m), look.theme.glass_fx(p.m), 1.0),
        };
        let (x0, y0, pw, ph) = p.rect;
        for y in y0.floor() as usize..((y0 + ph).ceil() as usize).min(h) {
            for x in x0.floor() as usize..((x0 + pw).ceil() as usize).min(w) {
                let k = cover(x as f32 + 0.5, y as f32 + 0.5, p.rect, p.radius);
                if k <= 0.0 {
                    continue;
                }
                let i = y * w + x;
                let mut c = match &fx {
                    // A glass: the blurred backdrop through the graph.
                    Some(fx) => model(bl[i].map(|v| v * bright), fx, tint),
                    // A name label: its tint over the desktop, no blur.
                    None => {
                        let a = tint.a as f32 / 255.0;
                        let t = f3((tint.r, tint.g, tint.b));
                        std::array::from_fn(|j| wp[i][j] * (1.0 - a) + t[j] * a)
                    }
                };
                if let Some(fx) = &fx {
                    let (u, v) = ((x as f32 + 0.5 - x0) / pw, (y as f32 + 0.5 - y0) / ph);
                    // The top light: the line and the glow.
                    let line = fx.spec * (1.0 - v / SPEC_LINE[1].0).max(0.0);
                    let d = (((u - 0.5) / SPEC_GLOW.0).powi(2) + (v / SPEC_GLOW.1).powi(2)).sqrt();
                    let glow = fx.spec * SPEC_GLOW.2 * (1.0 - d).max(0.0);
                    for a in [line, glow] {
                        c = c.map(|v| v * (1.0 - a) + a);
                    }
                    if fx.noise > 0.0 {
                        let n = tile[((y % TILE as usize) * TILE as usize + x % TILE as usize) * 4 + 3] as f32 / 255.0 * fx.noise;
                        c = c.map(|v| v * (1.0 - n) + 0.5 * n);
                    }
                }
                img[i] = std::array::from_fn(|j| img[i][j] * (1.0 - k) + c[j] * k);
            }
        }
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for c in img {
        let b = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        out.extend_from_slice(&[b[2], b[1], b[0], 255]);
    }
    out
}

/// Draws one look's quadrant at (`ox`, `oy`): the composed plates, then
/// the rims and what sits on the plates, in the look's tokens.
unsafe fn quadrant(stage: &kit::Stage, cv: &kit::Canvas, look: &Look, px: &[u8], ox: f32, oy: f32) {
    let dc = &cv.dc;
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: ManuallyDrop::new(None),
    };
    let bmp = dc.CreateBitmap(D2D_SIZE_U { width: W as u32, height: H as u32 }, Some(px.as_ptr() as _), W as u32 * 4, &props).unwrap();
    dc.SetTransform(&Matrix3x2::identity());
    dc.DrawBitmap(&bmp, Some(&D2D_RECT_F { left: ox, top: oy, right: ox + W, bottom: oy + H }), 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
    dc.SetTransform(&Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: ox, M32: oy });
    let t = &look.theme;
    let fam = t.font.family.clone();
    let brush = |c: Rgba| dc.CreateSolidColorBrush(&col(c), None).unwrap();
    let fill = |r: D2D_RECT_F, radius: f32, c: Rgba| dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius }, &brush(c));
    let text = |s: &str, style: ramp::TextStyle, x: f32, y: f32, c: Rgba| -> f32 {
        let (layout, tw, _) = stage.gfx.text_style(s, &fam, style, 1.0).unwrap();
        crate::gfx::draw_text(dc, Vector2 { X: x, Y: y }, &layout, &brush(c), D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        tw
    };
    let rect = |x: f32, y: f32, w: f32, h: f32| D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h };
    let (ink, dim, faint) = (t.text_primary(), t.text_secondary(), t.text_tertiary());
    let rim = match &look.fixed {
        Some(f) if f.rim_today => {
            let mut g = Theme::graphite();
            g.is_light = t.is_light;
            g.rim()
        }
        _ => t.rim(),
    };
    let ps = plates(t);
    for p in &ps {
        if p.m != Material::Bar {
            if let Some(stops) = rim {
                let (x, y, w, h) = p.rect;
                kit::draw_rim_px(cv, x, y, w, h, p.radius, stops, 1.0);
            }
        }
    }
    // The bar.
    let mut x = 10.0;
    for (i, s) in ["NopBar", "파일", "편집", "보기", "창", "도움말"].iter().enumerate() {
        let style = if i == 0 { ramp::BODY_STRONG } else { ramp::BODY };
        if i == 1 {
            fill(rect(x - 2.0, 5.0, 40.0, 22.0), 11.0, t.fill_row(crate::theme::State::Hover));
        }
        x += text(s, style, x + 8.0, 7.0, ink) + 16.0;
    }
    let clock = "11월 11일 (수) 오전 10:24";
    let (layout, cw, _) = stage.gfx.text_style(clock, &fam, ramp::BODY, 1.0).unwrap();
    crate::gfx::draw_text(dc, Vector2 { X: W - cw - 14.0, Y: 7.0 }, &layout, &brush(ink), D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
    // The menu.
    let (mx, my, mw, _) = ps[1].rect;
    let rows = [("새 창", "Ctrl+N"), ("열기…", "Ctrl+O"), ("최근 항목", ""), ("", ""), ("저장", "Ctrl+S"), ("다른 이름으로 저장…", ""), ("", ""), ("창 닫기", "Ctrl+W")];
    let mut y = my + 6.0;
    for (i, (name, key)) in rows.iter().enumerate() {
        if name.is_empty() {
            fill(rect(mx + 14.0, y + 5.0, mw - 28.0, 1.0), 0.0, t.hairline());
            y += 11.0;
            continue;
        }
        if i == 1 {
            fill(rect(mx + 6.0, y, mw - 12.0, 26.0), 7.0, t.fill_row(crate::theme::State::Hover));
        }
        let c = if i == 5 { t.text_disabled() } else { ink };
        text(name, ramp::BODY, mx + 16.0, y + 4.0, c);
        if !key.is_empty() {
            let (layout, kw, _) = stage.gfx.text_style(key, &fam, ramp::FOOTNOTE, 1.0).unwrap();
            crate::gfx::draw_text(dc, Vector2 { X: mx + mw - 16.0 - kw, Y: y + 5.0 }, &layout, &brush(faint), D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }
        y += 26.0;
    }
    // The panel.
    let (px0, py0, pw, _) = ps[2].rect;
    text("여유", ramp::LARGE_TITLE, px0 + 16.0, py0 + 14.0, ink);
    text("지금 PC는 한가해", ramp::FOOTNOTE, px0 + 16.0, py0 + 48.0, dim);
    for (i, (name, v)) in [("CPU", 0.18f32), ("메모리", 0.46), ("GPU", 0.09), ("디스크", 0.04)].iter().enumerate() {
        let y = py0 + 76.0 + i as f32 * 20.0;
        text(name, ramp::FOOTNOTE, px0 + 16.0, y, dim);
        fill(rect(px0 + 76.0, y + 5.0, pw - 140.0, 6.0), 3.0, t.fill_track());
        fill(rect(px0 + 76.0, y + 5.0, (pw - 140.0) * v, 6.0), 3.0, t.fill());
        text(&format!("{}%", (v * 100.0).round()), ramp::FOOTNOTE, px0 + pw - 52.0, y, dim);
    }
    let card = rect(px0 + 12.0, py0 + 162.0, pw - 24.0, 58.0);
    fill(card, 12.0, t.fill_tile(crate::theme::State::Rest));
    for (i, (name, v, c)) in [("브라우저", "12%", (0x3d, 0x7b, 0xea)), ("사진 편집", "6%", (0xff, 0x8a, 0x3a))].iter().enumerate() {
        let y = card.top + 6.0 + i as f32 * 24.0;
        fill(rect(card.left + 8.0, y + 2.0, 18.0, 18.0), 5.0, Rgba { r: c.0, g: c.1, b: c.2, a: 255 });
        text(name, ramp::BODY, card.left + 34.0, y + 1.0, ink);
        text(v, ramp::BODY, card.right - 36.0, y + 1.0, dim);
    }
    fill(rect(px0 + 12.0, py0 + 228.0, pw - 24.0, 30.0), 12.0, t.fill_tile(crate::theme::State::Rest));
    text("작업 관리자", ramp::BODY_STRONG, px0 + pw / 2.0 - 34.0, py0 + 234.0, ink);
    // The dock: colored tiles, a gap, the bin.
    let (dx, dy, _, _) = ps[3].rect;
    let tiles = [(0x4f, 0x9c, 0xf5), (0x2c, 0x9c, 0xf0), (0xf8, 0xc8, 0x4a), (0xf2, 0x4f, 0x72), (0xf0, 0xf0, 0xf4), (0x45, 0x80, 0xf0), (0x2a, 0x2a, 0x2a), (0x8c, 0x8c, 0x92), (0xb0, 0xb0, 0xb5)];
    for (i, c) in tiles.iter().enumerate() {
        let x = dx + 12.0 + i as f32 * 56.0 + if i == 8 { 8.0 } else { 0.0 };
        fill(rect(x, dy + 11.0, 48.0, 48.0), 11.0, Rgba { r: c.0, g: c.1, b: c.2, a: 255 });
        if [0, 1, 3, 6].contains(&i) {
            fill(rect(x + 22.0, dy + 63.0, 4.0, 4.0), 2.0, t.indicator_color());
        }
    }
    fill(rect(dx + 12.0 + 8.0 * 56.0 - 2.0, dy + 16.0, 1.0, 38.0), 0.0, t.dock_separator());
    let (lx, ly, _, _) = ps[4].rect;
    let (lt, _) = t.label_colors();
    let _ = lt;
    // The name label: all but opaque, no halo.
    let (layout, _, _) = stage.gfx.text_style("브라우저", &fam, ramp::FOOTNOTE, 1.0).unwrap();
    dc.DrawTextLayout(Vector2 { X: lx + 12.0, Y: ly + 5.0 }, &layout, &brush(ink), D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
    // Which look.
    dc.SetTransform(&Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: ox, M32: oy });
    let (layout, nw, _) = stage.gfx.text_style(&look.name, &fam, ramp::HEADLINE, 1.0).unwrap();
    fill(rect(12.0, H - 150.0, nw + 20.0, 28.0), 14.0, Rgba { r: 0, g: 0, b: 0, a: 200 });
    dc.DrawTextLayout(Vector2 { X: 22.0, Y: H - 146.0 }, &layout, &brush(Rgba { r: 255, g: 255, b: 255, a: 255 }), D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
    dc.SetTransform(&Matrix3x2::identity());
}

/// Final audit 10: the sheets' PC bars took Windows' default accent
/// (blue). Every look's fills and text are achromatic (the app icons on
/// the card keep their own colors).
#[test]
fn the_sheets_tokens_are_achromatic() {
    for light in [false, true] {
        for look in looks(light) {
            let t = &look.theme;
            for c in [t.fill(), t.fill_track(), t.text_primary(), t.text_secondary(), t.fill_tile(crate::theme::State::Rest)] {
                assert!(c.r == c.g && c.g == c.b, "{} light {light}: {c:?}", look.name);
            }
        }
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_glass_sheets() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let (w, h) = (W as usize, H as usize);
    for wall in &WALLS {
        let wp = wallpaper(wall, w, h);
        for (folder, light) in kit::LOOKS {
            let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), W * 2.0 + 8.0, H * 2.0 + 8.0, 1.0);
            unsafe { cv.dc.Clear(Some(&col(Rgba { r: 0x10, g: 0x10, b: 0x10, a: 255 }))) };
            for (i, look) in looks(light).iter().enumerate() {
                let bl = blur(&wp, w, h, look.blur);
                let px = compose(look, &wp, &bl);
                let (ox, oy) = ((i % 2) as f32 * (W + 8.0), (i / 2) as f32 * (H + 8.0));
                unsafe { quadrant(&stage, &cv, look, &px, ox, oy) };
            }
            let (px, pw, ph) = cv.finish();
            kit::save_png(&px, pw, ph, &kit::harmony_dir().join("glass-v3").join(format!("{}-{folder}.png", wall.name)));
        }
    }
}

/// Glass as the mockup draws it (its recipe as it is, no Luminosity
/// layer) beside the same glass in our tokens, the mockup's on the left. The same scene, wallpaper, blur and CPU stand-in as
/// harmony_glass_sheets, the same text tokens on both (the plates are
/// what is compared).
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_glass_mockup_sheets() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let (w, h) = (W as usize, H as usize);
    for wall in WALLS.iter().filter(|w| w.name != "paper") {
        let wp = wallpaper(wall, w, h);
        for (folder, light) in kit::LOOKS {
            let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), W * 2.0 + 8.0, H, 1.0);
            unsafe { cv.dc.Clear(Some(&col(Rgba { r: 0x10, g: 0x10, b: 0x10, a: 255 }))) };
            for (row, tokens) in looks(light).into_iter().filter(|l| l.fixed.is_none() && l.theme.glass()).enumerate() {
                let key = "글래스";
                let bl = blur(&wp, w, h, tokens.blur);
                let mock = Look { name: format!("{key} 목업"), theme: tokens.theme.clone(), blur: tokens.blur, fixed: Some(mockup(tokens.theme.finish, light)) };
                let ours = Look { name: format!("{key} 토큰"), ..tokens };
                for (c, look) in [mock, ours].iter().enumerate() {
                    let px = compose(look, &wp, &bl);
                    let (ox, oy) = (c as f32 * (W + 8.0), row as f32 * (H + 8.0));
                    unsafe { quadrant(&stage, &cv, look, &px, ox, oy) };
                }
            }
            let (px, pw, ph) = cv.finish();
            kit::save_png(&px, pw, ph, &kit::harmony_dir().join("glass-v3").join(format!("compare-{}-{folder}.png", wall.name)));
        }
    }
}
