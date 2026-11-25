//! The top bar's panels drawn off screen for the design audit (tests
//! only; see crate::popup::harmony): the rows this file builds, as a menu
//! each. Nothing is read from this PC: the volume is given, the recent
//! items are made up, and the calendar is put together here from the
//! same pieces `build_measured` uses (the clock row, the month header,
//! month_grid, agenda rows, timer_menu), for 9 November 2026 with
//! made-up events (`build` reads the clock and the user's calendars).

use windows::Win32::Foundation::SYSTEMTIME;

use super::*;
use crate::popup::harmony::{nth_row, shoot, Shot};

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_topbar_volume_and_bar_menu() {
    let _l = crate::i18n::test_lock("ko");
    let b = build(Panel::Volume, Some((40, false)), 800.0);
    shoot("topbar-volume", &b.items, &Shot::hover(t("topbar.panel.volume.mute")));
    // The states inventory (final/states-inventory.md): 0, 50 and 100 %,
    // muted, the keyboard on the slider.
    for (v, muted) in [(0u8, false), (50, false), (100, false), (40, true)] {
        let b = build(Panel::Volume, Some((v, muted)), 800.0);
        let name = if muted { "topbar-volume-muted".to_string() } else { format!("topbar-volume-{v}") };
        shoot(&name, &b.items, &Shot::default());
    }
    let b = build(Panel::Volume, Some((60, false)), 800.0);
    shoot("topbar-volume-key", &b.items, &Shot { key: nth_row(&b.items, 0), ..Default::default() });
    let mut b = Built::default();
    bar_menu(&mut b, &[], super::super::shutdown::Pending::None, &|_| String::new());
    shoot("topbar-bar-menu", &b.items, &Shot { hover: nth_row(&b.items, 0), ..Default::default() });
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_topbar_system_menu() {
    let _l = crate::i18n::test_lock("ko");
    let recent: Vec<_> = ["분기 보고서.docx", "예산안 2026.xlsx", "로고 시안.png", "회의록.txt", "여행 사진"]
        .into_iter()
        .map(|n| crate::topbar::system::Recent { name: n.into(), path: format!("Z:\\harmony\\{n}") })
        .collect();
    let mut b = Built::default();
    system_menu(&mut b, &recent, crate::topbar::shutdown::Pending::None, &|_| String::new());
    shoot("topbar-system-menu", &b.items, &Shot { hover: nth_row(&b.items, 1), ..Default::default() });
    let open = Shot { sub: Some(t("topbar.system.recent").into()), sub_hover: Some("예산안 2026.xlsx".into()), ..Default::default() };
    shoot("topbar-system-menu-recent", &b.items, &open);
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_topbar_calendar() {
    let _l = crate::i18n::test_lock("ko");
    let (y, m, today, picked) = (2026, 11u32, 9u32, 12u32);
    let day = |y: i32, m: u32, d: u16| SYSTEMTIME { wYear: y as u16, wMonth: m as u16, wDay: d, ..Default::default() };
    let mut b = Built::default();
    b.items.push(Item::clock());
    b.sep();
    let (py, pm) = prev_month(y, m);
    let (ny, nm) = next_month(y, m);
    let nav = crate::popup::MonthNav {
        prev: b.push_action(Action::Month(py, pm)),
        next: b.push_action(Action::Month(ny, nm)),
        today: 0,
        prev_name: t("topbar.panel.calendar.previous").into(),
        next_name: t("topbar.panel.calendar.next").into(),
        today_name: t("topbar.panel.calendar.today").into(),
    };
    b.items.push(Item::month(crate::i18n::date::year_month(&day(y, m, 1)), nav));
    let wd: Vec<String> = (1..=7).map(|d| crate::i18n::date::weekday(&day(2026, 11, d))).collect();
    let mut head = Item::grid(wd, None);
    head.checked = true;
    head.abbrev = true;
    b.items.push(head);
    for week in month_grid(y, m) {
        let mark = week.iter().position(|&d| d == today);
        let mut row = Item::grid(week.iter().map(|&d| if d == 0 { String::new() } else { d.to_string() }).collect(), mark);
        row.ring = week.iter().position(|&d| d == picked);
        row.cell_ids = week.iter().map(|&d| if d == 0 { 0 } else { b.push_action(Action::Day(y, m, d)) }).collect();
        b.items.push(row);
    }
    b.sep();
    // The agenda (agenda_section's rows): one over, one now, one to come.
    for (time, title, place, now, past) in [("09:00", "아침 점검", "", false, true), ("10:00", "주간 회의", "회의실 2", true, false), ("15:30", "디자인 리뷰", "온라인", false, false)] {
        let id = b.push_action(Action::Notifications);
        b.items.push(Item::event(id, crate::popup::EventRow { time: time.into(), title: title.into(), place: place.into(), now, past }));
    }
    b.sep();
    timer_menu(&mut b, TimerMenu { idle: true, running: false, focus: true, today: 2 }, [(25, 5), (50, 10), (40, 8)]);
    b.sep();
    b.add(t("topbar.panel.calendar.notifications"), Action::Notifications).shortcut = "Win+N".into();
    b.add(t("topbar.panel.calendar.settings"), Action::Open("ms-settings:dateandtime"));
    let clock = SYSTEMTIME { wYear: 2026, wMonth: 11, wDayOfWeek: 1, wDay: 9, wHour: 9, wMinute: 41, wSecond: 0, wMilliseconds: 0 };
    shoot("topbar-calendar", &b.items, &Shot { hover: Some(t("topbar.panel.calendar.notifications").into()), clock: Some(clock), ..Default::default() });
    // The pointer on a day (a round fill on its circle) and the button
    // down on the next month's.
    let week = b.items.iter().position(|it| it.cells.iter().any(|c| c == "18")).unwrap();
    let month = b.items.iter().position(|it| it.nav.is_some()).unwrap();
    crate::popup::harmony::compose(
        "topbar-calendar-states",
        "flat",
        1.5,
        &[
            (&b.items, Shot { point: Some(crate::popup::parts::Target::Cell(week, 3)), clock: Some(clock), ..Default::default() }),
            (&b.items, Shot { point: Some(crate::popup::parts::Target::Nav(month, crate::popup::NavPart::Next)), press: true, clock: Some(clock), ..Default::default() }),
        ],
    );
    let timer = Shot { sub: Some(t("timer.menu").into()), clock: Some(clock), ..Default::default() };
    shoot("topbar-calendar-timer", &b.items, &timer);
}

/// The network and battery panels, for a made-up Wi-Fi list and made-up
/// power readings (network_menu, battery_menu: `build` asks Windows).
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_network() {
    let _l = crate::i18n::test_lock("ko");
    let net = |ssid: &str, signal: u32, secured: bool, connected: bool, known: bool| Wifi {
        ssid: ssid.into(),
        signal,
        secured,
        connected,
        profile: known.then(|| ssid.to_string()),
        iface: windows::core::GUID::zeroed(),
    };
    let nets = vec![net("Home Wi-Fi", 88, true, true, true), net("Studio 5G", 61, true, false, true), net("Cafe Guest", 47, false, false, false), net("Printer Setup", 18, false, false, false)];
    let mut b = Built::default();
    network_menu(&mut b, Ok(nets));
    shoot("network", &b.items, &Shot::hover("Studio 5G"));
    // The list did not come: said so with a button that asks again,
    // Windows' page still there. Before that: the ring while it is waited
    // for.
    let mut b = Built::default();
    network_menu(&mut b, Err(WifiError::Slow));
    shoot("network-failed", &b.items, &Shot::default());
    let mut b = Built::default();
    network_menu(&mut b, Err(WifiError::Pending));
    shoot("network-waiting", &b.items, &Shot::default());

    // On battery, 72 %, 3 h 25 min left, the saver on; then charging.
    let power = |percent: u8, ac: bool, charging: bool, secs: u32, saver: bool| windows::Win32::System::Power::SYSTEM_POWER_STATUS {
        ACLineStatus: ac as u8,
        BatteryFlag: if charging { 8 } else { 0 },
        BatteryLifePercent: percent,
        SystemStatusFlag: saver as u8,
        BatteryLifeTime: secs,
        BatteryFullLifeTime: u32::MAX,
    };
    let mut b = Built::default();
    battery_menu(&mut b, Some(power(72, false, false, 3 * 3600 + 25 * 60, true)));
    shoot("battery", &b.items, &Shot { hover: Some(t("topbar.panel.battery.settings").into()), ..Default::default() });
    let mut b = Built::default();
    battery_menu(&mut b, Some(power(41, true, true, u32::MAX, false)));
    shoot("battery-charging", &b.items, &Shot::default());
}
