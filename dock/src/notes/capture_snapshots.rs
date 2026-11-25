//! Actual runtime painters, hidden Composition visual readback, WARP raster.
use super::{capture, inbox, time, widgets};
use crate::{
    companion::{
        anchors::Anchors,
        chat::{bubbles::Bubbles, paint},
        motion::Motion,
        peek::{self, Edge},
        remind, render,
    },
    gfx::promo_kit as kit,
};
use windows::Win32::Foundation::*;
use windows_numerics::{Matrix3x2, Vector2};
unsafe extern "system" fn hidden(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(h, m, w, l)
}
#[test]
#[ignore = "offscreen capture/inbox/reminder acceptance PNGs; no screen capture or input"]
fn capture_remind_png() {
    let _lang = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let gfx = &stage.gfx;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ui-snapshots/capture-remind");
    std::fs::create_dir_all(&root).unwrap();
    let now = crate::search::dates::days(crate::search::dates::Ymd::new(2026, 11, 15)) * 86400
        + 14 * 3600
        + 50 * 60;
    for dpi in [1., 1.5] {
        for light in [false, true] {
            let theme = kit::theme(light);
            let mode = if light { "light" } else { "dark" };
            for state in ["empty", "time", "tab", "send", "popover"] {
                let stand = crate::glass::StandIn::new(kit::harmony_backdrop(light));
                let v = unsafe {
                    capture::View::new(
                        gfx.clone(),
                        &theme,
                        dpi,
                        RECT {
                            left: 0,
                            top: 0,
                            right: 1200,
                            bottom: 800,
                        },
                        Some(hidden),
                    )
                    .unwrap()
                };
                let canvas_width = if state == "send" { 900. } else { 656. };
                let bg = gfx.compositor.CreateSpriteVisual().unwrap();
                bg.SetSize(Vector2 {
                    X: canvas_width * dpi,
                    Y: 326. * dpi,
                })
                .unwrap();
                bg.SetBrush(
                    &gfx.compositor
                        .CreateColorBrushWithColor({
                            let c = kit::harmony_backdrop(light);
                            windows::UI::Color {
                                A: c.a,
                                R: c.r,
                                G: c.g,
                                B: c.b,
                            }
                        })
                        .unwrap(),
                )
                .unwrap();
                v._root.Children().unwrap().InsertAtBottom(&bg).unwrap();
                let s = match state {
                    "empty" => "",
                    "tab" => "로그 레벨 낮추기",
                    _ => "3시 회의 자료 출력",
                };
                let found = time::parse(s, now, "ko", Some(9));
                v.draw(
                    &theme,
                    &widgets::Capture {
                        text: s,
                        target: usize::from(state == "tab"),
                        tabs: state == "tab",
                        found: found.as_ref(),
                        now,
                        caret: state != "send",
                        caret_at: None,
                        selection: (0, 0),
                        popup: state == "popover",
                        lead: 10,
                        changing: false,
                    },
                )
                .unwrap();
                if state == "send" {
                    v.animate(0.8, 0.35, (120. * dpi, 12. * dpi)).unwrap();
                }
                let (w, h) = ((canvas_width * dpi) as i32, (326. * dpi) as i32);
                let pixels = unsafe { crate::popup::chrome_harmony::read_back(&v._root, w, h) }
                    .expect("hidden composition readback");
                kit::save_png(
                    &pixels,
                    w as u32,
                    h as u32,
                    &root.join(format!(
                        "capture-{state}-{mode}-{}.png",
                        (dpi * 100.) as u32
                    )),
                );
                drop(stand);
            }
            for state in ["empty", "three", "overflow", "folded", "hover"] {
                let mut rows = Vec::new();
                if state != "empty" {
                    for (i, s) in ["회의 자료 출력", "세금계산서 확인", "택배 반품"]
                        .iter()
                        .enumerate()
                    {
                        rows.push(inbox::Row::new(
                            (*s).into(),
                            now - i as i64 * 60,
                            if i == 0 { Some(now + 600) } else { None },
                            10,
                            None,
                        ));
                    }
                }
                if state == "folded" {
                    rows[2].complete(now);
                    for _ in 0..2 {
                        let mut r = inbox::Row::new("완료".into(), now - 90000, None, 0, None);
                        r.complete(now - 86401);
                        rows.push(r);
                    }
                }
                if state == "overflow" {
                    for n in 3..11 {
                        rows.push(inbox::Row::new(
                            format!("할 일 {}", n + 1),
                            now,
                            None,
                            0,
                            None,
                        ));
                    }
                }
                let (v, _, _) = inbox::visible(&rows, now, false, None, true);
                let data = widgets::Inbox {
                    rows: &rows,
                    remaining: rows.iter().filter(|r| r.completed.is_none()).count(),
                    visible: &v,
                    hover: (state == "hover").then_some(1),
                    flash: None,
                };
                let cv = kit::Canvas::at(324., widgets::inbox_height(&data) + 64., dpi);
                unsafe {
                    cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                    cv.dc.SetTransform(
                        &(Matrix3x2::translation(32., 24.) * render::scale(dpi, dpi)),
                    );
                    widgets::inbox(&cv.dc, gfx, &theme, &data).unwrap();
                }
                let (px, w, h) = cv.finish();
                kit::save_png(
                    &px,
                    w,
                    h,
                    &root.join(format!("inbox-{state}-{mode}-{}.png", (dpi * 100.) as u32)),
                );
            }
        }
    }
    for scale in [0.28, 0.56] {
        for light in [false, true] {
            for state in ["standing", "bottom", "side"] {
                for variant in ["single", "group", "say"] {
                    let cv = kit::Canvas::at(480., 400., 1.);
                    let frame = if state == "side" {
                        "sidepeek_0"
                    } else if state == "bottom" {
                        "notify_1"
                    } else {
                        "wait_0"
                    };
                    let edge = match state {
                        "bottom" => Some(Edge::Bottom),
                        "side" => Some(Edge::Right),
                        _ => None,
                    };
                    let (p, x, y) = if let Some(edge) = edge {
                        peek::pose(
                            edge,
                            RECT {
                                left: 0,
                                top: 0,
                                right: 480,
                                bottom: 400,
                            },
                            if state == "side" { 280. } else { 240. },
                            frame,
                            scale,
                            1.,
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
                    let text = match variant {
                        "group" => {
                            let mut queue = super::reminders::Queue::default();
                            for id in ["a", "b", "c"] {
                                queue.push(super::reminders::Notice::single(
                                    id.into(),
                                    "회의".into(),
                                ));
                            }
                            queue.next().unwrap().text
                        }
                        "say" => {
                            let mut queue = crate::companion::chat::say_queue::SayQueue::default();
                            b.say("알림".into(), 0.);
                            b.reminder = true;
                            queue.push("작업 완료".into());
                            assert!(queue.take(false).is_none());
                            b.cancel();
                            queue
                                .take(b.phase == crate::companion::chat::bubbles::Phase::Hidden)
                                .unwrap()
                        }
                        _ => "10분 뒤 회의 자료 출력".into(),
                    };
                    b.say(text, 0.);
                    b.reminder = variant != "say";
                    b.lifetime = 60.;
                    let input = paint::Input {
                        text: "",
                        caret: 0,
                        selection: (0, 0),
                        composition: None,
                    };
                    let work = paint::rect(0., 0., 480., 400.);
                    let mut items =
                        paint::arrange(gfx, &b, &input, tip, work, 1., true, 1.).unwrap();
                    paint::place_edge(&mut items, tip, work, edge);
                    if b.reminder {
                        remind::place(&mut items, work, edge);
                    }
                    unsafe {
                        cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                        render::Painter::new()
                            .paint(&cv.dc, &p, x, y, scale, false, 0., 1.)
                            .unwrap();
                        for item in &items {
                            paint::item(&cv.dc, item, None, false, &kit::theme(light)).unwrap();
                        }
                        if variant == "single" {
                            remind::buttons(&cv.dc, gfx, items[0].bounds, 1., &kit::theme(light)).unwrap();
                        }
                    }
                    let rs = remind::bounds(items[0].bounds);
                    if variant == "single" {
                        assert!(rs[0].left >= 0. && rs[1].right <= 480. && rs[1].bottom <= 400.);
                    }
                    let (px, w, h) = cv.finish();
                    kit::save_png(
                        &px,
                        w,
                        h,
                        &root.join(format!(
                            "{}-{state}-{}-{}.png",
                            if variant == "single" {
                                "reminder".into()
                            } else {
                                format!("review-{variant}")
                            },
                            if light { "light" } else { "dark" },
                            (scale * 100.) as u32
                        )),
                    );
                }
            }
        }
    }
}
