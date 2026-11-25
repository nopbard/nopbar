//! The Control Center drawn off screen for the design audit (tests only;
//! see crate::gfx::promo_kit, "harmony"): the panel made as `show` makes
//! it (Panel::make: its window never shown), with workers that do nothing
//! (no radio, display, sound or media of this PC is asked for), for a
//! made-up state put into what the workers would have reported, painted
//! by its own `paint` over the plate popup::chrome gives it (the popup
//! tint, rim and E3 shadows, drawn here).
//!
//! What the panel keeps as visuals of their own (the buttons, the thick
//! sliders, the album art, the position bar) is drawn here from the same
//! specs, where sync_parts puts them: a button's shape (or outline) and
//! glyph by the masks' own drawing (paint_shape) in the spec's colors; a
//! slider and the position bar by the paint their surfaces are made with
//! (draw_pill, draw_time); the art cut to its corners. Nothing springs,
//! fades or presses smaller here.
//!
//! The scenes (harmony A): the grid at rest (toggles on and off, the
//! sliders' values, a track playing, a removable device), the grid's other
//! states (the pointer on a tile, a slider held, the keyboard's ring,
//! muted and paused), and the two lists (Project's modes, the sound
//! devices and apps); light, dark and high contrast, at 100 and 150 %.

use std::mem::ManuallyDrop;

use windows::Win32::Graphics::Direct2D::{D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1};

use super::*;
use crate::gfx::promo_kit as kit;

/// Workers that never run: what is sent to them goes nowhere.
fn idle(_: &Arc<Shared>) -> Workers {
    Workers::idle()
}

/// Runs `f` with the canvas cut to the rounded rect `r` (DIP).
unsafe fn clipped(cv: &kit::Canvas, r: D2D_RECT_F, rad: f32, f: impl FnOnce()) {
    let geom = cv.factory.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }).unwrap();
    let params = D2D1_LAYER_PARAMETERS1 {
        contentBounds: r,
        geometricMask: ManuallyDrop::new(Some(geom.cast().unwrap())),
        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        maskTransform: Matrix3x2::identity(),
        opacity: 1.0,
        opacityBrush: ManuallyDrop::new(None),
        layerOptions: D2D1_LAYER_OPTIONS1_NONE,
    };
    cv.dc.PushLayer(&params, None);
    let _ = ManuallyDrop::into_inner(params.geometricMask);
    f();
    cv.dc.PopLayer();
}

/// Runs `f` in DIPs from (`x`, `y`) at `s` px per DIP.
unsafe fn at<R>(cv: &kit::Canvas, x: f32, y: f32, s: f32, f: impl FnOnce(&ID2D1DeviceContext) -> R) -> R {
    cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: x * s, M32: y * s });
    let r = f(&cv.dc);
    cv.dc.SetTransform(&Matrix3x2::identity());
    r
}

/// The parts `plan` asks for, where sync_parts puts their visuals; the
/// panel's top left at (`m`, `m`) DIP, at `s` px per DIP.
unsafe fn draw_parts(cv: &kit::Canvas, p: &Panel, plan: &[Spec], m: f32, s: f32) {
    for spec in plan {
        match spec {
            Spec::Disc(d) => {
                let side = d.r.right - d.r.left;
                let px = (side * s).ceil() as u32;
                let (px0, py0) = p.placed(d.r.left, d.r.top, side);
                let (x, y) = ((m + px0) * s, (m + py0) * s);
                let k = px as f32 / side;
                let shape = MaskKey::Shape(px, (d.rad * k * 100.0).round() as u32, d.ring.map_or(0, |w| (w * k * 100.0).round() as u32));
                let ink = rgba(d.ink);
                let glyph_ink = D2D1_COLOR_F { a: ink.a * if d.dim { DIM } else { 1.0 }, ..ink };
                cv.dc.SetTransform(&Matrix3x2::identity());
                paint_shape(&cv.dc, x, y, shape, side, s, px, rgba(d.fill)).expect("a button");
                paint_shape(&cv.dc, x, y, MaskKey::Glyph(d.glyph, px, (d.gsize * 100.0) as u32), side, s, px, glyph_ink).expect("its glyph");
                if d.mark {
                    paint_shape(&cv.dc, x, y, MaskKey::Mark(px), side, s, px, D2D1_COLOR_F { a: ink.a * MARK_ALPHA, ..ink }).expect("its mark");
                }
            }
            Spec::Pill(sp) => {
                let (px0, py0) = p.placed(sp.r.left, sp.r.top, sp.r.right - sp.r.left);
                let left = m + px0;
                let r = rect(left, m + py0, left + sp.r.right - sp.r.left, m + py0 + sp.r.bottom - sp.r.top);
                // Held: as the visual's scale leaves it (a little thicker).
                let grow = if sp.held { HELD_GROW } else { 1.0 };
                let cy = (r.top + r.bottom) / 2.0;
                cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s * grow, M31: r.left * s, M32: (cy - CAPSULE * grow / 2.0) * s });
                p.draw_pill(&cv.dc, &sp.look).expect("a slider");
                cv.dc.SetTransform(&Matrix3x2::identity());
            }
            Spec::Art(r, art) => {
                let Some(a) = art else { continue };
                let (px0, py0) = p.placed(r.left, r.top, ART_DIP);
                let left = m + px0;
                let rr = rect(left, m + py0, left + ART_DIP, m + py0 + ART_DIP);
                cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 });
                clipped(cv, rr, p.radius(Radius::Control), || {
                    let bmp = Panel::bitmap(&cv.dc, a.px, a.px, &a.pixels).expect("the art");
                    cv.dc.DrawBitmap(&bmp, Some(&rr), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None);
                });
                cv.dc.SetTransform(&Matrix3x2::identity());
            }
            Spec::Time(t) => {
                let (px0, py0) = p.placed(t.r.left, t.r.top, t.r.right - t.r.left);
                at(cv, m + px0, m + py0, s, |dc| p.draw_time(dc, t)).expect("the position bar")
            }
        }
    }
}

