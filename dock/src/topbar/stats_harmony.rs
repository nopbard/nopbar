//! The PC status panel drawn off screen for the design audit (tests only;
//! see crate::gfx::promo_kit, "harmony"): the panel made as `show` makes
//! it (its window never shown), with made-up readings and apps
//! (stats_promo.rs: nothing of this PC is in a picture), painted by its own
//! `paint` over the plate popup::chrome gives it (the panel's tint, rim
//! and E3 shadows, drawn here).
//!
//! The scenes (04-pc-status.md 10): the three states; the details open
//! (with a battery); on battery (the list by power); the wait and "Cannot
//! measure"; the states of what can be pressed (the pointer on a row and
//! on Quit, a press, a picked lens, the keyboard's cursor and its ring);
//! Glass; high contrast; right to left; and the busy panel at 100 and
//! 150 %.

use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

use super::promo::{made_up, staged, Scene};
use super::*;
use crate::gfx::promo_kit as kit;

/// `p` painted at `now` over its plate, at `scale` px per DIP, saved as
/// `target/harmony-shots/<folder>/<name>.png`.
unsafe fn shot(stage: &kit::Stage, p: &Panel, now: u64, light: bool, folder: &str, name: &str, scale: f32) {
    let (m, h) = (kit::HARMONY_M, p.shown_h);
    let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), W + 2.0 * m, h + 2.0 * m, scale);
    cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
    kit::draw_plate(&cv, m, m, W, h, &kit::popup_plate(&p.theme, p.theme.radii().panel, crate::theme::Elevation::E3));
    cv.dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: m * scale, M32: m * scale });
    let mut hits = Vec::new();
    p.paint(&cv.dc, &mut hits, now).expect("painted");
    cv.dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
    kit::harmony_save(cv, folder, name);
    assert!(kit::harmony_dir().join(folder).join(format!("{name}.png")).is_file(), "{name}");
}

/// The apps using the most power (made up).
fn power() -> Vec<Hog> {
    vec![made_up("동영상 편집기", true, 0.46), made_up("브라우저", true, 0.31), made_up("백업 도우미", false, 0.12)]
}

