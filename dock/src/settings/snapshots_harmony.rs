//! States of the Settings window the snapshots do not take, for the design
//! audit (tests only; see crate::gfx::promo_kit, "harmony"): an open
//! list, the sidebar's search results, "Reset all settings" asked, a
//! shortcut being recorded and an "i" with its tip up. The window's own
//! App and paint (snapshots' harness, default settings, offline), at 2x,
//! put on the audit's backdrop as a window (8 DIP corners, the panels'
//! shadows; its Mica is DWM's: drawn solid).

use std::mem::ManuallyDrop;

use windows::Win32::Graphics::Direct2D::Common::D2D1_PIXEL_FORMAT;
use windows::Win32::Graphics::Direct2D::{D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_BITMAP_OPTIONS_NONE, D2D1_BRUSH_PROPERTIES, D2D1_EXTEND_MODE_CLAMP, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_ROUNDED_RECT};

use super::super::{reset, tooltip, Hit, H0, W0};
use super::*;
use crate::gfx::promo_kit as kit;

/// A window's corners on Windows 11 (DIP).
const WINDOW_R: f32 = 8.0;


/// set_look, with the audit's grey for Windows' accent (kit::theme's).
fn look(h: &mut Harness, light: bool) {
    let mut sys = crate::syslook::current();
    let grey = if light { 0x3A } else { 0xE6 };
    sys.accent = crate::theme::Rgba { r: grey, g: grey, b: grey, a: 255 };
    sys.light = light;
    sys.transparency = false;
    sys.high_contrast = None;
    sys.animations = true;
    h.app.cfg.theme.mode = String::new();
    h.app.refresh_look(sys);
}

/// The window as it is now, `ht` DIPs tall, to
/// `target/harmony-shots/<look>/settings-<name>.png`.
fn save(h: &mut Harness, light: bool, name: &str, ht: f32) {
    let (_, _, px, pw, ph) = paint(h, W0, ht, kit::scale());
    let cv = kit::Canvas::new(W0 + kit::HARMONY_M * 2.0, ht + kit::HARMONY_M * 2.0);
    let m = kit::HARMONY_M;
    let r = kit::rect(m, m, m + W0, m + ht);
    unsafe {
        cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
        // A window, not a panel: only the plate's shadows (its body is the
        // window's own picture, below).
        let clear = crate::theme::Rgba { r: 0, g: 0, b: 0, a: 0 };
        let plate = kit::Plate { radius: WINDOW_R, tint: clear, border: None, highlight: None, shadows: h.app.look.elevation(crate::theme::Elevation::E3), rim: None, rim_w: 1.0, contact: clear, fall: None, grain: 0.0, ground: None, spec: 0.0 };
        kit::draw_plate(&cv, m, m, W0, ht, &plate);
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: ManuallyDrop::new(None),
        };
        let bmp = cv.dc.CreateBitmap(D2D_SIZE_U { width: pw, height: ph }, Some(px.as_ptr() as _), pw * 4, &props).unwrap();
        let bprops = D2D1_BITMAP_BRUSH_PROPERTIES1 { extendModeX: D2D1_EXTEND_MODE_CLAMP, extendModeY: D2D1_EXTEND_MODE_CLAMP, interpolationMode: D2D1_INTERPOLATION_MODE_LINEAR };
        // The bitmap is in px, the canvas in DIPs.
        let brush_props = D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: Matrix3x2 { M11: 1.0 / kit::scale(), M12: 0.0, M21: 0.0, M22: 1.0 / kit::scale(), M31: m, M32: m } };
        let brush = cv.dc.CreateBitmapBrush(&bmp, Some(&bprops), Some(&brush_props)).unwrap();
        cv.dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: WINDOW_R, radiusY: WINDOW_R }, &brush);
    }
    let folder = if light { "light" } else { "dark" };
    kit::harmony_save(cv, folder, &format!("settings-{name}"));
    assert!(kit::harmony_dir().join(folder).join(format!("settings-{name}.png")).is_file());
}

/// Nothing hovered, focused or open.
fn rest(h: &mut Harness) {
    h.app.keyboard = false;
    h.app.focus = None;
    h.app.hover = None;
    h.app.menu = None;
    h.app.tip = None;
    h.app.confirm = None;
    h.app.recording = None;
    h.app.scroll = 0.0;
}