/// Album art for the picture: a plain grey square, lighter to one corner.
fn art() -> Arc<Art> {
    let px = 96u32;
    let mut pixels = Vec::with_capacity((px * px * 4) as usize);
    for y in 0..px {
        for x in 0..px {
            let v = (0x60 + (x + y) * 0x50 / (2 * px)) as u8;
            pixels.extend_from_slice(&[v, v, v, 255]);
        }
    }
    Arc::new(Art { px, pixels })
}

/// What the workers would have reported, made up: Wi-Fi on a network,
/// Bluetooth off, a hotspot to share, a display that dims, two displays, a
/// battery (energy saver on), Keep Awake on, two outputs and a
/// microphone, two apps playing, a track with a timeline.
fn made_up(r: &mut Remote) {
    let dev = |id: &str, name: &str, kind: devices::Kind, default: bool| Endpoint { id: id.into(), name: name.into(), kind, default };
    let app = |key: u64, name: &str, level: u8| AppMix { key, name: name.into(), level, muted: false, active: true, icon: None };
    r.wifi = RadioView::On;
    r.ssid = Ssid::Some("집 Wi-Fi".into());
    r.bt = RadioView::Off;
    r.brightness = Some(Some(70));
    r.radios = Some(true);
    r.energy = Some(true);
    r.hotspot = RadioView::Off;
    r.displays = Displays::Many(Some(Topology::Extend));
    r.battery = Some((76, false));
    r.awake = true;
    r.outputs = Some(vec![dev("harmony-out-1", "스피커", devices::Kind::Speakers, false), dev("harmony-out-2", "하모니 헤드폰", devices::Kind::Headphones, true)]);
    r.inputs = Some(vec![dev("harmony-in-1", "마이크 배열", devices::Kind::Microphone, true), dev("harmony-in-2", "헤드셋 마이크", devices::Kind::Microphone, false)]);
    r.apps = Some(vec![app(1, "음악", 70), app(2, "브라우저", 40)]);
    r.media = Some(Media {
        title: "새벽 유리".into(),
        artist: "하모니 악단".into(),
        app: "harmony.music".into(),
        playing: true,
        prev: true,
        next: true,
        toggle: true,
        art: Some(art()),
        timeline: Some(Timeline { start_ms: 0, end_ms: 220_000, pos_ms: 72_000, at: Instant::now(), rate: 1.0, seekable: true, seek_min_ms: 0, seek_max_ms: 0 }),
        gen: 1,
        session: 1,
        refused: 0,
    });
}

/// A made-up USB drive.
fn usb() -> crate::removable::model::Shown {
    use crate::removable::model::{Device, Kind, Note, Shown, Volume};
    Shown { device: Device { id: "harmony-usb".into(), gen: 1, name: "USB 드라이브".into(), kind: Kind::Usb, volumes: vec![Volume { letters: vec!["E:".into()], label: String::new() }], ..Default::default() }, note: Note::Ready }
}

/// The three looks: light, dark, and high contrast (the system colors
/// fixed here: black, white, a sky blue Highlight).
fn looks() -> [(&'static str, Theme, bool); 3] {
    use crate::syslook::{HighContrast, SysLook};
    let (white, black, sky) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 }, Rgba { r: 26, g: 235, b: 255, a: 255 });
    let hc = Theme::graphite().resolve(&SysLook { high_contrast: Some(HighContrast { window: black, text: white, highlight: sky, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT }), ..Default::default() });
    [("light", kit::theme(true).windows_shell(), true), ("dark", kit::theme(false).windows_shell(), false), ("hc", hc, false)]
}

