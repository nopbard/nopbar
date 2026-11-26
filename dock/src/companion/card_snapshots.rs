//! Shared-painter acceptance scenes; no screen capture or real wallpaper.
use super::*;
use crate::companion::render;
use crate::{desklayer::graphite_snapshots as widgets, gfx::promo_kit as kit};
use windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS;

#[test]
fn card_hover_keeps_remaining_time_and_dismissal_fades() {
    let now = Instant::now();
    let mut lifetime = CardLifetime::new(now);
    assert_eq!(lifetime.opacity(now + Duration::from_secs(15), true, true), 1.);
    assert_eq!(lifetime.opacity(now + Duration::from_secs(60), false, true), 1.);
    let expired = now + Duration::from_secs(65);
    assert_eq!(lifetime.opacity(expired, false, true), 1.);
    assert_eq!(lifetime.opacity(expired + FADE / 2, false, true), 0.5);
    assert_eq!(lifetime.opacity(expired + FADE, true, true), 0.);
    assert_eq!(lifetime.opacity(expired, false, false), 0.);
}

fn morning_fixture() -> Card {
    let mut rows = vec![
        Line::new(glass::Mark::Calendar, "일정", Route::Agenda(0)),
        Line::new(
            glass::Mark::Note,
            "메모 알림",
            Route::Notes("fixture".into()),
        ),
        Line::new(glass::Mark::Inbox, "인박스", Route::Inbox),
        Line::new(glass::Mark::Mouse, "마우스", Route::Close),
    ];
    rows[0].value = "3 · 09:30".into();
    rows[0].detail = "제품 검토".into();
    rows[1].value = "2 · 10:00".into();
    rows[2].value = "4".into();
    rows[3].value = "20%".into();
    Card {
        kind: Kind::Morning,
        title: "오늘".into(),
        emphasis: Some(Emphasis {
            value: "09:30".into(),
            title: "제품 검토".into(),
            route: Route::Agenda(0),
        }),
        lines: rows,
        buttons: vec![],
    }
}

#[test]
#[ignore = "offscreen companion cards over opposite wallpaper and system polarities"]
fn harmony_glass_c_companion_review() {
    let _lang = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let card = morning_fixture();
    let out = std::path::PathBuf::from(std::env::var_os("NOPBAR_GLASS_C_RENDER_DIR").unwrap());
    for (scale, tag) in [(1., 100), (1.5, 150)] {
        for (system_light, wall, name) in [(false, 220, "dark-system"), (true, 36, "light-system")] {
            let cv = kit::Canvas::of(stage.gfx.d2d_factory().clone(), WIDTH + 48., card.height() + 48., scale);
            let mon = RECT { left: 0, top: 0, right: cv.w as i32, bottom: cv.h as i32 };
            let wallpaper = crate::icons::Icon { width: 2, height: 2, pixels: [wall, wall, wall, 255].repeat(4) };
            let grey = backdrop::grey_of(mon, wallpaper).unwrap();
            backdrop::set_test(Some(grey));
            let origin = (24. * scale) as i32;
            let at = RECT { left: origin, top: origin, right: mon.right - origin, bottom: mon.bottom - origin };
            let theme = kit::theme(system_light);
            let under = backdrop::under(at);
            assert_eq!(glass::glass_for(&theme, under.as_ref()).unwrap().light, !system_light);
            unsafe {
                cv.dc.Clear(Some(&kit::col(crate::theme::Rgba { r: wall, g: wall, b: wall, a: 255 })));
                cv.dc.SetTransform(&(render::translate(24., 24.) * render::scale(scale, scale)));
                paint(&cv.dc, &stage.gfx, &card, &theme, under.as_ref()).unwrap();
            }
            backdrop::set_test(None);
            let (px, w, h) = cv.finish();
            kit::save_png(&px, w, h, &out.join(format!("review-companion-{name}-{tag}.png")));
        }
    }
}

