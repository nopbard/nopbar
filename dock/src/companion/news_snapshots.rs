//! The production card and menu painters on synthetic backdrops.
use super::*;
use crate::{
    companion::{assistant, convenience, device_battery, downloads, remind, render, routine},
    desklayer::{backdrop, card as glass},
    gfx::promo_kit as kit,
    notes::inbox::Row,
    popup,
    theme::Theme,
};
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::RECT;

fn fixtures() -> Vec<(&'static str, Card)> {
    let now = Instant::now();
    let local = 22_000 * 86400 + 8 * 3600;
    let rows: Vec<_> = [
        "견적 검토",
        "회의 자료 공유",
        "메일 회신",
        "계약서 확인",
        "파일 정리",
        "일정 갱신",
        "요청 확인",
    ]
    .into_iter()
    .map(|text| Row::new(text.into(), local, None, 0, None))
    .collect();
    let morning = assistant::morning(
        assistant::Morning {
            calendar: Some(vec![(local + 5400, local + 9000, "제품 검토".into())]),
            notes: rows.clone(),
            batteries: vec![("마우스".into(), 20)],
        },
        local,
        0,
    )
    .unwrap();
    let wrap = routine::wrap(&rows, local).unwrap();
    let download = downloads::card(&[downloads::Download {
        path: PathBuf::from("Downloads/회의자료.pdf"),
        bytes: Some(2_400_000),
    }])
    .unwrap();
    let notice = Notice::single("fixture".into(), "회의 자료 보내기".into());
    let reminder = remind::card(&notice);
    let battery = device_battery::card("무선 키보드".into(), 18);
    let inserts = [
        (
            download.clone(),
            Receipt::Download(downloads::NoticeTime::present(now)),
        ),
        (reminder.clone(), Receipt::Reminder(notice)),
        (battery.clone(), Receipt::Battery("keyboard".into())),
        (wrap.clone(), Receipt::Wrap(local / 86400)),
        (
            download.clone(),
            Receipt::Download(downloads::NoticeTime::present(now)),
        ),
        (
            device_battery::card("마우스".into(), 20),
            Receipt::Battery("mouse".into()),
        ),
        (
            device_battery::card("이어폰".into(), 15),
            Receipt::Battery("earbuds".into()),
        ),
    ];
    let pending: Vec<_> = inserts
        .into_iter()
        .enumerate()
        .map(|(id, (card, receipt))| Pending {
            id: id as u64,
            card,
            receipt,
            deferred: true,
            during_absence: false,
            attempts: 0,
            retry: None,
        })
        .collect();
    let (bundle, shown) = batch(&pending.iter().collect::<Vec<_>>());
    let mut history = History::default();
    history.push(100, summary(&morning), local, true);
    for p in &pending {
        history.push(
            p.id,
            summary(&p.card),
            local + 60 * p.id as i64,
            shown.contains(&p.id),
        );
    }
    assert_eq!(history.unread(), 2);
    vec![
        ("morning", morning),
        ("wrap", wrap),
        ("download", download),
        ("reminder", reminder),
        ("battery", battery),
        ("bundle", bundle),
        ("news", history.card()),
    ]
}