/// The panel as `show` makes it (its window never shown, the workers
/// idle) over the made-up state, at `s`.
unsafe fn made_panel(stage: &kit::Stage, theme: &Theme, light: bool, s: f32) -> Panel {
    register_class();
    let shared = Arc::new(Shared::new());
    shared.set_quiet(made_up);
    let first = layout(&shape_of(&shared.snapshot(), None, removable_rows()));
    // Far off every display (the window is never shown anyway): a bar's
    // button at the top right of 1920 x 1080 DIP.
    let px = |dip: f32| -32000 + (dip * s) as i32;
    let button = RECT { left: px(1800.0), top: px(0.0), right: px(1840.0), bottom: px(32.0) };
    let work = RECT { left: px(0.0), top: px(32.0), right: px(1920.0), bottom: px(1080.0) };
    let req = Request { gfx: &stage.gfx, theme, button, scale: s, volume: Some((40, false)) };
    let mut p = Panel::make(&req, shared, first, work, !light, idle).expect("the panel");
    p.size = 1.0;
    p.rtl = false;
    p
}

/// One scene in every look at 100 and 150 %: `set` puts the panel in the
/// state the picture is about (after the made-up state is in), saved as
/// `<look>/<name>@100.png` and `@150`.
unsafe fn scene(name: &str, set: impl Fn(&mut Panel)) {
    let stage = kit::stage();
    // 100 and 150 %, and the run's own scale when one is asked for
    // (HARMONY_SCALE: the states inventory's 125 and 175 %).
    let mut scales = vec![1.0f32, 1.5];
    if std::env::var_os("HARMONY_SCALE").is_some() && !scales.contains(&kit::scale()) {
        scales.push(kit::scale());
    }
    for (folder, theme, light) in looks() {
        for &s in &scales {
            let mut p = made_panel(&stage, &theme, light, s);
            set(&mut p);
            p.resize(false);
            let remote = p.shared.snapshot();
            let layout = p.layout_now(&remote);
            let m = kit::HARMONY_M;
            let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), W + 2.0 * m, p.height + 2.0 * m, s);
            let backdrop = if folder == "hc" { Rgba { r: 0x20, g: 0x20, b: 0x20, a: 255 } } else { kit::harmony_backdrop(light) };
            cv.dc.Clear(Some(&kit::col(backdrop)));
            cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 });
            kit::draw_plate(&cv, m, m, W, p.height, &kit::popup_plate(&theme, PANEL_CORNER, Elevation::E3));
            let (mut hits, mut sliders) = (Vec::new(), Vec::new());
            if p.rtl {
                // As redraw draws it right to left: mirrored whole.
                cv.dc.SetTransform(&mirrored(s, m * s, m * s));
                p.paint(&cv.dc, &mut hits, &mut sliders, &remote, &layout).expect("painted");
                cv.dc.SetTransform(&Matrix3x2::identity());
            } else {
                at(&cv, m, m, s, |dc| p.paint(dc, &mut hits, &mut sliders, &remote, &layout)).expect("painted");
            }
            let plan = std::mem::take(&mut *p.plan.borrow_mut());
            cv.dc.SetTransform(&Matrix3x2 { M11: s, M12: 0.0, M21: 0.0, M22: s, M31: 0.0, M32: 0.0 });
            clipped(&cv, rect(m, m, m + W, m + p.height), PANEL_CORNER, || draw_parts(&cv, &p, &plan, m, s));
            cv.dc.SetTransform(&Matrix3x2::identity());
            let file = format!("{name}@{}", (s * 100.0) as u32);
            kit::harmony_save(cv, folder, &file);
            p.workers.quit(&p.shared);
            let _ = DestroyWindow(p.hwnd);
            assert!(kit::harmony_dir().join(folder).join(format!("{file}.png")).is_file());
        }
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_control() {
    let _l = crate::i18n::test_lock("ko");
    FAKE_ROWS.with(|f| *f.borrow_mut() = Some(vec![usb()]));
    unsafe {
        // At rest: Wi-Fi, Keep Awake and energy saver on, Bluetooth and the
        // hotspot off, brightness 70, volume 40, a track playing, a USB
        // drive.
        scene("control", |_| {});
        // The other states: the pointer on a 1 x 1 tile (its fill up),
        // brightness held (its value shown, thicker), the keyboard's ring on
        // Project; Wi-Fi off, Bluetooth and the hotspot on, dark mode the
        // other way, muted, paused.
        scene("control-states", |p| {
            p.hover = Some(Hit::Quick(Quick::NightLight));
            p.keyboard = true;
            p.focus = Some(Hit::Project);
            p.sliding = Some(Hit::Brightness);
            p.shared.set_quiet(|r| {
                r.wifi = RadioView::Off;
                r.bt = RadioView::On;
                r.hotspot = RadioView::On;
                if let Some(m) = r.media.as_mut() {
                    m.playing = false;
                }
            });
            p.volume = Some((40, true));
            p.dark = !p.dark;
        });
        // Project's list, a mode under the pointer.
        scene("control-project", |p| {
            p.open = Some(Section::Project);
            p.hover = Some(Hit::Mode(Topology::Clone));
        });
        // The sound list: outputs, inputs, the apps' volumes; the keyboard
        // on the back button.
        scene("control-sound", |p| {
            p.open = Some(Section::Sound);
            p.keyboard = true;
            p.focus = Some(Hit::Back);
        });
        // The connections' rows pressed (2026-11-11 review): Wi-Fi off,
        // its name held down; high contrast fills the row with Highlight
        // and the chip on it takes the row's inverted colors.
        scene("control-row-pressed", |p| {
            p.shared.set_quiet(|r| r.wifi = RadioView::Off);
            p.hover = Some(Hit::WifiName);
            p.pressed = Some(Hit::WifiName);
        });
    }
    FAKE_ROWS.with(|f| *f.borrow_mut() = None);
}

