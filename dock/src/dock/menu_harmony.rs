//! The dock's menus drawn off screen for the design audit (tests only;
//! see crate::popup::harmony): the rows this file builds for a made-up
//! app, the dock's empty space, the Recycle Bin and NopBar's notification
//! icon, as a menu each.

use super::*;
use crate::popup::harmony::{nth_row, shoot, Shot};

/// A made-up app: pinned, starting with Windows, two windows open.
fn facts() -> AppFacts {
    let win = |h: isize, title: &str, active: bool| WinFact { hwnd: h, pid: 7, title: title.into(), active, minimized: false };
    AppFacts {
        path: "c:\\apps\\memo.lnk".into(),
        name: "메모장".into(),
        pinned: true,
        locatable: true,
        startup: true,
        windows: vec![win(1, "회의록.txt - 메모장", true), win(2, "장보기 목록.txt - 메모장", false)],
        ..Default::default()
    }
}

#[test]
#[ignore = "offscreen undo menu, no desktop capture or actions"]
fn native_undo_menu() {
    let _l = crate::i18n::test_lock("ko");
    let mut b = Builder::default();
    dock_menu(&mut b, &Config::default());
    b.sep(); b.add(t_("shortcuts.dock_undo"), DockAction::Undo);
    shoot("native-dock-undo", &b.rows, &Shot::hover(t_("shortcuts.dock_undo")));
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_dock_menus() {
    let _l = crate::i18n::test_lock("ko");
    let mut b = Builder::default();
    app_menu(&mut b, facts(), false);
    shoot("dock-menu-app", &b.rows, &Shot { hover: nth_row(&b.rows, 1), ..Default::default() });
    // Its options (unpin, start with Windows checked, show in Explorer).
    let options = Shot { sub: Some(t_("dock.menu.options").into()), sub_hover: Some(t_("dock.menu.startup").into()), ..Default::default() };
    shoot("dock-menu-app-options", &b.rows, &options);

    let mut b = Builder::default();
    dock_menu(&mut b, &Config::default());
    shoot("dock-menu-dock", &b.rows, &Shot { hover: nth_row(&b.rows, 1), ..Default::default() });
    let position = Shot { sub: Some(t_("dock.menu.position").into()), sub_hover: Some(t_("dock.menu.position.left").into()), ..Default::default() };
    shoot("dock-menu-dock-position", &b.rows, &position);

    let mut b = Builder::default();
    trash_menu(&mut b);
    shoot("dock-menu-trash", &b.rows, &Shot { hover: nth_row(&b.rows, 1), ..Default::default() });

    let (rows, _) = tray_menu(true);
    shoot("tray-menu", &rows, &Shot { hover: nth_row(&rows, 1), ..Default::default() });
}

/// The dock's menus for the font comparison (crate::font_compare): an
/// app with an English name and mixed window titles, and the dock's own.
pub(crate) fn font_compare() {
    let _l = crate::i18n::test_lock("ko");
    let win = |h: isize, title: &str, active: bool| WinFact { hwnd: h, pid: 7, title: title.into(), active, minimized: false };
    let facts = AppFacts {
        path: r"c:\apps\notion.lnk".into(),
        name: "Notion".into(),
        pinned: true,
        locatable: true,
        startup: true,
        windows: vec![win(1, "주간 회의록 - Notion", true), win(2, "Q4 Roadmap · 2026 계획 - Notion", false)],
        ..Default::default()
    };
    let mut b = Builder::default();
    app_menu(&mut b, facts, false);
    shoot("dock-menu-app", &b.rows, &Shot { hover: nth_row(&b.rows, 1), ..Default::default() });
    let mut b = Builder::default();
    dock_menu(&mut b, &Config::default());
    shoot("dock-menu-dock", &b.rows, &Shot { hover: nth_row(&b.rows, 1), ..Default::default() });
}

#[test]
#[ignore = "Windows feel app menu offscreen; synthetic app"]
fn windows_feel_menu() {
    let _l = crate::i18n::test_lock("ko");
    let mut f = facts();
    f.tasks = [("고정됨", "작업 문서"), ("최근", "회의록.txt"), ("", "새 문서")].into_iter().map(|(category, title)| crate::jumplist::Task { packaged_file: None, title: title.into(), category: category.into(), path: "sample.exe".into(), args: String::new(), dir: String::new() }).collect();
    let mut b = Builder::default();
    app_menu(&mut b, f.clone(), false);
    shoot("windows-app-menu", &b.rows, &Shot::hover("메모장"));

    let first = f.tasks[0].clone();
    for i in 1..20 {
        f.tasks.push(crate::jumplist::Task { title: format!("회의록 {i}.txt"), ..first.clone() });
    }
    for (limit, name) in [(2, "windows-jump-limit-two"), (0, "windows-jump-limit-zero")] {
        let mut b = Builder::default();
        f.destination_limit = Some(limit);
        app_menu(&mut b, f.clone(), false);
        shoot(name, &b.rows, &Shot::hover("메모장"));
    }
}
