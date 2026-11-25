//! Offscreen fixtures from the production menu, fan, card and bubble painters.
use super::{
    Config, Motion,
    anchors::Anchors,
    assistant, bubble, card,
    chat::{bubbles::Bubbles, paint},
    convenience, fan, grab, remind, render, routine,
};
use crate::{
    desklayer::{backdrop, card as glass},
    gfx::promo_kit as kit,
    notes::inbox::Row,
    popup,
    theme::Theme,
};
use std::path::Path;
use windows::Win32::Foundation::RECT;

const WORK: RECT = RECT {
    left: 0,
    top: 0,
    right: 540,
    bottom: 650,
};
const BODY: (f32, f32) = (270., 615.);

fn save(canvas: kit::Canvas, root: &Path, name: &str) {
    let (pixels, w, h) = canvas.finish();
    kit::save_png(&pixels, w, h, &root.join(format!("{name}.png")));
}

fn canvas(stage: &kit::Stage, wallpaper_light: bool) -> kit::Canvas {
    let canvas = kit::Canvas::of(
        stage.gfx.d2d_factory().clone(),
        WORK.right as f32,
        WORK.bottom as f32,
        1.,
    );
    unsafe {
        canvas
            .dc
            .Clear(Some(&kit::col(kit::harmony_backdrop(wallpaper_light))));
    }
    canvas
}

fn pose(frame: &str) -> super::motion::Pose {
    let mut pose = Motion::new(BODY.0).pose();
    pose.frame = frame.into();
    pose
}

fn hat(pose: &super::motion::Pose, size: f32) -> (f32, f32) {
    let (x, y) = Anchors::body_tip(pose);
    (BODY.0 + x * size, BODY.1 + y * size)
}

fn body(canvas: &kit::Canvas, pose: &super::motion::Pose, size: f32, system_light: bool) {
    unsafe {
        render::shadow(&canvas.dc, pose, BODY.0, BODY.1, size, system_light).unwrap();
        render::Painter::new()
            .paint(&canvas.dc, pose, BODY.0, BODY.1, size, false, 0., 1.)
            .unwrap();
    }
}

fn menus(root: &Path, system_light: bool, wallpaper_light: bool) {
    let theme = kit::theme(system_light).windows_shell();
    let mut rows = convenience::slot_items(Config::default());
    rows[0] = popup::Item::new(1, crate::i18n::t("companion.hide"));
    let home = [popup::Item::new(0, crate::i18n::t("companion.title")).submenu(rows.clone())];
    let shot = |sub| popup::harmony::Shot {
        sub: Some(crate::i18n::t(sub).into()),
        ..Default::default()
    };
    let (pixels, w, h, _) = popup::harmony::read_menus(
        &theme,
        wallpaper_light,
        1.,
        &[
            (&home, shot("companion.title")),
            (&rows, shot("companion.rest")),
        ],
    );
    kit::save_png(
        &pixels.expect("offscreen menu visual"),
        w as u32,
        h as u32,
        &root.join("menu.png"),
    );
}

fn fan_scene(stage: &kit::Stage, root: &Path, theme: &Theme, wallpaper_light: bool, size: f32) {
    let pose = pose("look_4");
    let canvas = canvas(stage, wallpaper_light);
    body(&canvas, &pose, size, theme.is_light);
    let (x, y) = hat(&pose, size);
    let tip = fan::Point { x, y };
    let shape = bubble::silhouette(&pose, BODY.0, BODY.1, size);
    let work = fan::Rect {
        left: 0.,
        top: 0.,
        right: WORK.right as f32,
        bottom: WORK.bottom as f32,
    };
    let layout = fan::place_tools(
        tip,
        &shape,
        size <= 0.4,
        work,
        &[],
        grab::Kind::Mouse,
        fan::Expanded::Rest,
        true,
        false,
    )
    .expect("offscreen fan layout");
    assert!(!layout.buttons.iter().any(|b| b.id == fan::Id::Detach));
    unsafe {
        bubble::paint(&canvas.dc, &layout, theme, None, 1., 1., None, false, 1.).unwrap();
    }
    save(canvas, root, "fan");
}