/// The states inventory (final/states-inventory.md, 2026-11-11): the
/// sliders at 0, 50 and 100 %, muted, the keyboard's ring on a slider
/// (muted: the owner's screenshot, now only after a key), every radio off
/// or not there, still loading, locked; long names (a track, an artist, a
/// device, an app) cut. (A list still asking Windows cannot be opened:
/// Hit::Outputs does nothing until there is something to list.)
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_control_values() {
    let _l = crate::i18n::test_lock("ko");
    FAKE_ROWS.with(|f| *f.borrow_mut() = Some(vec![usb()]));
    unsafe {
        for v in [0u8, 50, 100] {
            scene(&format!("control-volume-{v}"), move |p| {
                p.volume = Some((v, false));
                p.shared.set_quiet(move |r| r.brightness = Some(Some(v)));
            });
        }
        scene("control-muted-focus", |p| {
            p.volume = Some((40, true));
            p.keyboard = true;
            p.focus = Some(Hit::Volume);
        });
        scene("control-slider-focus", |p| {
            p.keyboard = true;
            p.focus = Some(Hit::Brightness);
            p.hover = Some(Hit::Volume);
        });
        scene("control-radios", |p| {
            p.shared.set_quiet(|r| {
                r.wifi = RadioView::Missing;
                r.bt = RadioView::Locked(false);
                r.hotspot = RadioView::Loading;
                r.brightness = Some(None);
                r.energy = None;
                r.awake = false;
                r.media = None;
            });
            p.volume = None;
        });
        scene("control-long", |p| {
            p.shared.set_quiet(|r| {
                r.ssid = Ssid::Some("아주 긴 이름의 공유기 5G 확장 네트워크 하모니".into());
                if let Some(m) = r.media.as_mut() {
                    m.title = "아주 길고 긴 노래 제목이 패널 끝까지 가고도 남는 경우".into();
                    m.artist = "하모니 필하모닉 오케스트라와 합창단 그리고 친구들".into();
                }
            });
        });
        scene("control-sound-long", |p| {
            p.open = Some(Section::Sound);
            p.shared.set_quiet(|r| {
                let dev = |id: &str, name: &str, kind: devices::Kind, default: bool| Endpoint { id: id.into(), name: name.into(), kind, default };
                r.outputs = Some(vec![dev("o1", "스피커 (Realtek High Definition Audio 장치 두 번째 출력)", devices::Kind::Speakers, true), dev("o2", "하모니 무선 헤드폰", devices::Kind::Headphones, false)]);
                r.inputs = Some(Vec::new());
                r.apps = Some(vec![AppMix { key: 1, name: "아주 긴 이름의 음악 재생 앱 하모니 플레이어".into(), level: 0, muted: true, active: true, icon: None }]);
            });
        });
    }
    FAKE_ROWS.with(|f| *f.borrow_mut() = None);
}

/// Right to left (Arabic): the grid mirrored whole, the thick sliders'
/// insides with it (the glyph and the mute button at the right end, the
/// fill growing to the left: 2026-11-11 review), the value shown.
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_control_rtl() {
    let _l = crate::i18n::test_lock("ar");
    FAKE_ROWS.with(|f| *f.borrow_mut() = Some(vec![usb()]));
    unsafe {
        scene("control-rtl", |p| {
            p.rtl = true;
            p.sliding = Some(Hit::Brightness);
            p.hover = Some(Hit::Mute);
        });
    }
    FAKE_ROWS.with(|f| *f.borrow_mut() = None);
}

#[test]
fn the_slider_groove_spans_the_full_height() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let scale = 1.5f32;
    for (folder, theme, light) in looks().into_iter().take(2) {
        unsafe {
            let p = made_panel(&stage, &theme, light, scale);
            let r = p.shared.snapshot();
            let strip = p.layout_now(&r).at(Item::Brightness).unwrap();
            let (slider, _) = strip_slider(strip, false);
            let mut look = p.pill_spec(Hit::Brightness, slider, &r, &p.tones()).look;
            look.v = 0.0;
            let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), look.w, CAPSULE, scale);
            cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
            cv.dc.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: 0.0, M32: 0.0 });
            p.draw_pill(&cv.dc, &look).expect("the slider");
            let (pixels, width, height) = cv.finish();
            let x = (look.w * 0.8 * scale) as usize;
            let value = |y: usize| pixels[(y * width as usize + x) * 4 + 1];
            assert_eq!(height, (CAPSULE * scale) as u32);
            assert_eq!(value(0), value(height as usize / 2), "{folder}");
            assert_eq!(value(height as usize - 1), value(height as usize / 2), "{folder}");
            assert_ne!(value(0), kit::harmony_backdrop(light).g, "{folder}");
            p.workers.quit(&p.shared);
            let _ = DestroyWindow(p.hwnd);
        }
    }
}

