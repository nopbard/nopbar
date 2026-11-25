//! "About this PC" drawn off screen for the design audit (tests only; see
//! crate::gfx::promo_kit, "harmony"): the window made as `show` makes it
//! (never shown, its helpers never started: no wallpaper, no disk read),
//! for a made-up PC (system::harmony::fake: nothing of this PC), its
//! screen off, painted by its own `paint` over the plate popup::chrome
//! gives it (the popup tint, rim and E3 shadows, drawn here).
//!
//! The scenes (harmony B): a laptop at rest, the pointer on "More info…";
//! a desktop with the keyboard's ring on the copy button and the pointer
//! on it; light, dark and high contrast, at 100 and 150 %.
//!
//! If this PC's facts were read before the made-up ones could be set,
//! nothing is drawn.

use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, LoadCursorW, RegisterClassExW, IDC_ARROW, WNDCLASSEXW};

use super::*;
use crate::gfx::promo_kit as kit;

/// The three looks: light, dark, and high contrast (the system colors
/// fixed here: black, white, a sky blue Highlight).
fn looks() -> [(&'static str, Theme, bool); 3] {
    use crate::syslook::{HighContrast, SysLook};
    let (white, black, sky) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 }, Rgba { r: 26, g: 235, b: 255, a: 255 });
    let hc = Theme::graphite().resolve(&SysLook { high_contrast: Some(HighContrast { window: black, text: white, highlight: sky, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() });
    [("light", kit::theme(true), true), ("dark", kit::theme(false), false), ("hc", hc, false)]
}

/// One scene of `form` in every look at 100 and 150 %, `set` putting the
/// window in its state; saved as `<look>/<name>@100.png` and `@150`.
unsafe fn scene(stage: &kit::Stage, name: &str, form: Form, set: impl Fn(&mut Panel)) {
    for (folder, theme, light) in looks() {
        for s in [1.0f32, 1.5] {
            // Far off every display: the window is never shown anyway.
            let work = RECT { left: -32000, top: -32000, right: -30000, bottom: -30500 };
            let mut p = Panel::new(&stage.gfx, &theme, work, work, 1.0).expect("the window");
            p.scale = s;
            p.fit = 1.0;
            p.rtl = false;
            p.dec = '.';
            // 512 GB, 212 free; its screen off.
            p.disk = Some((512 << 30, 212 << 30));
            p.disk_in = true;
            p.facts_in = true;
            p.form = FormState::Decided(form);
            (p.light, p.dark) = (art_icon(&art_name(form, true)), art_icon(&art_name(form, false)));
            set(&mut p);
            p.height = p.now().2.height;
            assert_eq!(p.now().0.pc, "하모니 노트북", "the made-up PC only");
            let m = kit::HARMONY_M;
            let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), W + 2.0 * m, p.height + 2.0 * m, s);
            let backdrop = if folder == "hc" { Rgba { r: 0x20, g: 0x20, b: 0x20, a: 255 } } else { kit::harmony_backdrop(light) };
            cv.dc.Clear(Some(&kit::col(backdrop)));
            cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 });
            kit::draw_plate(&cv, m, m, W, p.height, &kit::popup_plate(&theme, theme.radii().panel, crate::theme::Elevation::E3));
            cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: m * s, M32: m * s });
            let mut buttons = p.buttons;
            p.paint(&cv.dc, &mut buttons).expect("painted");
            cv.dc.SetTransform(&Matrix3x2::identity());
            let file = format!("{name}@{}", (s * 100.0) as u32);
            kit::harmony_save(cv, folder, &file);
            let _ = DestroyWindow(p.hwnd);
            assert!(kit::harmony_dir().join(folder).join(format!("{file}.png")).is_file());
        }
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_about_pc() {
    let _l = crate::i18n::test_lock("ko");
    if !system::harmony::fake() {
        eprintln!("about-pc: this PC's facts were read first; not drawn");
        return;
    }
    let stage = kit::stage();
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("NopBarAbout"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // A laptop at rest, the pointer on "More info…".
        scene(&stage, "about-pc", Form::Laptop, |p| p.hover = Some(Btn::More));
        // A desktop, the keyboard's ring and the pointer on the copy button.
        scene(&stage, "about-pc-desktop", Form::Desktop, |p| {
            p.focus = Some(Btn::Copy);
            p.ring = true;
            p.hover = Some(Btn::Copy);
        });
    }
}

/// The ring on the buttons shows only after a key that moves the focus
/// (crate::input): media, volume and letter keys leave it off, Tab shows
/// it, a press of the pointer hides it; a screen reader's focus moves the
/// focus and shows it only as it was, or with Windows' keyboard cues on.
#[test]
fn the_ring_shows_only_after_a_key_that_moves_the_focus() {
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_TAB;
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: w!("NopBarAbout"), ..Default::default() };
        RegisterClassExW(&wc);
        let work = RECT { left: -32000, top: -32000, right: -30000, bottom: -30500 };
        let theme = kit::theme(false);
        let mut p = Panel::new(&stage.gfx, &theme, work, work, 1.0).expect("the window");
        let shows = |p: &Panel| p.ring && p.focus.is_some();
        crate::input::sample::sequence("about this pc", &mut p, VK_TAB.0, VK_TAB.0, shows, |p, vk| p.on_key(vk, false, false), |p| p.on_down());
        // Ctrl+C copies, no ring.
        p.on_key(b'C' as u16, true, false);
        assert!(!p.ring);
        // A screen reader: the focus moves, the ring by the rule.
        p.focus = None;
        p.act(asked("more", crate::a11y::Act::Focus), false);
        assert_eq!((p.focus, p.ring), (Some(Btn::More), crate::input::keyboard_cues()));
        let src = include_str!("about.rs");
        assert!(src.contains("with_panel(|p| p.on_key(vk, ctrl, shift));") && src.contains("p.on_down();"));
        let _ = DestroyWindow(p.hwnd);
    }
}

/// The states inventory (final/states-inventory.md, 2026-11-11): right to
/// left (Arabic), the keyboard's ring on "More info…", the pointer on the
/// copy button.
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_about_pc_rtl() {
    let _l = crate::i18n::test_lock("ar");
    if !system::harmony::fake() {
        eprintln!("about-pc: this PC's facts were read first; not drawn");
        return;
    }
    let stage = kit::stage();
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: w!("NopBarAbout"), ..Default::default() };
        RegisterClassExW(&wc);
        scene(&stage, "about-pc-rtl", Form::Laptop, |p| {
            p.rtl = true;
            p.focus = Some(Btn::More);
            p.ring = true;
            p.hover = Some(Btn::Copy);
        });
    }
}