#[test]
fn card_rows_and_buttons_route_only_inside_bounds() {
    let c = Card {
        kind: Kind::Wrap,
        title: "fixture".into(),
        emphasis: None,
        lines: vec![Line::new(glass::Mark::Inbox, "Inbox 2", Route::Inbox)],
        buttons: vec![Button {
            label: "Close".into(),
            route: Route::Close,
            primary: false,
        }],
    };
    assert!(matches!(c.hit(30., 55.), Some(Route::Inbox)));
    assert!(matches!(c.hit(30., c.height() - 30.), Some(Route::Close)));
    assert!(c.hit(-1., 55.).is_none());
    let c = morning_fixture();
    assert!(matches!(c.hit(30., TOP + 8.), Some(Route::Agenda(_))));
    assert!(matches!(
        c.hit(30., c.rows_top() + 40.),
        Some(Route::Agenda(_))
    ));
    assert!(matches!(
        c.hit(30., c.rows_top() + 60.),
        Some(Route::Notes(_))
    ));
}

#[test]
fn graphite_button_text_boxes_are_centered_within_one_pixel() {
    let stage = kit::stage();
    let mut max_error = 0f32;
    for dpi in [1., 1.25, 1.5, 2.] {
        for label in [
            "열기",
            "폴더",
            "내일로",
            "닫기",
            "Open",
            "Folder",
            "Tomorrow",
            "Close",
        ] {
            let r = rect(16., 20., 124., 32.);
            let layout = glass::pill_layout(&stage.gfx, label, r, 13.).unwrap();
            let mut m = DWRITE_TEXT_METRICS::default();
            unsafe {
                layout.GetMetrics(&mut m).unwrap();
            }
            let dx = (r.left + 12. + m.left + m.width / 2. - (r.left + r.right) / 2.).abs() * dpi;
            let dy = (r.top + m.top + m.height / 2. - (r.top + r.bottom) / 2.).abs() * dpi;
            max_error = max_error.max(dx).max(dy);
            assert!(dx <= 1. && dy <= 1., "{label} at {dpi}: {dx}, {dy}");
        }
    }
    eprintln!("button label maximum center error: {max_error:.4}px");
}

#[test]
fn card_placement_clamps_all_edges_and_ignores_small_breathing_shifts() {
    for dpi in [1., 1.5, 2.] {
        let work = RECT {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 1080,
        };
        for tip in [(-2000., -400.), (-10., -300.), (-2000., 1100.), (0., 1100.)] {
            let r = place(work, dpi, tip, 280., None);
            assert!(
                r.left >= work.left
                    && r.top >= work.top
                    && r.right <= work.right
                    && r.bottom <= work.bottom
            );
            assert_eq!(r.right - r.left, (WIDTH * dpi) as i32);
        }
        let tip = (-700., 400.);
        let r = place(work, dpi, tip, 280., None);
        assert_eq!(r, place(work, dpi, (tip.0 + 1., tip.1 + 1.), 280., Some(r)));
        assert_eq!(r, place(work, dpi, (tip.0 + 200., tip.1 - 100.), 280., Some(r)));
        let grown = place(work, dpi, (0., 0.), 300., Some(r));
        assert_eq!((grown.left, grown.top), (r.left, r.top));
    }
}