/// The 2026-11-11 review (P2), on the panel itself: a removable device's
/// eject is in the reader's tree in every state, where it is drawn, with
/// its status; it takes input only when it can be asked (ready, or
/// refused: try again).
#[test]
fn an_ejects_node_stays_in_the_tree_while_it_goes() {
    use crate::removable::model::{Note, Refusal};
    let _l = crate::i18n::test_lock("en");
    let stage = kit::stage();
    let (_, theme, light) = looks().into_iter().next().unwrap();
    for note in [Note::Ready, Note::Ejecting, Note::Safe, Note::Refused(Refusal::Failed)] {
        let row = crate::removable::model::Shown { note: note.clone(), ..usb() };
        let h = Hit::Eject(removable_key(&row.device));
        FAKE_ROWS.with(|f| *f.borrow_mut() = Some(vec![row.clone()]));
        unsafe {
            let mut p = made_panel(&stage, &theme, light, 1.0);
            let remote = p.shared.snapshot();
            let layout = p.layout_now(&remote);
            p.height = layout.height;
            let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), W, p.height, 1.0);
            let (mut hits, mut sliders) = (Vec::new(), Vec::new());
            at(&cv, 0.0, 0.0, 1.0, |dc| p.paint(dc, &mut hits, &mut sliders, &remote, &layout)).expect("painted");
            p.hits = hits;
            let tree = p.tree();
            let node = tree.nodes.iter().find(|n| n.key == hit_key(h)).unwrap_or_else(|| panic!("{note:?}: the eject is in the tree"));
            assert_eq!(node.enabled, eject_ready(&note), "{note:?}");
            assert_eq!(p.hits.iter().any(|x| x.1 == h), eject_ready(&note), "{note:?}: input");
            assert!(node.name.contains("USB"), "{note:?}: {}", node.name);
            let said = removable_note(&row);
            if matches!(note, Note::Ejecting | Note::Safe) {
                assert!(!said.is_empty());
                assert_eq!(node.value.as_deref(), Some(said.as_str()), "{note:?}: its status");
            }
            // The device follows playback in reading order.
            let eject = tree.nodes.iter().position(|n| n.key == hit_key(h)).unwrap();
            let playback = tree.nodes.iter().position(|n| n.key == hit_key(Hit::Next)).unwrap();
            assert!(playback < eject, "{note:?}");
            p.workers.quit(&p.shared);
            let _ = DestroyWindow(p.hwnd);
        }
    }
    FAKE_ROWS.with(|f| *f.borrow_mut() = None);
}

/// The 2026-11-11 review (P2), on the panel itself: a failed device
/// switch takes the panel into the sound list; leaving it (back), the next
/// unrelated update leaves the panel on the grid; a new failure leads in
/// again.
#[test]
fn a_failed_switch_left_stays_left() {
    let _l = crate::i18n::test_lock("en");
    let stage = kit::stage();
    let (_, theme, light) = looks().into_iter().next().unwrap();
    FAKE_ROWS.with(|f| *f.borrow_mut() = None);
    unsafe {
        let mut p = made_panel(&stage, &theme, light, 1.0);
        p.sync();
        assert_eq!(p.open, None);
        let fail = |p: &Panel| {
            p.shared.set_quiet(|r| {
                r.audio_fail = Some((Flow::Output, "Headphones".into()));
                r.audio_fail_seq += 1;
            })
        };
        fail(&p);
        p.sync();
        assert_eq!(p.open, Some(Section::Sound), "the failure is said in the list");
        // Back (the button, Esc): the grid.
        p.set_section(None);
        // Unrelated updates (the apps' sessions), the failure still there.
        for _ in 0..3 {
            p.shared.set_quiet(|r| {
                if let Some(a) = r.apps.as_mut() {
                    a[0].level = a[0].level.wrapping_add(1) % 100;
                }
            });
            p.sync();
            assert_eq!(p.open, None, "left after it was shown");
        }
        // The line stays in the list when gone back into.
        assert_eq!(fail_for(&p.shared.snapshot(), Flow::Output), Some("Headphones"));
        // A new failure: in again.
        fail(&p);
        p.sync();
        assert_eq!(p.open, Some(Section::Sound));
        p.workers.quit(&p.shared);
        let _ = DestroyWindow(p.hwnd);
    }
}