fn scene(
    stage: &kit::Stage,
    theme: &Theme,
    light_wall: bool,
    dpi: f32,
    card: &Card,
    from_tray: bool,
    path: &Path,
) {
    let wall = kit::harmony_backdrop(light_wall);
    let canvas = kit::Canvas::of(stage.gfx.d2d_factory().clone(), 480., 480., dpi);
    let screen = RECT {
        left: 0,
        top: 0,
        right: canvas.w as i32,
        bottom: canvas.h as i32,
    };
    let mut work = screen;
    let (x, y, width) = if from_tray {
        work.bottom -= (40. * dpi) as i32;
        (444., 440., 24.)
    } else {
        (198., 12., 84.)
    };
    let wallpaper = crate::icons::Icon {
        width: 2,
        height: 2,
        pixels: [wall.b, wall.g, wall.r, wall.a].repeat(4),
    };
    backdrop::set_test(backdrop::grey_of(screen, wallpaper));
    let slot = SlotAnchor {
        rect: RECT {
            left: (x * dpi) as i32,
            top: (y * dpi) as i32,
            right: ((x + width) * dpi) as i32,
            bottom: ((y + 28.) * dpi) as i32,
        },
        dpi,
    };
    let card_at = card::place_slot(slot, work, card.height());
    if from_tray {
        assert_eq!(slot.rect.top - card_at.bottom, (8. * dpi) as i32);
        assert_eq!(card_at.right, work.right);
    } else {
        assert_eq!(card_at.top - slot.rect.bottom, (8. * dpi) as i32);
    }
    let under = backdrop::under(card_at).expect("fixture backdrop");
    assert_eq!(glass::theme_for(theme, Some(&under)).is_light, light_wall);
    let bar_under = backdrop::under(slot.rect).expect("fixture backdrop");
    unsafe {
        canvas.dc.Clear(Some(&kit::col(wall)));
        let at = |x, y| render::translate(x, y) * render::scale(dpi, dpi);
        canvas.dc.SetTransform(&at(x, y));
        glass::plate(&canvas.dc, theme, width, 28., false, Some(&bar_under)).unwrap();
        canvas.dc.SetTransform(&at(x + width / 2. - 10., y + 4.));
        let bar_theme = glass::theme_for(theme, Some(&bar_under));
        crate::topbar::paint_companion_home(
            &canvas.dc,
            20.,
            glass::ink(bar_theme, 1),
            card.kind == Kind::Bundle,
        )
        .unwrap();
        canvas
            .dc
            .SetTransform(&at(card_at.left as f32 / dpi, card_at.top as f32 / dpi));
        card::paint(&canvas.dc, &stage.gfx, card, theme, Some(&under)).unwrap();
    }
    backdrop::set_test(None);
    let (pixels, w, h) = canvas.finish();
    kit::save_png(&pixels, w, h, path);
}

fn menu(root: &Path, theme: &Theme, light_wall: bool, dpi: f32) {
    let mut rows = convenience::slot_items(super::super::Config::default());
    rows[0] = popup::Item::new(1, crate::i18n::t("companion.show"));
    rows.insert(0, convenience::news_item(2));
    assert_eq!(rows[0].label, "소식 2");
    let home = [popup::Item::new(0, crate::i18n::t("companion.title")).submenu(rows)];
    let shot = popup::harmony::Shot {
        sub: Some(crate::i18n::t("companion.title").into()),
        sub_hover: Some("소식 2".into()),
        ..Default::default()
    };
    let (pixels, w, h, _) = popup::harmony::read_menus(
        &theme.clone().windows_shell(),
        light_wall,
        dpi,
        &[(&home, shot)],
    );
    kit::save_png(
        &pixels.expect("offscreen menu"),
        w as u32,
        h as u32,
        &root.join("menu.png"),
    );
}

pub(crate) fn paint_site_news(stage: &kit::Stage, cv: &kit::Canvas, theme: &Theme, slot: SlotAnchor) -> RECT {
    let time = 22_000 * 86400 + 8 * 3600;
    let briefing = assistant::morning(
        assistant::Morning {
            calendar: Some(vec![
                (time + 5400, time + 9000, "Studio review".into()),
                (time + 14400, time + 16200, "Print check".into()),
            ]),
            notes: vec![Row::new("Send the draft".into(), time, None, 0, None)],
            batteries: vec![("Keyboard".into(), 20)],
        },
        time,
        0,
    ).unwrap();
    let work = RECT { left: 0, top: slot.rect.bottom, right: cv.w as i32, bottom: cv.h as i32 };
    let r = card::place_slot(slot, work, briefing.height());
    let under = backdrop::under(r).expect("briefing backdrop");
    let s = slot.dpi;
    unsafe {
        cv.dc.SetTransform(&(render::translate(r.left as f32 / s, r.top as f32 / s) * render::scale(s, s)));
        card::paint(&cv.dc, &stage.gfx, &briefing, theme, Some(&under)).unwrap();
    }
    r
}