#[test]
#[ignore = "offscreen moving pet sequence; fixture wallpaper only"]
fn harmony_companion_fixed_panels() {
    use crate::companion::{chat::{bubbles::Bubbles,paint as chat},panels::ChatPlacement};
    let _lang=crate::i18n::test_lock("ko");
    let stage=kit::stage();
    let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/ui-snapshots/companion-panels");
    std::fs::create_dir_all(&root).unwrap();
    let mut geometry=String::from("file,card_x,card_y,chat_x,chat_y,bubble_x,bubble_y\n");
    let card=morning_fixture();
    for dpi in [1.,2.] {for size in [0.28,0.56] {for light in [true,false] {
        let work=RECT{left:0,top:0,right:(1100.*dpi) as i32,bottom:(760.*dpi) as i32};
        let mut previous=None;let mut pinned=ChatPlacement::default();
        let mut chat_origin=None;let mut bubble_origin=None;
        let mut long=Bubbles::default();long.say("첫 번째 줄\n두 번째 줄\n세 번째 줄\n대화 패널".into(),0.);
        let mut short=Bubbles::default();short.say("안녕!".into(),0.);
        let input=chat::Input{text:"",caret:0,selection:(0,0),composition:None};
        for (frame,art,x,y) in [(0,"idle_0",550.,710.),(1,"read_2",535.,705.),(2,"idle_0",685.,710.)] {
            let mut pose=crate::companion::motion::Motion::new(0.).pose();pose.frame=art.into();
            let tip=crate::companion::anchors::Anchors::body_tip(&pose);
            let tip=(x+tip.0*size,y+tip.1*size);
            // Three adjacent fixtures share the same pet displacement.
            let r=place(work,dpi,(tip.0-350.,tip.1),card.height(),previous);
            if let Some(p)=previous {assert_eq!(r,p);}previous=Some(r);
            let bounds=chat::rect(0.,0.,1100.,760.);
            let mut items=chat::arrange(&stage.gfx,&long,&input,tip,bounds,1.,false,dpi).unwrap();
            pinned.apply(&mut items,work,dpi,false,false);
            let origin=(items[0].bounds.left,items[0].bounds.top);
            if let Some(old)=chat_origin {assert_eq!(origin,old);}chat_origin=Some(origin);
            let small=chat::arrange(&stage.gfx,&short,&input,(tip.0+300.,tip.1),bounds,1.,false,dpi).unwrap();
            let small_origin=(small[0].bounds.left,small[0].bounds.top);
            if let Some(old)=bubble_origin {assert_ne!(small_origin,old);}bubble_origin=Some(small_origin);
            let cv=kit::Canvas::of(stage.gfx.d2d_factory().clone(),1100.,760.,dpi);
            let wall=widgets::wallpaper(!light,cv.w,cv.h,dpi);
            backdrop::set_test(backdrop::grey_of(work,widgets::mica(&wall)));
            unsafe {
                cv.dc.SetTransform(&windows_numerics::Matrix3x2::identity());
                cv.dc.DrawBitmap(&widgets::bitmap(&cv.dc,&wall),None,1.,windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,None,None);
                cv.dc.SetTransform(&(render::translate(r.left as f32/dpi,r.top as f32/dpi)*render::scale(dpi,dpi)));
                paint(&cv.dc,&stage.gfx,&card,&kit::theme(light),backdrop::under(r).as_ref()).unwrap();
                cv.dc.SetTransform(&render::scale(dpi,dpi));
                for item in items.iter().chain(&small) {chat::item(&cv.dc,item,None,false,&kit::theme(!light)).unwrap();}
                for dx in [-350.,0.,300.] {
                    render::shadow(&cv.dc,&pose,x+dx,y,size,light).unwrap();
                    render::Painter::new().paint(&cv.dc,&pose,x+dx,y,size,false,0.,dpi).unwrap();
                }
            }
            backdrop::set_test(None);
            let name=format!("{}-{}-{}-{frame}.png",if light{"light"}else{"dark"},(dpi*100.) as u32,(size*100.) as u32);
            geometry+=&format!("{name},{},{},{},{},{},{}\n",r.left,r.top,origin.0,origin.1,small_origin.0,small_origin.1);
            let(px,w,h)=cv.finish();kit::save_png(&px,w,h,&root.join(name));
        }
    }}}
    std::fs::write(root.join("geometry.csv"),geometry).unwrap();
}

#[test]
fn hidden_card_releases_its_wallpaper_listener() {
    let stage = kit::stage();
    assert!(!active());
    assert!(show_at(&stage.gfx, morning_fixture(), Anchor::Body { work: RECT::default(), dpi: 1., tip: (0., 0.) }));
    let h = VIEW.with(|v| v.borrow().as_ref().unwrap().hwnd);
    assert!(backdrop::listening(h));
    unsafe {
        proc(h, backdrop::WM_BACKDROP, WPARAM(0), LPARAM(0));
    }
    assert!(close());
    assert!(!backdrop::listening(h));
}