/// 2026-11-11 (final/owner-volume-ring.png): the mute key put the ring
/// round the muted sound slider the pointer had last held. The panel as
/// `show` makes it: media, volume and letter keys leave the ring off, Tab
/// and the arrows show it, a press of the pointer hides it; a screen
/// reader's focus moves the focus and shows the ring only as it was, or
/// with Windows' keyboard cues always on (crate::input).
#[test]
fn the_ring_shows_only_after_a_key_that_moves_the_focus() {
    use crate::input::sample::{sequence, MUTE};
    let _l = crate::i18n::test_lock("ko");
    unsafe {
        let stage = kit::stage();
        let theme = kit::theme(false).windows_shell();
        let mut p = made_panel(&stage, &theme, false, 1.0);
        let hwnd = p.hwnd;
        // A point inside the panel, in its window's client space.
        let mut wr = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut wr);
        let inside = LPARAM((((p.rect.top + 6 - wr.top) as isize) << 16) | ((p.rect.left + 6 - wr.left) as isize & 0xFFFF));
        // As after the slider was dragged: the focus on it, no ring.
        p.focus = Some(Hit::Volume);
        p.volume = Some((40, true));
        p.keyboard = false;
        p.on_key(MUTE);
        assert!(!p.keyboard, "the owner's case: the mute key on the muted slider");
        sequence("control center", &mut p, VK_TAB.0, VK_DOWN.0, |p| p.keyboard, |p, vk| p.on_key(vk), |p| p.on_button(hwnd, inside, true, false));
        // The right button hides it as the left does.
        p.on_key(VK_UP.0);
        assert!(p.keyboard);
        p.on_button(hwnd, inside, true, true);
        assert!(!p.keyboard, "the left button");
        // A screen reader's SetFocus: the focus moves, the ring by the rule.
        p.focus = None;
        p.a11y_act(&hit_key(Hit::Volume), crate::a11y::Act::Focus);
        assert_eq!((p.focus, p.keyboard), (Some(Hit::Volume), crate::input::keyboard_cues()));
        p.keyboard = true;
        p.a11y_act(&hit_key(Hit::Project), crate::a11y::Act::Focus);
        assert_eq!((p.focus, p.keyboard), (Some(Hit::Project), true), "shown already: it stays");
        p.workers.quit(&p.shared);
        let _ = DestroyWindow(p.hwnd);
    }
}

#[test]
#[ignore = "Windows feel quick settings offscreen; idle workers"]
fn windows_feel_quick_settings() {
    let _l = crate::i18n::test_lock("ko");
    unsafe { scene("windows-quick-settings", |_| {}); }
}

fn wireless_fixtures(r: &mut Remote) {
    use super::super::panels::Wifi;
    r.bt = RadioView::On;
    r.bt_audio_ready = true;
    r.bt_devices = vec![
        BtDevice { key: 1, name: "무선 이어폰".into(), kind: BtKind::Audio, connected: true, battery: Some(79), can_switch: true },
        BtDevice { key: 2, name: "무선 마우스".into(), kind: BtKind::Mouse, connected: true, battery: Some(64), can_switch: false },
        BtDevice { key: 3, name: "휴대용 스피커".into(), kind: BtKind::Audio, connected: false, battery: None, can_switch: true },
        BtDevice { key: 4, name: "Xbox Wireless Controller".into(), kind: BtKind::Gamepad, connected: false, battery: None, can_switch: false },
    ];
    r.wifi_networks = Some(Ok([
        ("집 Wi-Fi", 90, true, true, true),
        ("NopBar Office", 65, true, false, true),
        ("Guest", 45, false, false, false),
    ].into_iter().map(|(name, signal, secured, connected, known)| Wifi {
        ssid: name.into(), signal, secured, connected, profile: known.then(|| name.into()), iface: Default::default(),
    }).collect()));
    r.access = [AccessView { on: Some(false), ..Default::default() }; 6];
    r.access[Access::ColorFilters as usize].on = Some(true);
    r.access[Access::StickyKeys as usize].on = Some(true);
    r.system = [SystemTile { state: SettingState { on: Some(false), writable: true, ..Default::default() }, ready: true, ..Default::default() }; 3];
    r.studio = Some("camera".into());
}

#[test]
fn a_redraw_refits_when_late_controls_grow_the_grid() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let (_, theme, light) = looks().into_iter().next().unwrap();
    unsafe {
        let mut p = made_panel(&stage, &theme, light, 1.5);
        p.shared.set_quiet(|r| { r.brightness = None; r.media = None; });
        p.resize(false);
        let before = p.height;
        p.shared.set_quiet(|r| { r.brightness = Some(Some(60)); r.media = Some(Media::default()); });
        p.redraw();
        let volume = p.hits.iter().find(|(_, hit)| *hit == Hit::Volume).unwrap().0;
        assert!(p.height > before && volume.bottom <= p.height, "volume at {volume:?}, viewport {}", p.height);
        p.workers.quit(&p.shared);
        let _ = DestroyWindow(p.hwnd);
    }
}