/// Page `i` shown from its top; its hits and its whole height.
fn open(h: &mut Harness, i: usize) -> (Vec<(D2D_RECT_F, Hit)>, f32) {
    h.app.section = i;
    h.app.scroll = 0.0;
    let ht = full_height(h);
    let (hits, ..) = paint(h, W0, ht, 1.0);
    (hits, ht)
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_settings_states() {
    let _l = crate::i18n::test_lock("ko");
    let mut h = harness(false);
    let page = |h: &Harness, f: &dyn Fn(&model::Section) -> bool| h.app.sections.iter().position(f).expect("the page");
    let windows = page(&h, &|s| s.kind == model::Kind::Windows);
    let shortcuts = page(&h, &|s| s.kind == model::Kind::Shortcuts);
    let general = page(&h, &|s| s.tab == model::Tab::General);
    for light in [true, false] {
        look(&mut h, light);

        // A list open (a hot corner's actions), its second row hovered.
        rest(&mut h);
        let (hits, ht) = open(&mut h, windows);
        if let Some(Hit::Menu(ri)) = hits.iter().map(|(_, x)| *x).find(|x| matches!(x, Hit::Menu(_))) {
            h.app.menu = h.app.row_ctl(ri).and_then(|c| c.options()).map(|(_, get, _)| (ri, get(&h.app.cfg)));
            h.app.menu_first = 0;
            h.app.hover = Some(Hit::MenuItem(ri, 1));
            save(&mut h, light, "dropdown", ht);
        }

        // A shortcut's field waiting for keys.
        rest(&mut h);
        let (hits, ht) = open(&mut h, shortcuts);
        if let Some(Hit::Shortcut(ri)) = hits.iter().map(|(_, x)| *x).find(|x| matches!(x, Hit::Shortcut(_))) {
            h.app.recording = Some((ri, 0));
            save(&mut h, light, "recording", ht.min(H0));
        }

        // An "i" under the pointer, its tip up.
        rest(&mut h);
        if let Some(Hit::Info(id)) = hits.iter().map(|(_, x)| *x).find(|x| matches!(x, Hit::Info(_))) {
            h.app.hover = Some(Hit::Info(id));
            h.app.tip = Some(tooltip::Tip { ri: id, from: std::time::Instant::now() - std::time::Duration::from_secs(5), hide: None });
            save(&mut h, light, "info-hover", ht.min(H0));
        }

        // "Reset all settings" asked: the foot of General.
        rest(&mut h);
        let (_, ht) = open(&mut h, general);
        h.app.confirm = Some(reset::Confirm::All);
        h.app.scroll = (ht - H0).max(0.0);
        save(&mut h, light, "reset-confirm", H0);

        // Load the About card's icon through the settings icon loader's key.
        rest(&mut h);
        let about = page(&h, &|s| s.kind == model::Kind::About);
        let key = crate::icons::NOPBAR_ICON_KEY;
        let icon = crate::icons::load(key, (64.0 * kit::scale().min(2.0)) as i32);
        assert!(icon.is_some(), "the bundled app icon loads");
        h.app.pin_icons.borrow_mut().insert(key.into(), icon);
        let (_, ht) = open(&mut h, about);
        save(&mut h, light, "about", ht.min(H0));

        // The sidebar's search: what "아이콘" finds, the best page narrowed.
        rest(&mut h);
        h.app.set_search("아이콘");
        let ht = full_height(&mut h);
        save(&mut h, light, "search", ht.min(H0 * 1.5));
        h.app.set_search("");
    }
}

/// Two pages (General, Appearance) at rest, light and dark, for the font
/// comparison (crate::font_compare): `settings-general.png`,
/// `settings-appearance.png`.
pub(crate) fn font_compare() {
    let _l = crate::i18n::test_lock("ko");
    // The font the comparison set (the harness's own config would put
    // Windows' UI font back when it loads its theme).
    let font = crate::fonts::ui_family();
    let mut h = harness(false);
    h.app.cfg.appearance.font = font;
    let page = |h: &Harness, t: model::Tab| h.app.sections.iter().position(|s| s.tab == t).expect("the page");
    let (general, appearance) = (page(&h, model::Tab::General), page(&h, model::Tab::Appearance));
    for light in [true, false] {
        look(&mut h, light);
        for (i, name) in [(general, "general"), (appearance, "appearance")] {
            rest(&mut h);
            let (_, ht) = open(&mut h, i);
            save(&mut h, light, name, ht.min(H0));
        }
    }
}