#[test]
#[ignore = "fixture backdrops and offscreen production painters"]
fn gnome_a_render_matrix() {
    let _language = crate::i18n::test_lock("ko");
    let root = PathBuf::from(
        std::env::var_os("NOPBAR_GNOME_RENDER_DIR").expect("render output directory"),
    );
    let stage = kit::stage();
    let cards = fixtures();
    for (dpi, scale) in [(1., 100), (1.5, 150)] {
        for (light, ui) in [(true, "light"), (false, "dark")] {
            let theme = kit::theme(light);
            for (light_wall, wall) in [(true, "light"), (false, "dark")] {
                let root = root.join(format!("{scale}-{ui}-ui-{wall}-background"));
                for (name, card) in &cards {
                    scene(
                        &stage,
                        &theme,
                        light_wall,
                        dpi,
                        card,
                        false,
                        &root.join(format!("{name}.png")),
                    );
                    if *name == "battery" {
                        scene(
                            &stage,
                            &theme,
                            light_wall,
                            dpi,
                            card,
                            true,
                            &root.join("tray-battery.png"),
                        );
                    }
                }
                menu(&root, &theme, light_wall, dpi);
            }
        }
    }
}

#[test]
#[ignore = "fixture backdrops and offscreen production painters"]
fn gnome_b_render_matrix_news() {
    use crate::agents::{Agent, Completion, St, test_session};
    use crate::companion::agent_news::{result_card, wait_card};
    let _language = crate::i18n::test_lock("ko");
    let root = PathBuf::from(std::env::var_os("NOPBAR_GNOME_RENDER_DIR").expect("render output directory"));
    let now = Instant::now();
    let waiting = test_session(Agent::Codex, "project", St::Wait, now, false);
    let completion = |agent, name, minutes: u64, failed| {
        let mut session = test_session(agent, name, St::Done, now, false);
        session.failed = failed;
        result_card(&Completion { session, elapsed: Duration::from_secs(minutes * 60) })
    };
    let done = completion(Agent::Codex, "project", 12, false);
    let failed = completion(Agent::Claude, "sample", 4, true);
    let pending: Vec<_> = [
        done.clone(), failed.clone(),
        completion(Agent::Claude, "docs", 3, false),
        completion(Agent::Codex, "tests", 5, false),
        completion(Agent::Claude, "examples", 8, false),
        completion(Agent::Codex, "tools", 2, false),
        completion(Agent::Claude, "build", 11, false),
    ].into_iter().enumerate().map(|(id, card)| Pending {
        id: id as u64, card, receipt: Receipt::AgentResult(now),
        deferred: false, during_absence: false, attempts: 0, retry: None,
    }).collect();
    let bundle = batch(&pending.iter().collect::<Vec<_>>()).0;
    let cards = [("waiting", wait_card(&waiting)), ("done", done), ("failed", failed), ("bundle", bundle)];
    let stage = kit::stage();
    let mut rows = vec![convenience::news_item(2)];
    rows.extend(convenience::waiting_items(std::slice::from_ref(&waiting)));
    rows.extend(convenience::slot_items(super::super::Config::default()));
    let home = [popup::Item::new(0, crate::i18n::t("companion.title")).submenu(rows)];
    for (dpi, scale) in [(1., 100), (1.5, 150)] {
        for (light, ui) in [(true, "light"), (false, "dark")] {
            let theme = kit::theme(light);
            for (light_wall, wall) in [(true, "light"), (false, "dark")] {
                let dir = root.join(format!("{ui}-on-{wall}-{scale}"));
                std::fs::create_dir_all(&dir).unwrap();
                for (name, card) in &cards {
                    scene(&stage, &theme, light_wall, dpi, card, false, &dir.join(format!("{name}.png")));
                }
                let shot = popup::harmony::Shot {
                    sub: Some(crate::i18n::t("companion.title").into()),
                    sub_hover: Some("Codex · project · 입력 대기".into()),
                    ..Default::default()
                };
                let (pixels, w, h, _) = popup::harmony::read_menus(
                    &theme.clone().windows_shell(), light_wall, dpi, &[(&home, shot)],
                );
                kit::save_png(&pixels.expect("offscreen menu"), w as u32, h as u32, &dir.join("waiting-menu.png"));
            }
        }
    }
}