fn short_work_area(p: &mut Panel) {
    let gap = crate::placement::popup_gap(p.scale);
    p.work.bottom = p.button.bottom + gap + (220.0 * p.scale * MIN_SIZE).ceil() as i32;
}

fn long_bluetooth_list(p: &mut Panel) {
    short_work_area(p);
    p.shared.set_quiet(|r| {
        wireless_fixtures(r);
        r.bt_devices.extend((5..=12).map(|key| BtDevice {
            key,
            name: format!("Bluetooth {key}"),
            kind: BtKind::Keyboard,
            connected: false,
            battery: None,
            can_switch: false,
        }));
    });
    p.open = Some(Section::Bluetooth);
}

#[test]
fn small_viewports_scroll_grid_and_bluetooth_rows_into_view() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let (_, theme, light) = looks().into_iter().next().unwrap();
    unsafe {
        let mut p = made_panel(&stage, &theme, light, 1.5);
        short_work_area(&mut p);
        p.redraw();
        assert!(p.scroll_max > 0.0);
        assert_eq!(p.rect.bottom, p.work.bottom);
        let control = |p: &Panel, hit| p.hits.iter().find(|(_, h)| *h == hit).unwrap().0;
        let volume = control(&p, Hit::Volume);
        assert!(p.visible_hit(volume, Hit::Volume).is_none());
        p.focus = Some(Hit::Mute);
        p.on_key(VK_TAB.0);
        assert_eq!(p.focus, Some(Hit::Volume));
        let volume = control(&p, Hit::Volume);
        assert_eq!(p.visible_hit(volume, Hit::Volume), Some(volume));

        p.scroll = 0.0;
        long_bluetooth_list(&mut p);
        p.redraw();
        let first = Hit::BtDevice(1);
        let last = Hit::BtDevice(12);
        assert!(p.live(last));
        assert!(p.visible_hit(control(&p, last), last).is_none());
        assert!(p.tree().nodes.iter().find(|n| n.key == hit_key(last)).unwrap().hidden);
        let header = control(&p, Hit::Back);
        p.on_wheel(false);
        assert_eq!(control(&p, Hit::Back), header);
        let above = control(&p, first);
        assert!(above.top < p.list_body_top());
        assert_ne!(p.hit_at(above.left + 8.0, above.top + 8.0), Some(first));
        p.focus = Some(Hit::BtDevice(11));
        p.on_key(VK_TAB.0);
        assert_eq!(p.focus, Some(last));
        let shown = control(&p, last);
        assert_eq!(p.visible_hit(shown, last), Some(shown));
        p.on_key(VK_TAB.0);
        assert_eq!(p.focus, Some(Hit::BluetoothSettings));
        let settings = control(&p, Hit::BluetoothSettings);
        assert_eq!(p.visible_hit(settings, Hit::BluetoothSettings), Some(settings));
        p.on_wheel(false);
        assert_eq!(p.scroll, p.scroll_max);
        p.workers.quit(&p.shared);
        let _ = DestroyWindow(p.hwnd);
    }
}

#[test]
#[ignore = "renders fitted short viewports from in-memory fixtures"]
fn quick_settings_overflow_snapshots() {
    let _l = crate::i18n::test_lock("ko");
    unsafe {
        scene("quick-settings-grid-overflow-top", short_work_area);
        scene("quick-settings-grid-overflow-end", |p| {
            short_work_area(p);
            p.scroll = f32::MAX;
        });
        scene("quick-settings-bluetooth-overflow-top", long_bluetooth_list);
        scene("quick-settings-bluetooth-overflow-end", |p| {
            long_bluetooth_list(p);
            p.scroll = f32::MAX;
        });
    }
}

#[test]
#[ignore = "renders quick settings from in-memory fixtures"]
fn quick_settings_snapshots() {
    unsafe {
        {
            let _l = crate::i18n::test_lock("ko");
            let basic = |p: &mut Panel| {
                p.shared.set_quiet(|r| {
                    wireless_fixtures(r);
                    r.media = None;
                });
            };
            scene("quick-settings", basic);
            scene("quick-settings-no-camera", |p| {
                basic(p);
                p.shared.set_quiet(|r| r.studio = None);
            });
            FAKE_ROWS.with(|f| *f.borrow_mut() = Some(vec![usb()]));
            scene("quick-settings-media", |p| p.shared.set_quiet(wireless_fixtures));
            FAKE_ROWS.with(|f| *f.borrow_mut() = None);
            scene("quick-settings-airplane", |p| {
                basic(p);
                p.shared.set_quiet(|r| {
                    r.system[Setting::Airplane as usize].state.on = Some(true);
                    r.wifi = RadioView::Off;
                    r.bt = RadioView::Off;
                });
            });
            for (section, name) in [
                (Section::Bluetooth, "quick-settings-bluetooth"),
                (Section::Wifi, "quick-settings-wifi"),
                (Section::Accessibility, "quick-settings-accessibility"),
            ] {
                scene(name, |p| { basic(p); p.open = Some(section); });
            }
        }
        let _l = crate::i18n::test_lock("de");
        scene("quick-settings-de", |p| {
            p.shared.set_quiet(|r| {
                wireless_fixtures(r);
                r.media = None;
                r.ssid = Ssid::Some("Drahtlosnetzwerk Besprechungsraum".into());
                r.bt_devices[0].name = "Kabellose Kopfhörer im Konferenzraum".into();
            });
        });
    }
}

