//! Offscreen acceptance uses the live Composition chrome and content painter.
use super::{
    model::{Desktop, Kind, Model, Size, Update},
    view::View,
};
use crate::gfx::promo_kit as kit;
use windows::{
    Win32::{Foundation::*, UI::WindowsAndMessaging::DefWindowProcW},
    UI::Color,
};
use windows_numerics::Vector2;
unsafe extern "system" fn hidden(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}
#[test]
#[ignore = "offscreen battery acceptance PNGs; no device enumeration or owner window"]
fn device_battery_matrix() {
    let _lang = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ui-snapshots/device-battery");
    std::fs::create_dir_all(&root).unwrap();
    for dpi in [1., 1.5] {
        for light in [false, true] {
            for state in ["small", "medium", "empty", "off", "charging", "low", "recent", "recent-small"] {
                // recent: the keyboard and the mouse dropped off an hour ago
                // (seen within a day): dimmed, after the connected ones.
                let recent = state.starts_with("recent");
                let at = if recent { super::model::clock() - 7200 } else { 0 };
                let theme = kit::theme(light);
                let mut model = Model::default();
                if state != "empty" {
                    for (i, (name, p, kind)) in [
                        ("이어폰", 18, Kind::Headphones),
                        ("마우스", 64, Kind::Mouse),
                        ("키보드", 91, Kind::Keyboard),
                        ("패드", 50, Kind::Gamepad),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        model.update(
                            Update {
                                id: i.to_string(),
                                name: Some(name.into()),
                                percent: Some(Some(if state == "low" && i == 1 { 19 } else { p })),
                                kind: Some(kind),
                                connected: Some(i != 3 && !(state == "off" && i == 0)),
                                charging: Some((state == "charging" && i == 0) || i == 2),
                            },
                            at,
                        );
                    }
                    if recent {
                        for id in ["1", "2"] {
                            model.update(Update { id: id.into(), connected: Some(false), ..Default::default() }, at + 3600);
                        }
                    }
                }
                let desktop = Desktop {
                    size: if matches!(state, "small" | "empty" | "recent-small") {
                        Size::SMALL
                    } else {
                        Size::MEDIUM
                    },
                    ..Default::default()
                };
                let backdrop = kit::harmony_backdrop(light);
                let stand = crate::glass::StandIn::new(backdrop);
                let view = unsafe {
                    View::new(stage.gfx.clone(), &theme, desktop.size, dpi, Some(hidden)).unwrap()
                };
                let m = 0;
                let (pw, ph) = (view.glass.card.0 + 2 * m, view.glass.card.1 + 2 * m);
                let bg = stage.gfx.compositor.CreateSpriteVisual().unwrap();
                bg.SetSize(Vector2 {
                    X: pw as f32,
                    Y: ph as f32,
                })
                .unwrap();
                bg.SetBrush(
                    &stage
                        .gfx
                        .compositor
                        .CreateColorBrushWithColor(Color {
                            A: 255,
                            R: backdrop.r,
                            G: backdrop.g,
                            B: backdrop.b,
                        })
                        .unwrap(),
                )
                .unwrap();
                view.glass.root.Children().unwrap().InsertAtBottom(&bg).unwrap();
                view.draw(&theme, &desktop, &model.rows(&desktop)).unwrap();
                let pixels =
                    unsafe { crate::popup::chrome_harmony::read_back(&view.glass.root, pw, ph) }
                        .unwrap();
                kit::save_png(
                    &pixels,
                    pw as u32,
                    ph as u32,
                    &root.join(format!(
                        "{state}-{}-{}.png",
                        if light { "light" } else { "dark" },
                        (dpi * 100.) as u32
                    )),
                );
                drop(stand);
            }
        }
    }
    println!("battery PNGs: {}", root.display());
}

#[test]
#[ignore = "offscreen local say bubble, standing and bottom peek"]
fn device_battery_companion() {
    use crate::companion::{
        anchors::Anchors,
        chat::{bubbles::Bubbles, paint},
        motion::Motion,
        peek::{self, Edge},
        render,
    };
    let _lang = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ui-snapshots/device-battery/companion");
    for scale in [0.28, 0.56] {
        for dpi in [1., 1.5] {
            for light in [false, true] {
                for bottom in [false, true] {
                    let cv = kit::Canvas::at(480., 400., dpi);
                    let frame = if bottom { "notify_1" } else { "idle_0" };
                    let (p, x, y) = if bottom {
                        peek::pose(
                            Edge::Bottom,
                            RECT {
                                left: 0,
                                top: 0,
                                right: (480. * dpi) as i32,
                                bottom: (400. * dpi) as i32,
                            },
                            240. * dpi,
                            frame,
                            scale,
                            dpi,
                            1.6,
                            0.,
                        )
                    } else {
                        let mut p = Motion::new(240.).pose();
                        p.frame = frame.into();
                        (p, 240., 380.)
                    };
                    let a = Anchors::load(frame).posed(&p, 1.);
                    let tip = (x + a.tip.0 * scale, y + a.tip.1 * scale);
                    let mut b = Bubbles::default();
                    b.say("이어폰 18%".into(), 0.);
                    let input = paint::Input {
                        text: "",
                        caret: 0,
                        selection: (0, 0),
                        composition: None,
                    };
                    let work = paint::rect(0., 0., 480., 400.);
                    let mut items =
                        paint::arrange(&stage.gfx, &b, &input, tip, work, 1., false, dpi).unwrap();
                    paint::place_edge(&mut items, tip, work, bottom.then_some(Edge::Bottom));
                    assert!(items.iter().all(|i| i.bounds.left >= 0.
                        && i.bounds.top >= 0.
                        && i.bounds.right <= 480.
                        && i.bounds.bottom <= 400.));
                    unsafe {
                        cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                        render::Painter::new()
                            .paint(&cv.dc, &p, x, y, scale, false, 0., dpi)
                            .unwrap();
                        for i in &items {
                            paint::item(&cv.dc, i, None, false, &kit::theme(light)).unwrap();
                        }
                    }
                    let (px, w, h) = cv.finish();
                    kit::save_png(
                        &px,
                        w,
                        h,
                        &root.join(format!(
                            "{}-{}-{}-{}.png",
                            if bottom { "peek" } else { "standing" },
                            if light { "light" } else { "dark" },
                            (scale * 100.) as u32,
                            (dpi * 100.) as u32
                        )),
                    );
                }
            }
        }
    }
}