#[test]
#[ignore = "offscreen graphite cards beside widgets and companion, no desktop access"]
fn harmony_assistant_cards() {
    let _lang = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    snapshot(&stage.gfx, "morning", &morning_fixture());
    let mut long = morning_fixture();
    long.emphasis.as_mut().unwrap().title =
        "여러 팀이 함께 검토하는 아주 긴 일정 제목과 발표 준비".into();
    long.lines[0].detail = long.emphasis.as_ref().unwrap().title.clone();
    snapshot(&stage.gfx, "morning-long", &long);
}

pub(crate) fn snapshot(gfx: &crate::gfx::Gfx, name: &str, card: &Card) {
    for (look, light, night) in [
        ("day", false, false),
        ("night", false, true),
        ("light", true, false),
    ] {
        for dpi in [1., 1.5, 2.] {
            for size in [0.28, 0.56] {
                let cv = kit::Canvas::of(gfx.d2d_factory().clone(), 760., 560., dpi);
                let wall = widgets::wallpaper(night, cv.w, cv.h, dpi);
                let mon = RECT {
                    left: 0,
                    top: 0,
                    right: cv.w as i32,
                    bottom: cv.h as i32,
                };
                backdrop::set_test(backdrop::grey_of(mon, widgets::mica(&wall)));
                let theme = kit::theme(light);
                let pose = super::super::motion::Motion::new(0.).pose();
                let (x, y) = (552., 520.);
                let tip = super::super::anchors::Anchors::body_tip(&pose);
                let r = place(
                    mon,
                    dpi,
                    (x + tip.0 * size, y + tip.1 * size),
                    card.height(),
                    None,
                );
                unsafe {
                    cv.dc.SetTransform(&windows_numerics::Matrix3x2::identity());
                    let bmp = widgets::bitmap(&cv.dc, &wall);
                    cv.dc.DrawBitmap(&bmp,None,1.,windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR,None,None);
                    // A real widget beside the assistant, using the same wallpaper cut.
                    let wr = RECT {
                        left: (40. * dpi) as i32,
                        top: r.top,
                        right: (328. * dpi) as i32,
                        bottom: r.top + (176. * dpi) as i32,
                    };
                    cv.dc.SetTransform(
                        &(render::translate(40., r.top as f32 / dpi) * render::scale(dpi, dpi)),
                    );
                    glass::plate(
                        &cv.dc,
                        &theme,
                        288.,
                        176.,
                        false,
                        backdrop::under(wr).as_ref(),
                    )
                    .unwrap();
                    glass::note(&cv.dc, gfx, &theme, widgets::MEMO, "오후 2:10", 288., 176.)
                        .unwrap();
                    cv.dc.SetTransform(
                        &(render::translate(r.left as f32 / dpi, r.top as f32 / dpi)
                            * render::scale(dpi, dpi)),
                    );
                    paint(&cv.dc, gfx, card, &theme, backdrop::under(r).as_ref()).unwrap();
                    cv.dc.SetTransform(&render::scale(dpi, dpi));
                    render::shadow(&cv.dc, &pose, x, y, size, light).unwrap();
                    render::Painter::new()
                        .paint(&cv.dc, &pose, x, y, size, false, 0., dpi)
                        .unwrap();
                }
                backdrop::set_test(None);
                let (px, w, h) = cv.finish();
                kit::save_png(
                    &px,
                    w,
                    h,
                    &kit::harmony_dir().join(format!(
                        "assistant-graphite/{name}-{look}-{}-{}.png",
                        (dpi * 100.) as u32,
                        (size * 100.) as u32
                    )),
                );
            }
        }
    }
}

#[test]
fn review_7_assistant_card_registers_and_forgets_capture_privacy() {
    let stage = crate::gfx::promo_kit::stage();
    close();
    assert!(show_at(&stage.gfx, morning_fixture(), body_anchor()));
    let h = VIEW.with(|v| v.borrow().as_ref().unwrap().hwnd);
    let registered = crate::desklayer::privacy::registered(h);
    close();
    assert!(registered);
    assert!(!crate::desklayer::privacy::registered(h));
}