fn cards(stage: &kit::Stage, root: &Path, theme: &Theme, wallpaper_light: bool, size: f32) {
    let words = [
        "견적 검토",
        "회의 자료 공유",
        "메일 회신",
        "문서 확인",
        "파일 정리",
        "일정 갱신",
        "요청 확인",
    ];
    let mut notes: Vec<_> = words
        .into_iter()
        .map(|s| Row::new(s.into(), 0, None, 0, None))
        .collect();
    let wrap = routine::wrap(&notes, 18 * 3600).unwrap();
    notes[0].briefing_day = Some(1);
    let today = assistant::morning(
        assistant::Morning {
            calendar: Some(vec![(
                86400 + 9 * 3600,
                86400 + 10 * 3600,
                "제품 검토".into(),
            )]),
            notes,
            batteries: vec![("마우스".into(), 20)],
        },
        86400 + 8 * 3600,
        0,
    )
    .unwrap();
    for (name, card) in [("card-today", today), ("card-wrap", wrap)] {
        save(
            paint_card(stage, &card, theme, wallpaper_light, size),
            root,
            name,
        );
    }
}

fn paint_card(
    stage: &kit::Stage,
    card: &card::Card,
    theme: &Theme,
    wallpaper_light: bool,
    size: f32,
) -> kit::Canvas {
    let pose = pose("idle_0");
    let canvas = canvas(stage, wallpaper_light);
    body(&canvas, &pose, size, theme.is_light);
    let x = (WORK.right as f32 - card::WIDTH) / 2.;
    let y = (hat(&pose, size).1 - card.height() - 12.).max(16.);
    let target = RECT {
        left: x as i32,
        top: y as i32,
        right: (x + card::WIDTH) as i32,
        bottom: (y + card.height()) as i32,
    };
    let under = backdrop::under(target).expect("fixture backdrop");
    assert_eq!(
        glass::theme_for(theme, Some(&under)).is_light,
        wallpaper_light
    );
    unsafe {
        canvas.dc.SetTransform(&render::translate(x, y));
        card::paint(&canvas.dc, &stage.gfx, card, theme, Some(&under)).unwrap();
    }
    canvas
}

fn bubbles(stage: &kit::Stage, root: &Path, theme: &Theme, wallpaper_light: bool, size: f32) {
    let text = "오늘 일정 알려줘";
    let input = paint::Input {
        text: "",
        caret: 0,
        selection: (0, 0),
        composition: None,
    };
    let work = paint::rect(0., 0., WORK.right as f32, WORK.bottom as f32);
    let under = backdrop::under(WORK).expect("fixture backdrop");
    let ink = glass::theme_for(theme, Some(&under));
    assert_eq!(ink.is_light, wallpaper_light);
    for (name, frame) in [
        ("reminder", "wait_0"),
        ("chat", "idle_0"),
        ("thinking", "think_2"),
    ] {
        let pose = pose(frame);
        let tip = hat(&pose, size);
        let canvas = canvas(stage, wallpaper_light);
        let mut bubbles = Bubbles::default();
        if name == "reminder" {
            bubbles.say("회의 자료 공유".into(), 0.);
            bubbles.reminder = true;
        } else {
            bubbles.input(0.);
            bubbles.submit(text.into(), 0.);
            if name == "chat" {
                bubbles.delta("오전 9시에 제품 검토가 있어.", 0.);
                bubbles.finish(0.);
            }
        }
        let mut items =
            paint::arrange(&stage.gfx, &bubbles, &input, tip, work, 1., false, 1.).unwrap();
        if bubbles.reminder {
            remind::place(&mut items, work, None);
        }
        body(&canvas, &pose, size, theme.is_light);
        unsafe {
            for item in &items {
                paint::item(&canvas.dc, item, None, false, ink).unwrap();
            }
            if bubbles.reminder {
                remind::buttons(&canvas.dc, &stage.gfx, items[0].bounds, 1., ink).unwrap();
            }
            if name == "thinking" {
                let foot = Anchors::spinner_foot(&pose, BODY.0, BODY.1, size);
                paint::spinner(&canvas.dc, foot, size, 1., false, ink).unwrap();
            }
        }
        save(canvas, root, name);
    }
}

