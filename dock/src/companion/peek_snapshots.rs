//! Acceptance PNGs from the production pose/painter/chat layout, offscreen only.
use super::{
    anchors::Anchors,
    chat::{
        bubbles::Bubbles,
        paint::{self, Input},
    },
    peek::{self, Edge},
    render,
};
use crate::gfx::promo_kit as kit;
use windows::Win32::{
    Foundation::RECT,
    Graphics::Direct2D::{Common::*, D2D1_ANTIALIAS_MODE_ALIASED},
};
use windows_numerics::Vector2;

#[test]
#[ignore = "offscreen edge peek PNG matrix; no screen capture or input"]
fn harmony_edge_peek() {
    let _language = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let root = kit::harmony_dir().join("companion-peek");
    let mut geometry = String::from("file,tip_x,tip_y,visible_depth,bubble_inside\n");
    for s in [0.28, 0.56] {
        for dpi in [1., 1.5] {
            for light in [true, false] {
                let folder = root.join(format!(
                    "{}-size{}-dpi{}",
                    if light { "light" } else { "dark" },
                    (s * 100.) as u32,
                    (dpi * 100.) as u32
                ));
                let (w, h) = ((480. * dpi) as u32, (360. * dpi) as u32);
                let mut sheet = vec![0u8; (w * h * 12 * 4) as usize];
                for (row, edge) in [Edge::Bottom, Edge::Left, Edge::Right]
                    .into_iter()
                    .enumerate()
                {
                    for (col, state) in ["base", "look", "notify", "bubble"].into_iter().enumerate()
                    {
                        let cv = kit::Canvas::at(480., 360., dpi);
                        let r = RECT {
                            left: 0,
                            top: 0,
                            right: (480. * dpi) as i32,
                            bottom: (360. * dpi) as i32,
                        };
                        let frame = if edge != Edge::Bottom {
                            "sidepeek_0"
                        } else {
                            match state {
                                "look" => "peek_2",
                                "notify" => "notify_1",
                                _ => "peek_0",
                            }
                        };
                        let depth = if state == "notify" {
                            if edge == Edge::Bottom {
                                1.6
                            } else {
                                1.4
                            }
                        } else {
                            1.
                        };
                        let along = if edge == Edge::Bottom {
                            240. * dpi
                        } else {
                            270. * dpi
                        };
                        let (p, x, y) = peek::pose(edge, r, along, frame, s, dpi, depth, 0.);
                        assert_monitor_clip(&p, x, y, s, dpi, r);
                        let a = Anchors::load(frame).posed(&p, 1.);
                        let tip = (x + a.tip.0 * s, y + a.tip.1 * s);
                        let mut b = Bubbles::default();
                        if state == "bubble" {
                            b.input(0.);
                        }
                        let input = Input {
                            text: "오늘 뭐 할까",
                            caret: 0,
                            selection: (0, 0),
                            composition: None,
                        };
                        let work = paint::rect(0., 24., 480., 288.);
                        let mut items =
                            paint::arrange(&stage.gfx, &b, &input, tip, work, 1., true, dpi)
                                .unwrap();
                        paint::place_edge(&mut items, tip, work, Some(edge));
                        for it in &items {
                            assert!(
                                it.bounds.left >= 0.
                                    && it.bounds.top >= 24.
                                    && it.bounds.right <= 480.
                                    && it.bounds.bottom <= 312.
                            );
                        }
                        unsafe {
                            cv.dc.Clear(Some(&kit::col(kit::harmony_backdrop(light))));
                            cv.dc.PushAxisAlignedClip(
                                &paint::rect(0., 0., 480., 360.),
                                D2D1_ANTIALIAS_MODE_ALIASED,
                            );
                            render::Painter::new()
                                .paint(&cv.dc, &p, x, y, s, false, 0., dpi)
                                .unwrap();
                            if state == "bubble" {
                                let foot = paint::spinner_foot((x, y), tip, s, work, Some(edge));
                                let (center, r, d) = super::chat::bubbles::spinner(foot, s);
                                assert!(
                                    center.0 - r - d >= work.left
                                        && center.0 + r + d <= work.right
                                        && center.1 - r - d >= work.top
                                        && center.1 + r + d <= work.bottom
                                );
                            }
                            for it in &items {
                                paint::item(&cv.dc, it, Some(&input), true, &kit::theme(light)).unwrap();
                            }
                            cv.dc.PopAxisAlignedClip();
                        }
                        let name = format!("{edge:?}-{state}.png");
                        let (px, pw, ph) = cv.finish();
                        assert_eq!((pw, ph), (w, h));
                        kit::save_png(&px, w, h, &folder.join(&name));
                        for line in 0..h as usize {
                            let src = line * w as usize * 4;
                            let dst =
                                ((row * h as usize + line) * w as usize * 4 + col * w as usize) * 4;
                            sheet[dst..dst + w as usize * 4]
                                .copy_from_slice(&px[src..src + w as usize * 4]);
                        }
                        geometry += &format!(
                            "{}/{name},{},{},{},true\n",
                            folder.file_name().unwrap().to_string_lossy(),
                            tip.0,
                            tip.1,
                            if edge == Edge::Bottom {
                                peek::cut(frame) - Anchors::load(frame).tip.1
                            } else {
                                super::clips::catalog().frames[frame].w as f32
                                    - Anchors::load(frame).edge.unwrap()
                            }
                        );
                    }
                }
                kit::save_png(&sheet, w * 4, h * 3, &folder.join("contact.png"));
            }
        }
    }
    std::fs::write(root.join("geometry.csv"), geometry).unwrap();
    monitor_map(&root);
}
/// Render on a larger transparent surface so outside pixels really exist.
/// Exercise the same CPU clip as the native window (not a canvas-boundary clip).
fn assert_monitor_clip(p: &super::motion::Pose, x: f32, y: f32, s: f32, dpi: f32, r: RECT) {
    let margin = 192.;
    let cv = kit::Canvas::at(480. + margin * 2., 360. + margin * 2., dpi);
    unsafe {
        cv.dc.Clear(Some(&D2D1_COLOR_F {
            r: 0.,
            g: 0.,
            b: 0.,
            a: 0.,
        }));
        render::Painter::new()
            .paint(&cv.dc, p, x + margin, y + margin, s, false, 0., dpi)
            .unwrap();
    }
    let (mut pixels, w, _) = cv.finish();
    let original = pixels.clone();
    let origin = (-(margin * dpi) as i32, -(margin * dpi) as i32);
    peek::clip_pixels(&mut pixels, w, origin, r);
    let mut inside_ink = 0;
    for (i, (after, before)) in pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(original.as_chunks::<4>().0.iter())
        .enumerate()
    {
        let (px, py) = (
            origin.0 + (i % w as usize) as i32,
            origin.1 + (i / w as usize) as i32,
        );
        if px >= r.left && px < r.right && py >= r.top && py < r.bottom {
            assert_eq!(after, before, "visible artwork changed by clip");
            inside_ink += usize::from(after[3] > 0);
        } else {
            assert_eq!(*after, [0, 0, 0, 0], "pixel outside monitor at {px},{py}");
        }
    }
    assert!(inside_ink > 0, "empty snapshot");
}
fn monitor_map(root: &std::path::Path) {
    let cv = kit::Canvas::at(1200., 500., 1.);
    unsafe {
        cv.dc.Clear(Some(&D2D1_COLOR_F {
            r: 0.94,
            g: 0.94,
            b: 0.94,
            a: 1.,
        }));
        let grey = cv
            .dc
            .CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.65,
                    g: 0.65,
                    b: 0.65,
                    a: 1.,
                },
                None,
            )
            .unwrap();
        let ink = cv
            .dc
            .CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.08,
                    g: 0.08,
                    b: 0.08,
                    a: 1.,
                },
                None,
            )
            .unwrap();
        for (i, layout) in [
            vec![(0, 0), (1, 0)],
            vec![(0, 0), (0, 1)],
            vec![(0, 0), (1, 0), (0, 1)],
        ]
        .into_iter()
        .enumerate()
        {
            let screens: Vec<_> = layout
                .iter()
                .enumerate()
                .map(|(n, (x, y))| {
                    (
                        n as u64,
                        RECT {
                            left: x * 1000,
                            top: y * 800,
                            right: (x + 1) * 1000,
                            bottom: (y + 1) * 800,
                        },
                    )
                })
                .collect();
            let ox = 30. + i as f32 * 400.;
            let oy = 80.;
            for (_, r) in &screens {
                let tr = |x: f32, y: f32| Vector2 {
                    X: ox + x * 0.16,
                    Y: oy + y * 0.16,
                };
                cv.dc.DrawRectangle(
                    &kit::rect(
                        ox + r.left as f32 * 0.16,
                        oy + r.top as f32 * 0.16,
                        ox + r.right as f32 * 0.16,
                        oy + r.bottom as f32 * 0.16,
                    ),
                    &grey,
                    1.,
                    None,
                );
                for edge in [Edge::Bottom, Edge::Left, Edge::Right] {
                    let (lo, hi) = edge.span(*r);
                    for j in 0..20 {
                        let a = lo + (hi - lo) * j as f32 / 20.;
                        let b = lo + (hi - lo) * (j + 1) as f32 / 20.;
                        if peek::exposed(edge, *r, a, b, &screens) {
                            let (from, to) = match edge {
                                Edge::Bottom => (tr(a, r.bottom as f32), tr(b, r.bottom as f32)),
                                Edge::Left => (tr(r.left as f32, a), tr(r.left as f32, b)),
                                Edge::Right => (tr(r.right as f32, a), tr(r.right as f32, b)),
                            };
                            cv.dc.DrawLine(from, to, &ink, 5., None);
                        }
                    }
                }
            }
        }
    }
    let (px, w, h) = cv.finish();
    kit::save_png(&px, w, h, &root.join("monitor-edges.png"));
}