fn body_anchor() -> Anchor {
    Anchor::Body {
        work: RECT { left: 0, top: 0, right: 1000, bottom: 800 },
        dpi: 1.,
        tip: (500., 650.),
    }
}

#[test]
fn chat_interrupts_unshown_body_cards_and_only_replays_shown_briefings() {
    let stage = kit::stage();
    close();
    assert!(show_at(&stage.gfx, morning_fixture(), body_anchor()));
    assert!(interrupt_for_chat().is_none());
    assert!(!active());

    assert!(show_at(&stage.gfx, morning_fixture(), body_anchor()));
    VIEW.with(|v| v.borrow_mut().as_mut().unwrap().opened = Some(Instant::now()));
    assert_eq!(interrupt_for_chat().unwrap().card.kind, Kind::Morning);
    assert!(!active());
}

#[test]
fn slot_position_respects_gap_and_monitor_edges_at_each_scale() {
    for dpi in [1., 1.5] {
        let work = RECT { left: -1600, top: 20, right: 0, bottom: 1000 };
        let rect = RECT { left: -1050, top: 20, right: -1010, bottom: 60 };
        let anchor = SlotAnchor { rect, dpi };
        let r = place_slot(anchor, work, 120.);
        assert_eq!(r.top, rect.bottom + (8. * dpi).round() as i32);
        assert_eq!((r.left + r.right) / 2, (rect.left + rect.right) / 2);
        let r = place_slot(SlotAnchor { rect: RECT { left: -48, right: -8, ..rect }, dpi }, work, 120.);
        assert_eq!(r.right, work.right);
        let bottom = RECT { top: 1000, bottom: 1030, ..rect };
        let r = place_slot(SlotAnchor { rect: bottom, dpi }, work, 120.);
        assert_eq!(r.bottom, bottom.top - (8. * dpi).round() as i32);
    }
    assert!(entry_offset(Duration::ZERO) < 0.);
    assert!(entry_offset(ARRIVAL / 2) > 0.);
    assert_eq!(entry_offset(ARRIVAL), 0.);
}

#[test]
fn news_pages_preserve_the_original_row_routes() {
    let card = Card {
        kind: Kind::News, title: "News".into(), emphasis: None, buttons: vec![],
        lines: (0..20).map(|i| Line::new(glass::Mark::Note, i.to_string(), Route::Notes(i.to_string()))).collect(),
    };
    let first = viewport(&card, 0, 10);
    assert_eq!(first.lines.len(), 8);
    assert!(matches!(first.buttons[0].route, Route::Scroll(8)));
    let last = viewport(&card, 12, 10);
    assert!(matches!(last.hit(30., TOP + 8.), Some(Route::Notes(id)) if id == "12"));
    assert!(matches!(last.buttons[0].route, Route::Scroll(-8)));
}

#[test]
fn outside_dismissal_has_grace_and_does_not_imply_read() {
    let opened = u32::MAX - 500;
    assert!(!dismissal_ready(opened, opened - 1));
    assert!(!dismissal_ready(opened, opened.wrapping_add(1499)));
    assert!(dismissal_ready(opened, opened.wrapping_add(1500)));

    let now = Instant::now();
    let mut dismissed = CardLifetime::new(now);
    dismissed.dismiss(now + Duration::from_secs(2));
    dismissed.opacity(now + LIFETIME, true, false);
    assert!(!dismissed.engaged);

    let mut expired = CardLifetime::new(now);
    expired.opacity(now + LIFETIME, false, false);
    assert!(expired.engaged);

    let mut hovered = CardLifetime::new(now);
    hovered.opacity(now, true, false);
    assert!(hovered.engaged);
}

#[test]
fn precision_wheel_accumulates_and_keeps_signed_remainders() {
    let mut remainder = 0;
    assert_eq!(wheel_steps(&mut remainder, 75), 0);
    assert_eq!(wheel_steps(&mut remainder, -20), 0);
    assert_eq!(wheel_steps(&mut remainder, 70), 1);
    assert_eq!(remainder, 5);
    assert_eq!(wheel_steps(&mut remainder, -255), -2);
    assert_eq!(remainder, -10);
}