/// One scene in both looks: `set` changes the staged panel (what the
/// picture is about); it is staged again after, at `wait` ms into the
/// wait when `wait` is given (no reading has come).
unsafe fn scene(stage: &kit::Stage, name: &str, of: Scene, theme_of: impl Fn(bool) -> Theme, wait: Option<u64>, scales: &[f32], set: impl Fn(&mut Panel)) {
    for (folder, light) in kit::LOOKS {
        let theme = theme_of(light);
        let (mut p, now) = staged(stage, &theme, of);
        set(&mut p);
        match wait {
            Some(ms) => p.stage(Snapshot::default(), now, now - ms),
            None => p.stage(p.snap.clone(), now, now - 61_000),
        }
        for s in scales {
            let file = if *s == kit::scale() { name.to_string() } else { format!("{name}@{}", (s * 100.0) as u32) };
            shot(stage, &p, now, light, folder, &file, *s);
        }
        let _ = DestroyWindow(p.hwnd);
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_stats() {
    use crate::syslook::{HighContrast, SysLook};
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let two = [kit::scale()];
    let plain = |light: bool| kit::theme(light);
    unsafe {
        super::promo::class();
        // The three states (the busy one at 100 and 150 % too).
        scene(&stage, "stats-calm", Scene::Calm, plain, None, &two, |_| {});
        scene(&stage, "stats-busy", Scene::Busy, plain, None, &[kit::scale(), 1.0, 1.5], |_| {});
        scene(&stage, "stats-very-busy", Scene::VeryBusy, plain, None, &two, |_| {});
        // Everything on: the details open, with a battery line.
        scene(&stage, "stats-full", Scene::Busy, plain, None, &[kit::scale(), 1.0, 1.5], |p| {
            p.details = true;
            p.battery = true;
            p.snap.power = sysstat::Power { battery: Some(72), on_ac: true, secs_left: None, saver: false };
        });
        // On battery, all calm: the list by power, Quit on the app using
        // the most; and a calm panel with a lens picked (the GPU's).
        scene(&stage, "stats-power", Scene::Calm, plain, None, &two, |p| {
            p.battery = true;
            p.on_battery = true;
            p.power = power();
            p.snap.power = sysstat::Power { battery: Some(72), on_ac: false, secs_left: Some(3 * 3600 + 25 * 60), saver: false };
        });
        scene(&stage, "stats-picked", Scene::Calm, plain, None, &two, |p| {
            p.picked = Some(Lens::Gpu);
            p.gpu = vec![made_up("브라우저", true, 0.52), made_up("편집기", true, 0.31), made_up("음악", false, 0.09)];
        });
        // Quiet states: the wait (dashes), nothing in the list, a
        // resource this PC does not tell, "Cannot measure".
        scene(&stage, "stats-waiting", Scene::Calm, plain, Some(1_000), &two, |_| {});
        scene(&stage, "stats-failed", Scene::Calm, plain, Some(6_000), &two, |_| {});
        scene(&stage, "stats-no-gpu", Scene::Calm, plain, None, &two, |p| {
            p.snap.gpu = None;
            p.snap.gpu_history = Spark::new(sysstat::SPARK_SPAN_MS);
            p.snap.apps_cpu.clear();
        });
        // What can be pressed, in its states: the pointer on an app's row;
        // on Quit; Task Manager pressed; the keyboard's cursor on a bar
        // (the disk's: the fill moves, the list stays); Tab's ring on Quit
        // and on "Details".
        scene(&stage, "stats-hover", Scene::Busy, plain, None, &two, |p| p.hover = Some(Hit::App(1)));
        scene(&stage, "stats-hover-quit", Scene::Busy, plain, None, &two, |p| p.hover = Some(Hit::Quit));
        scene(&stage, "stats-pressed", Scene::Busy, plain, None, &two, |p| {
            p.hover = Some(Hit::TaskManager);
            p.pressed = Some(Hit::TaskManager);
        });
        scene(&stage, "stats-cursor", Scene::Busy, plain, None, &two, |p| {
            p.focus = Some(Hit::Meter(Res::Disk));
            p.keyed = true;
        });
        scene(&stage, "stats-focus", Scene::Busy, plain, None, &two, |p| {
            p.focus = Some(Hit::Quit);
            p.keyed = true;
        });
        scene(&stage, "stats-focus-more", Scene::Calm, plain, None, &two, |p| {
            p.focus = Some(Hit::More);
            p.keyed = true;
        });
        // Glass (the tint's opacity over the backdrop; the blur itself is
        // the compositor's).
        let glass = |light: bool| {
            let mut raw = Theme::graphite();
            raw.finish = crate::theme::Finish::Glass;
            raw.resolve_fx(&SysLook { light, transparency: true, animations: true, ..Default::default() }, true)
        };
        scene(&stage, "stats-glass", Scene::Busy, glass, None, &two, |_| {});
        // High contrast (the system colors fixed here: black, white, a sky
        // blue Highlight): the busy panel, and the keyboard's cursor on a
        // row (a Highlight fill, HighlightText inside it).
        let hc = |_light: bool| {
            let (white, black, sky) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 }, Rgba { r: 26, g: 235, b: 255, a: 255 });
            Theme::graphite().resolve(&SysLook { high_contrast: Some(HighContrast { window: black, text: white, highlight: sky, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() })
        };
        // The bundled app icon in the Power list, including high contrast.
        let with_own = |p: &mut Panel| {
            let mut own = made_up("NopBar", false, 0.04);
            own.procs = vec![(std::process::id(), 0)];
            own.icon = crate::hogs::load_icon(crate::icons::NOPBAR_ICON_KEY);
            assert!(own.icon.is_some());
            p.battery = true;
            p.on_battery = true;
            p.power = vec![made_up("동영상 편집기", true, 0.46), own];
            p.snap.power = sysstat::Power { battery: Some(72), on_ac: false, secs_left: Some(3 * 3600 + 25 * 60), saver: false };
        };
        scene(&stage, "stats-own", Scene::Calm, plain, None, &[kit::scale(), 1.0], with_own);
        scene(&stage, "stats-own-hc", Scene::Calm, hc, None, &two, with_own);
        scene(&stage, "stats-hc", Scene::Busy, hc, None, &two, |p| p.hover = Some(Hit::App(1)));
        scene(&stage, "stats-hc-cursor", Scene::VeryBusy, hc, None, &two, |p| {
            p.details = true;
            p.focus = Some(Hit::Meter(Res::Gpu));
            p.keyed = true;
        });
    }
    // Right to left: the layout reflected, the bars filling from the right.
    drop(_l);
    let _l = crate::i18n::test_lock("ar");
    unsafe {
        scene(&stage, "stats-rtl", Scene::Busy, plain, None, &two, |p| {
            p.rtl = true;
            p.details = true;
            p.name_w = p.measure_names();
        });
    }
}

/// The busy panel in both looks for the font comparison
/// (crate::font_compare): `stats-busy.png`.
pub(crate) fn font_compare() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    unsafe {
        super::promo::class();
        scene(&stage, "stats-busy", Scene::Busy, kit::theme, None, &[kit::scale()], |_| {});
    }
}

/// The keyboard's ring and cursor show only after a key that moves the
/// focus (crate::input): media, volume and letter keys leave them off, Tab
/// and the arrows show them, a press of the pointer hides them.
#[test]
fn the_ring_shows_only_after_a_key_that_moves_the_focus() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_TAB};
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    unsafe {
        super::promo::class();
        let theme = kit::theme(false);
        let (mut p, _) = staged(&stage, &theme, Scene::Busy);
        p.keyed = false;
        crate::input::sample::sequence("pc status", &mut p, VK_TAB.0, VK_DOWN.0, |p| p.keyed, |p, vk| p.on_key(vk), |p| p.on_down(false, 4.0, 4.0));
        // The wndproc goes through them.
        let src = include_str!("stats.rs");
        assert!(src.contains("with_panel(|p| p.on_key(vk));") && src.contains("p.on_down(msg == WM_LBUTTONDOWN, x, y);"));
        assert!(src.contains("let show = crate::input::reader_focus(p.keyed);"), "a screen reader's focus by the rule");
        let _ = DestroyWindow(p.hwnd);
    }
}