#[test]
fn split_tiles_publish_distinct_controls_and_restore_focus() {
    let _l = crate::i18n::test_lock("en");
    let stage = kit::stage();
    let (_, theme, light) = looks().into_iter().next().unwrap();
    unsafe {
        let mut p = made_panel(&stage, &theme, light, 1.0);
        p.shared.set_quiet(wireless_fixtures);
        p.redraw();
        assert_eq!(p.node(Hit::BtName, &p.shared.snapshot(), RECT::default()).value.as_deref(), Some("무선 이어폰 +1"));
        for (toggle, opener, section) in [
            (Hit::WifiToggle, Hit::WifiName, Section::Wifi),
            (Hit::BtToggle, Hit::BtName, Section::Bluetooth),
        ] {
            let order = p.order();
            let i = order.iter().position(|hit| *hit == toggle).unwrap();
            assert_eq!(order[i + 1], opener);
            let r = p.shared.snapshot();
            let node = |hit| p.node(hit, &r, RECT::default());
            assert!(node(toggle).toggle.is_some());
            assert!(node(opener).toggle.is_none());
            p.activate(opener);
            assert_eq!((p.open, p.focus), (Some(section), Some(Hit::Back)));
            p.on_key(VK_ESCAPE.0);
            assert_eq!((p.open, p.focus), (None, Some(opener)));
        }
        let r = p.shared.snapshot();
        let tile = p.tile_view(Hit::AccessList, &r);
        assert_eq!(tile.glyph, Glyph::Accessibility);
        assert!(tile.on && tile.disclosure);
        assert!(p.tile_view(Hit::Studio, &r).external);
        assert!(p.tile_view(Hit::Quick(Quick::EnergySaver), &r).external);
        for q in [Quick::Airplane, Quick::NightLight, Quick::Dnd] {
            assert!(!p.tile_view(Hit::Quick(q), &r).external);
        }
        let node = p.node(Hit::AccessList, &r, RECT::default());
        assert_eq!(node.role, Role::Button);
        assert!(node.invoke && node.toggle.is_none());
        assert!(!p.live(Hit::Access(Access::Magnifier)));
        p.activate(Hit::AccessList);
        assert_eq!((p.open, p.focus), (Some(Section::Accessibility), Some(Hit::Back)));
        p.on_key(VK_ESCAPE.0);
        assert_eq!((p.open, p.focus), (None, Some(Hit::AccessList)));
        p.workers.quit(&p.shared);
        let _ = DestroyWindow(p.hwnd);
    }
}

#[test]
fn network_text_cannot_disconnect_and_settings_wait_for_the_first_read() {
    let _l = crate::i18n::test_lock("en");
    let stage = kit::stage();
    let (_, theme, light) = looks().into_iter().next().unwrap();
    unsafe {
        let mut p = made_panel(&stage, &theme, light, 1.0);
        for setting in [Quick::Airplane, Quick::NightLight, Quick::Dnd] {
            p.activate(Hit::Quick(setting));
            assert!(p.done.is_none());
        }
        p.shared.set_quiet(wireless_fixtures);
        let missing = Remote { wifi: RadioView::Missing, ..Default::default() };
        let tile = p.tile_view(Hit::WifiToggle, &missing);
        assert!(tile.enabled);
        assert_eq!(tile.note, Some("control.radio.missing"));
        p.set_section(Some(Section::Wifi));
        let r = p.shared.snapshot();
        let key = wifi_key(&r.wifi_networks.as_ref().unwrap().as_ref().unwrap()[0]);
        let row = Hit::WifiRow(key);
        let disconnect = Hit::WifiDisconnect(key);
        assert!(!p.live(row));
        assert!(p.live(disconnect));
        assert_eq!(p.node(row, &r, RECT::default()).role, Role::Text);
        assert!(p.node(disconnect, &r, RECT::default()).invoke);
        let (tx, rx) = channel();
        p.workers.radio = tx;
        p.activate(row);
        assert!(rx.try_recv().is_err());
        p.activate(disconnect);
        assert!(matches!(rx.try_recv(), Ok(RadioCmd::WifiSwitch(k, false)) if k == key));
        p.set_section(None);
        p.shared.set_quiet(|r| {
            r.system[Setting::Dnd as usize].state = SettingState {
                on: Some(true), selected: Some(false), ..Default::default()
            };
        });
        p.activate(Hit::Quick(Quick::Dnd));
        assert_eq!(p.after, Some(After::Open("ms-settings:notifications")));
        p.workers.quit(&p.shared);
        let _ = DestroyWindow(p.hwnd);
    }
}