#[test]
#[ignore = "offscreen morning card with more than five carried items"]
fn gnome_fixes_review2_morning() {
    let _language = crate::i18n::test_lock("ko");
    let root = std::path::PathBuf::from(
        std::env::var_os("NOPBAR_GNOME_RENDER_DIR").expect("render output directory"),
    )
    .join("review2");
    let stage = kit::stage();
    let notes = [
        "견적 검토",
        "회의 자료 공유",
        "메일 회신",
        "문서 확인",
        "파일 정리",
        "일정 갱신",
        "요청 확인",
        "계약 검토",
    ]
    .into_iter()
    .map(|text| {
        let mut note = Row::new(text.into(), 0, None, 0, None);
        note.briefing_day = Some(2);
        note
    })
    .collect();
    let card = assistant::morning(
        assistant::Morning {
            notes,
            ..Default::default()
        },
        4 * 86400,
        0,
    )
    .unwrap();
    for (light, palette) in [(true, "light"), (false, "dark")] {
        let theme = kit::theme(light);
        for (wallpaper_light, wallpaper_name) in [(true, "light"), (false, "dark")] {
            let bg = kit::harmony_backdrop(wallpaper_light);
            let wallpaper = crate::icons::Icon {
                width: 2,
                height: 2,
                pixels: [bg.b, bg.g, bg.r, bg.a].repeat(4),
            };
            backdrop::set_test(backdrop::grey_of(WORK, wallpaper));
            let canvas = paint_card(&stage, &card, &theme, wallpaper_light, 0.28);
            save(
                canvas,
                &root,
                &format!("{palette}-ui-{wallpaper_name}-background-today"),
            );
        }
    }
    backdrop::set_test(None);
}

#[test]
#[ignore = "fixture backdrops, no screen capture, input or provider"]
fn gnome_fixes_render_matrix() {
    let _language = crate::i18n::test_lock("ko");
    let root = std::path::PathBuf::from(
        std::env::var_os("NOPBAR_GNOME_RENDER_DIR").expect("render output directory"),
    );
    let stage = kit::stage();
    for system_light in [true, false] {
        let theme = kit::theme(system_light);
        for wallpaper_light in [true, false] {
            let label = |light| if light { "light" } else { "dark" };
            let folder = root.join(format!(
                "{}-ui-{}-background",
                label(system_light),
                label(wallpaper_light)
            ));
            menus(&folder, system_light, wallpaper_light);
            super::chat::snapshots::gnome_fixes_history(
                &stage,
                &folder,
                system_light,
                wallpaper_light,
            );
            let bg = kit::harmony_backdrop(wallpaper_light);
            let wallpaper = crate::icons::Icon {
                width: 2,
                height: 2,
                pixels: [bg.b, bg.g, bg.r, bg.a].repeat(4),
            };
            backdrop::set_test(backdrop::grey_of(WORK, wallpaper));
            for (size, tag) in [(0.28, "size-28"), (0.56, "size-56")] {
                let folder = folder.join(tag);
                fan_scene(&stage, &folder, &theme, wallpaper_light, size);
                cards(&stage, &folder, &theme, wallpaper_light, size);
                bubbles(&stage, &folder, &theme, wallpaper_light, size);
            }
            backdrop::set_test(None);
        }
    }
}
