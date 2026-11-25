//! The Control Center's tests.

/// The panel's code, every file but this one (for the tests that look
/// for words gone from it).
fn sources() -> String {
    [
        include_str!("../control.rs"),
        include_str!("audio.rs"),
        include_str!("dimmer.rs"),
        include_str!("input.rs"),
        include_str!("jobs.rs"),
        include_str!("layout.rs"),
        include_str!("media.rs"),
        include_str!("motion.rs"),
        include_str!("paint.rs"),
        include_str!("parts.rs"),
        include_str!("quick.rs"),
        include_str!("radio.rs"),
        include_str!("reader.rs"),
    ]
    .concat()
}

use super::*;
use crate::theme::State;

#[test]
fn settings_links_speak_like_the_rest_of_the_panel() {
    // Korean in 해요체, like "바꾸지 못했어요" (owner, 2026-11-08).
    let _l = crate::i18n::test_lock("ko");
    assert_eq!(t("control.in_settings"), "설정에서 바꿔요");
    assert_eq!(t("control.opens_settings"), "설정을 열어요");
}

#[test]
fn an_accepted_radio_switch_waits_for_the_radio() {
    let t0 = std::time::Instant::now();
    let ms = |n: u64| t0 + std::time::Duration::from_millis(n);
    // Accepted, the radio still off: switching, not failed.
    let mut s = Switch::new(true, t0);
    assert_eq!(s.seen(Some(false), t0), SwitchStep::Wait);
    assert_eq!(s.seen(None, ms(500)), SwitchStep::Wait, "Windows still changing it");
    // StateChanged shows it on: done.
    assert_eq!(s.seen(Some(true), ms(900)), SwitchStep::Done);
    // Never shows: failed once, then quiet; a late arrival clears it.
    let mut s = Switch::new(false, t0);
    assert_eq!(s.seen(Some(true), ms(5000)), SwitchStep::Wait);
    assert_eq!(s.seen(Some(true), t0 + SWITCH_WAIT), SwitchStep::Failed);
    assert_eq!(s.seen(Some(true), t0 + SWITCH_WAIT * 2), SwitchStep::Quiet);
    assert_eq!(s.seen(Some(false), t0 + SWITCH_WAIT * 3), SwitchStep::Done, "late success");
    assert_eq!(switch_state(RadioView::On), Some(true));
    assert_eq!(switch_state(RadioView::Locked(true)), None);
}

#[test]
fn a_newer_dark_answer_is_never_wiped() {
    let shared = Shared::new();
    // Two quick clicks: answer 1 arrives, the panel looks at the state
    // (the old code took a snapshot here and cleared later)...
    shared.update(|r| r.dark = Some((1, true)));
    let seen = shared.snapshot();
    assert_eq!(seen.dark, Some((1, true)));
    // ...answer 2 lands before the panel takes its answer.
    shared.update(|r| r.dark = Some((2, false)));
    assert_eq!(shared.take_dark(), Some((2, false)), "the newest answer, not wiped");
    assert_eq!(shared.take_dark(), None, "taken once");
    // In order: each answer taken by itself.
    shared.update(|r| r.dark = Some((3, true)));
    assert_eq!(shared.take_dark(), Some((3, true)));
    shared.update(|r| r.dark = Some((4, false)));
    assert_eq!(shared.take_dark(), Some((4, false)));
}

#[test]
fn a_switch_says_when_it_is_busy_or_did_not_go_through() {
    let t0 = std::time::Instant::now();
    assert_eq!(switch_note(false, None, t0), None, "the ordinary status");
    assert_eq!(switch_note(true, None, t0), Some("control.audio.switching"));
    assert_eq!(switch_note(true, Some(t0), t0), Some("control.audio.switching"), "a new try overrides the last failure");
    assert_eq!(switch_note(false, Some(t0), t0 + std::time::Duration::from_secs(1)), Some("control.switch_failed"));
    assert_eq!(switch_note(false, Some(t0), t0 + FAIL_SHOW), None, "gone after its time");
}

#[test]
fn the_connections_card_and_icon_tiles_share_four_columns() {
    let r = Remote {
        studio: Some("camera".into()), displays: Displays::Many(None),
        brightness: Some(Some(50)), media: Some(Media::default()), hotspot: RadioView::Off,
        ..Default::default()
    };
    let lay = layout(&shape_of(&r, None, 2));
    assert_eq!(W, 328.0);
    assert_eq!(lay.items[0], (Item::Connections, cell(0, 0, 2, 2)));
    let tiles = &lay.items[1..1 + grid_tiles(&r).len()];
    assert_eq!(tiles[0], (Item::Tile(Hit::Dark), cell(2, 0, 1, 1)));
    assert_eq!(tiles[3], (Item::Tile(Hit::Quick(Quick::Awake)), cell(3, 1, 1, 1)));
    assert_eq!(tiles[6], (Item::Tile(Hit::Project), cell(2, 2, 2, 1)));
    assert_eq!(tiles[7], (Item::Tile(Hit::AccessList), cell(0, 3, 1, 1)));
    assert_eq!(tiles[8], (Item::Tile(Hit::Studio), cell(1, 3, 1, 1)));
    assert_eq!(lay.items[1 + tiles.len()..].iter().map(|&(item, _)| item).collect::<Vec<_>>(),
        [Item::Brightness, Item::Volume, Item::Media, Item::Eject(0), Item::Eject(1)]);
    let short = layout(&Shape { section: Some((Section::Project, 100.0)), ..shape_of(&r, None, 0) });
    assert_eq!((short.height, short.fit), (100.0, short.grid_h));
    let long = layout(&Shape { section: Some((Section::Sound, 900.0)), ..shape_of(&r, None, 0) });
    assert_eq!((long.height, long.grid_h, long.fit), (900.0, short.grid_h, 900.0));
}

#[test]
fn removable_strips_follow_media_when_a_device_is_present() {
    let r = Remote { media: Some(Media::default()), ..Default::default() };
    let base = layout(&shape_of(&r, None, 0));
    let with = layout(&shape_of(&r, None, 2));
    assert_eq!(with.items[..base.items.len()], base.items);
    assert_eq!(with.items[base.items.len()..].iter().map(|&(item, _)| item).collect::<Vec<_>>(), [Item::Eject(0), Item::Eject(1)]);
    assert_eq!(with.height, base.height + 2.0 * (UNIT + GAP));
}

#[test]
fn connection_chips_fit_inside_each_row_with_room_for_navigation() {
    let card = cell(0, 0, 2, 2);
    let rows = connection_rects(card, 4).collect::<Vec<_>>();
    assert!(rows.windows(2).all(|pair| pair[0].bottom == pair[1].top));
    for row in rows {
        let chip = connection_chip(row);
        assert_eq!(chip.right - chip.left, CHIP);
        assert!(row.top < chip.top && chip.bottom < row.bottom);
        assert!(chip.right + 96.0 < row.right);
    }
}

#[test]
fn moved_tiles_reject_pointer_clicks_until_the_position_settles() {
    let start = Instant::now();
    let moved = start + Duration::from_millis(10);
    let a = rect(0.0, 0.0, 48.0, 48.0);
    let b = rect(56.0, 0.0, 104.0, 48.0);
    let c = rect(112.0, 0.0, 160.0, 48.0);
    let mut clicks = TileClicks::default();
    clicks.update([(Hit::WifiToggle, a), (Hit::AccessList, b)].into_iter(), start);
    assert!(!clicks.blocked(Hit::AccessList, start));
    let changed = [(Hit::WifiToggle, a), (Hit::Studio, b), (Hit::AccessList, c)];
    clicks.update(changed.into_iter(), moved);
    assert!(!clicks.blocked(Hit::WifiToggle, moved));
    assert!(clicks.blocked(Hit::Studio, moved));
    assert!(clicks.blocked(Hit::AccessList, moved));
    clicks.update(changed.into_iter(), moved + Duration::from_millis(399));
    assert!(clicks.blocked(Hit::AccessList, moved + Duration::from_millis(399)));
    assert!(!clicks.blocked(Hit::AccessList, moved + Duration::from_millis(400)));
}

#[test]
fn wifi_reports_loading_until_the_first_result() {
    let mut r = Remote { wifi: RadioView::Off, ..Default::default() };
    assert_eq!(section_cards(Section::Wifi, &r)[0], [Line::Status("control.off")]);
    r.wifi = RadioView::On;
    assert_eq!(section_cards(Section::Wifi, &r)[0], [Line::Status("control.loading")]);
    r.wifi_networks = Some(Ok(Vec::new()));
    assert_eq!(section_cards(Section::Wifi, &r)[0], [Line::Status("topbar.panel.network.none")]);
}

#[test]
fn toggles_follow_the_machine() {
    use Quick::*;
    let mut r = Remote::default();
    assert_eq!(connection_rows(&r), [Hit::WifiToggle, Hit::BtToggle, Hit::Quick(Airplane)]);
    assert_eq!(grid_tiles(&r), [
        Hit::Dark, Hit::Quick(NightLight), Hit::Quick(Dnd), Hit::Quick(Awake),
        Hit::Quick(EnergySaver), Hit::Cast, Hit::AccessList,
    ]);
    r.radios = Some(false);
    r.system[Setting::Night as usize].state.hidden = true;
    assert!(!connection_rows(&r).contains(&Hit::Quick(Airplane)));
    assert!(!grid_tiles(&r).contains(&Hit::Quick(NightLight)));
    assert!(grid_tiles(&r).contains(&Hit::Quick(EnergySaver)));
    r.studio = Some("camera".into());
    r.hotspot = RadioView::Off;
    r.displays = Displays::Many(None);
    assert_eq!(connection_rows(&r), [Hit::WifiToggle, Hit::BtToggle, Hit::Quick(Hotspot)]);
    assert_eq!(grid_tiles(&r)[4..], [Hit::Cast, Hit::Project, Hit::AccessList, Hit::Studio]);
}

#[test]
fn toggles_say_what_they_do() {
    let t0 = std::time::Instant::now();
    let mut r = Remote::default();
    // Wait for the first result, then offer Settings for unsupported switches.
    for q in [Quick::Airplane, Quick::NightLight, Quick::Dnd] {
        let v = quick_view(q, &r, t0);
        assert_eq!((v.ready, v.link, v.switch, v.status), (false, false, false, Some("control.loading")), "{q:?}");
        r.system[q.system_setting().unwrap() as usize].ready = true;
        let v = quick_view(q, &r, t0);
        assert_eq!((v.link, v.switch, v.on, v.status), (true, false, false, Some("control.in_settings")), "{q:?}");
    }
    for q in [Quick::Airplane, Quick::NightLight, Quick::Dnd] {
        let tile = &mut r.system[q.system_setting().unwrap() as usize];
        tile.state = SettingState { on: Some(true), writable: true, ..Default::default() };
        tile.busy = true;
        let view = quick_view(q, &r, t0);
        assert_eq!((view.link, view.switch, view.on, view.status), (false, true, true, Some("control.audio.switching")));
    }
    // Energy saver: its state read-only, and Settings to change it.
    r.energy = Some(true);
    let v = quick_view(Quick::EnergySaver, &r, t0);
    assert_eq!((v.link, v.switch, v.on, v.status), (true, false, true, Some("control.on")));
    // Hotspot: a switch, with the radios' notes.
    r.hotspot = RadioView::On;
    let v = quick_view(Quick::Hotspot, &r, t0);
    assert_eq!((v.link, v.switch, v.on, v.status), (false, true, true, Some("control.on")));
    r.hotspot_busy = true;
    assert_eq!(quick_view(Quick::Hotspot, &r, t0).status, Some("control.audio.switching"));
    r.hotspot_busy = false;
    r.hotspot = RadioView::Off;
    r.hotspot_failed = Some(t0);
    assert_eq!(quick_view(Quick::Hotspot, &r, t0).status, Some("control.switch_failed"));
    // Keep Awake: a switch of ours, lit while on; refused, it says so
    // a moment, then its state again.
    let v = quick_view(Quick::Awake, &r, t0);
    assert_eq!((v.link, v.switch, v.on, v.status), (false, true, false, Some("control.off")));
    r.awake = true;
    let v = quick_view(Quick::Awake, &r, t0);
    assert_eq!((v.link, v.switch, v.on, v.status), (false, true, true, Some("control.on")));
    r.awake_failed = Some(t0);
    assert_eq!(quick_view(Quick::Awake, &r, t0).status, Some("control.switch_failed"));
    assert_eq!(quick_view(Quick::Awake, &r, t0 + FAIL_SHOW).status, Some("control.on"));
    assert_eq!(quick_page(Quick::Awake, 26100), None);
    assert_eq!(quick_label(Quick::Awake), "control.keep_awake");
    assert_eq!(quick_view(Quick::Hotspot, &r, t0 + FAIL_SHOW).status, Some("control.off"));
    // Project's card: the current mode (none when Windows would not say).
    assert_eq!(project_status(Displays::Many(Some(Topology::Extend))), Some("control.project.extend"));
    assert_eq!(project_status(Displays::Many(None)), None);
    assert_eq!(project_status(Displays::Single), None);
    // The pages: documented ms-settings URIs; Do Not Disturb moved to
    // Notifications in Windows 11 22H2.
    assert_eq!(quick_page(Quick::Airplane, 26100), Some("ms-settings:network-airplanemode"));
    assert_eq!(quick_page(Quick::NightLight, 26100), Some("ms-settings:nightlight"));
    assert_eq!(quick_page(Quick::EnergySaver, 26100), Some("ms-settings:batterysaver"));
    assert_eq!(quick_page(Quick::Dnd, 22621), Some("ms-settings:notifications"));
    assert_eq!(quick_page(Quick::Dnd, 22000), Some("ms-settings:quietmomentshome"));
    assert_eq!(quick_page(Quick::Dnd, 19045), Some("ms-settings:quietmomentshome"));
    assert_eq!(quick_page(Quick::Hotspot, 26100), None);
}

#[test]
fn a_strip_holds_the_slider_and_sound_button() {
    let strip = layout(&shape(false, 0)).at(Item::Volume).unwrap();
    // Brightness: the slider 16 in from both ends, 40 tall, centered.
    let (s, b) = strip_slider(strip, false);
    assert_eq!((s.left, s.right, s.bottom - s.top), (strip.left + STRIP_IN, strip.right - STRIP_IN, CAPSULE));
    assert_eq!((s.top + s.bottom) / 2.0, (strip.top + strip.bottom) / 2.0);
    assert!(b.is_none());
    // Volume: the output button (40 round) at its end, 8 after the slider.
    let (s, b) = strip_slider(strip, true);
    let b = b.unwrap();
    assert_eq!((b.right, b.right - b.left, b.bottom - b.top), (strip.right - STRIP_IN, ROUND_L, ROUND_L));
    assert_eq!(b.left - s.right, GAP);
    // The mute hit target sits at the start of the slider strip.
    let m = mute_in(s);
    assert_eq!((m.left - s.left, m.top - s.top, m.right - m.left), (4.0, 4.0, MUTE));
}

#[test]
fn the_slider_value_follows_the_pointer_between_the_track_endpoints() {
    let s = rect(24.0, 0.0, 304.0, 40.0);
    let (start, end) = track_span(s);
    assert_eq!(start, (mute_in(s).left + mute_in(s).right) / 2.0);
    assert_eq!(end, s.right - CAPSULE / 2.0);
    for v in [0.0f32, 0.25, 0.5, 1.0] {
        assert!((cap_value(start + (end - start) * v, s) - v).abs() < 1e-4, "{v}");
    }
    assert_eq!(cap_value(-100.0, s), 0.0);
    assert_eq!(cap_value(1000.0, s), 1.0);
}

#[test]
fn sliders_clamp() {
    assert_eq!(slider_value(50.0, 50.0, 250.0), 0.0);
    assert_eq!(slider_value(150.0, 50.0, 250.0), 0.5);
    assert_eq!(slider_value(999.0, 50.0, 250.0), 1.0);
    assert_eq!(slider_value(-5.0, 50.0, 250.0), 0.0);
    assert_eq!(slider_value(10.0, 5.0, 5.0), 0.0);
}

#[test]
fn slider_keys_follow_the_kit() {
    let near = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() < 1e-4);
    // Every slider runs across: right and up a percent, left and down
    // back (right to left, left grows), Page Up / Down ten, Home / End.
    assert!(near(slider_key(VK_UP.0, 0.5, false), 0.51));
    assert!(near(slider_key(VK_DOWN.0, 0.5, false), 0.49));
    assert!(near(slider_key(VK_RIGHT.0, 0.5, false), 0.51));
    assert!(near(slider_key(VK_LEFT.0, 0.5, false), 0.49));
    assert!(near(slider_key(VK_LEFT.0, 0.5, true), 0.51));
    assert!(near(slider_key(VK_PRIOR.0, 0.5, false), 0.6));
    assert!(near(slider_key(VK_NEXT.0, 0.05, false), 0.0));
    assert_eq!(slider_key(VK_HOME.0, 0.5, false), Some(0.0));
    assert_eq!(slider_key(VK_END.0, 0.5, false), Some(1.0));
    assert_eq!(slider_key(VK_RETURN.0, 0.5, false), None);
    assert_eq!(WHEEL_STEP, 0.02);
}

fn ep(id: &str, default: bool) -> Endpoint {
    Endpoint { id: id.into(), name: id.into(), kind: devices::Kind::Speakers, default }
}

fn app(pid: u32, active: bool) -> AppMix {
    AppMix { key: mix_key(pid, &["s"]), name: format!("App {pid}"), level: 50, muted: false, active, icon: None }
}

#[test]
fn the_sound_list_holds_devices_inputs_and_apps() {
    let mut r = Remote::default();
    assert!(section_cards(Section::Sound, &r).is_empty(), "still asking: nothing to go into");
    r.outputs = Some(vec![ep("a", true), ep("b", false)]);
    r.inputs = Some(Vec::new());
    let k = |id: &str| device_key(id);
    assert_eq!(section_cards(Section::Sound, &r), vec![vec![Line::Head("control.output"), Line::Device(Flow::Output, k("a")), Line::Device(Flow::Output, k("b"))]]);
    // Inputs under their heading, on the same card.
    r.inputs = Some(vec![ep("m", true)]);
    let cards = section_cards(Section::Sound, &r);
    assert_eq!(&cards[0][3..], &[Line::Head("control.input"), Line::Device(Flow::Input, k("m"))]);
    // One app playing: no mixer (the volume slider is it); two: both, the
    // silent ones left out, on a card of their own.
    r.apps = Some(vec![app(10, true), app(11, false)]);
    assert_eq!(section_cards(Section::Sound, &r).len(), 1);
    r.apps = Some(vec![app(10, true), app(11, false), app(12, true)]);
    let cards = section_cards(Section::Sound, &r);
    assert_eq!(cards[1], vec![Line::Head("control.app_volume"), Line::App(mix_key(10, &["s"])), Line::App(mix_key(12, &["s"]))]);
    assert_eq!(mixer_apps(None).len(), 0);
    // Long lists are not cut: every device can be picked.
    r.outputs = Some((0..20).map(|i| ep(&i.to_string(), i == 15)).collect());
    assert_eq!(section_cards(Section::Sound, &r)[0].iter().filter(|l| matches!(l, Line::Device(Flow::Output, _))).count(), 20);
    // Keys follow the device, not its place in the list.
    assert_eq!(device_key("{0.0.0.00000000}.{a}"), device_key("{0.0.0.00000000}.{a}"));
    assert_ne!(device_key("{0.0.0.00000000}.{a}"), device_key("{0.0.0.00000000}.{b}"));
    // Project's: its four modes.
    assert_eq!(section_cards(Section::Project, &r), vec![Topology::ALL.map(Line::Mode).to_vec()]);
}

#[test]
fn a_failed_switch_is_reported_in_its_list() {
    let mut r = Remote::default();
    assert_eq!(fail_for(&r, Flow::Output), None);
    r.audio_fail = Some((Flow::Output, "Headphones".into()));
    assert_eq!(fail_for(&r, Flow::Output), Some("Headphones"));
    assert_eq!(fail_for(&r, Flow::Input), None);
    // The last device vanished: the list still has the message and the
    // way to Windows' Sound page.
    assert_eq!(section_cards(Section::Sound, &r), vec![vec![Line::Head("control.output"), Line::Failed(Flow::Output), Line::SoundSettings]]);
    r.audio_fail = Some((Flow::Input, String::new()));
    assert_eq!(section_cards(Section::Sound, &r), vec![vec![Line::Head("control.input"), Line::Failed(Flow::Input), Line::SoundSettings]]);
}

#[test]
fn a_list_stacks_under_its_header() {
    let cards = vec![Topology::ALL.map(Line::Mode).to_vec()];
    let (head, list) = sub_plan(&cards);
    assert_eq!(head, rect(PAD, PAD, W - PAD, PAD + SUB_HEAD_H));
    let (card, tops) = &list[0];
    assert_eq!(card.top, head.bottom + GAP);
    assert_eq!(tops[0], card.top + LIST_PAD);
    assert_eq!(card.bottom, card.top + LIST_PAD * 2.0 + 4.0 * LINE_H);
    assert_eq!(sub_h(&cards), card.bottom + PAD);
    // Two cards GAP apart; headings 24, rows 28, apps 32.
    let cards = vec![vec![Line::Head("control.output"), Line::Device(Flow::Output, 1)], vec![Line::Head("control.app_volume"), Line::App(2)]];
    let (_, list) = sub_plan(&cards);
    assert_eq!(list[1].0.top, list[0].0.bottom + GAP);
    assert_eq!(list[0].1, vec![list[0].0.top + LIST_PAD, list[0].0.top + LIST_PAD + HEAD_H]);
    assert_eq!(list[1].0.bottom - list[1].0.top, LIST_PAD * 2.0 + HEAD_H + APP_ROW_H);
    // The back button 28 round, 4 in, around the header's middle.
    let b = back_in(head);
    assert_eq!((b.left, b.right - b.left, (b.top + b.bottom) / 2.0), (PAD + S2, ROUND_S, (head.top + head.bottom) / 2.0));
}

#[test]
fn the_media_card_keeps_metadata_beside_transport_and_above_the_timeline() {
    let strip = cell(0, 0, COLS, 1);
    let m = media_plan(strip, true, false);
    assert_eq!(m.art.top, PAD + (UNIT - ART_DIP) / 2.0);
    assert_eq!(m.text_x, m.art.right + MEDIA_IN);
    assert_eq!(m.buttons.map(|b| b.0), [Hit::Prev, Hit::PlayPause, Hit::Next]);
    assert_eq!(m.buttons.map(|b| b.1.right - b.1.left), [MEDIA_BTN, PLAY_BTN, MEDIA_BTN]);
    assert!(m.buttons.iter().all(|b| b.1.left > m.text_x + m.text_w));
    let bar = m.bar.unwrap();
    assert!(bar.top > m.artist_top + ARTIST_H);
    assert_eq!((bar.left, bar.right, bar.bottom - bar.top), (m.text_x, m.text_x + m.text_w, TRACK_H));
    let bare = media_plan(strip, false, true);
    assert!(bare.bar.is_none());
    assert_eq!(bare.buttons.map(|b| b.0), [Hit::Next, Hit::PlayPause, Hit::Prev]);
}

#[test]
fn springs_settle_like_the_kit() {
    // A is SNAP, C is GLIDE (design system 7.1).
    assert!((SPRING_A.ratio() - 0.980).abs() < 0.001, "{}", SPRING_A.ratio());
    assert!((SPRING_C.ratio() - 0.950).abs() < 0.001, "{}", SPRING_C.ratio());
    assert_eq!((SPRING_A.max_ms, SPRING_C.max_ms), (crate::motion::SNAP_MAX_MS, crate::motion::GLIDE_MAX_MS));
    for s in [SPRING_A, SPRING_C] {
        assert_eq!(s.progress(0.0), 0.0);
        assert!((s.progress(s.max_ms as f32 / 1000.0) - 1.0).abs() < 0.03, "{s:?}");
        let f = s.frames(SPRING_FRAMES);
        assert_eq!(f.len(), SPRING_FRAMES);
        assert_eq!(*f.last().unwrap(), (1.0, 1.0), "pinned on the target");
        assert!(f.iter().all(|&(t, p)| (0.0..=1.0).contains(&t) && (0.0..=1.002).contains(&p)), "{f:?}");
        assert!(f.windows(2).all(|w| w[1].0 > w[0].0), "in time order");
    }
    assert_eq!(SPRING_A.progress(-1.0), 0.0);
    assert_eq!(SPRING_A.progress(f32::NAN), 0.0);
}

#[test]
fn motion_follows_the_kit_and_reduced_motion() {
    use crate::motion::{fade_plan, Mode, CROSS_MS, FADE_IN_MS, FADE_OUT_MS, REDUCED_MS};
    // Reduced motion: every fade 60 ms, straight, the speed setting not
    // applied (motion::fade_plan, design system 12.2); nothing grows.
    for ms in [OPEN_FADE_MS, CLOSE_MS, ART_FADE_MS, CROSS_MS, FADE_IN_MS, FADE_OUT_MS] {
        let (len, curve) = fade_plan(Mode::Reduced, ms, EASE_OUT, 4.0);
        assert_eq!(len, REDUCED_MS.min(ms));
        assert_eq!(curve, crate::motion::fade_curve(Mode::Reduced, EASE_OUT));
    }
    assert_eq!(open_from(false), 1.0);
    // Like the other panels: grows from 0.96 on GLIDE, fades 180 in and
    // 120 out; art crosses in 180.
    assert_eq!(open_from(true), crate::motion::OPEN_SCALE);
    assert_eq!(SPRING_C.kit, crate::motion::SPRING_GLIDE);
    assert_eq!((OPEN_FADE_MS, CLOSE_MS, ART_FADE_MS), (crate::motion::PANEL_FADE_MS, crate::motion::CLOSE_MS, CROSS_MS));
    // A button's colors (6.3): a press at once, a switch crossing, a
    // hover on 90 easing out (a press let go too), off 120 easing in.
    let rest = Look::default();
    assert_eq!(tone_ms(rest, Look { hover: true, pressed: true, ..rest }), (0, EASE_OUT));
    assert_eq!(tone_ms(Look { hover: true, ..rest }, Look { on: true, hover: true, ..rest }), (CROSS_MS, EASE_OUT));
    assert_eq!(tone_ms(rest, Look { hover: true, ..rest }), (FADE_IN_MS, EASE_OUT));
    assert_eq!(tone_ms(Look { hover: true, pressed: true, ..rest }, Look { hover: true, ..rest }), (FADE_IN_MS, EASE_OUT));
    assert_eq!(tone_ms(Look { hover: true, ..rest }, rest), (FADE_OUT_MS, EASE_EXIT));
    // A press shrinks by the one formula: a 72 tile, a 28 chip, a 40
    // button.
    assert!((crate::motion::press_scale(UNIT, UNIT) - (1.0 - 2.0 / UNIT)).abs() < 1e-6);
    assert_eq!(crate::motion::press_scale(ROUND_S, ROUND_S), 0.94);
    assert_eq!(crate::motion::press_scale(ROUND_L, ROUND_L), 0.95);
}

#[test]
fn tones_are_achromatic_and_tell_on_by_lightness() {
    let hex = |s: &str| crate::theme::parse_color(s).unwrap();
    let luma = |c: Rgba| c.r as u32 + c.g as u32 + c.b as u32;
    // Graphite dark: the tiles' states and track come in as given.
    let d = tones(false, (hex("#313131"), hex("#3B3B3B"), hex("#414141")), hex("#454545"), hex("#E6E6E6"), hex("#FFFFFF9E"), hex("#242424F7"), None);
    assert_eq!(d.on, hex("#E6E6E6"));
    assert_eq!(d.on_ink, hex("#171717"));
    assert_eq!((d.tile, d.tile_hover, d.tile_press, d.bar), (hex("#313131"), hex("#3B3B3B"), hex("#414141"), hex("#454545")));
    // The chips (on a card) and rows: the kit's washes.
    assert_eq!((d.chip.a, d.chip_hover.a, d.chip_press.a), (26, 41, 18));
    assert_eq!((d.row_hover.a, d.row_press.a), (18, 28));
    // Light: a near-black "on".
    let l = tones(true, (hex("#E9E9E9"), hex("#E1E1E1"), hex("#DBDBDB")), hex("#D1D1D1"), hex("#272727"), hex("#00000099"), hex("#F5F5F5D1"), None);
    for (t, light) in [(d, false), (l, true)] {
        let all = [t.tile, t.tile_hover, t.tile_press, t.ink, t.dim, t.on, t.on_ink, t.on_hover, t.on_press, t.fill, t.bar, t.chip, t.row_hover];
        for c in all {
            let spread = c.r.max(c.g).max(c.b) - c.r.min(c.g).min(c.b);
            assert!(spread <= if light { 0x18 } else { 0 }, "{c:?} takes a hue");
        }
        // On: lighter than off in dark mode, darker in light mode; its
        // glyph the other way.
        assert_eq!(luma(t.on) > luma(Rgba { a: 255, ..t.tile }), !light);
        assert_eq!(luma(t.on_ink) < luma(t.on), !light);
        assert_eq!(raised_tone(&t, Look { on: true, ..Look::default() }), (t.on, t.on_ink));
        // Off: the card's fills with the ink (no faint glyph at rest).
        assert_eq!(raised_tone(&t, Look::default()), (t.tile, t.ink));
        assert_eq!(raised_tone(&t, Look { pressed: true, hover: true, ..Look::default() }).0, t.tile_press);
        assert_eq!(chip_tone(&t, Look { hover: true, ..Look::default() }), (t.chip_hover, t.ink));
        assert_eq!(glyph_tone(&t, Look::default()).0.a, 0, "a glyph button has no fill at rest");
        assert_eq!(glyph_tone(&t, Look { hover: true, ..Look::default() }).0, t.row_hover);
        // A thick slider's fill is white in both looks, its glyph dark.
    }
    assert_eq!(l.on_ink, hex("#F9F9F9"));
    assert_eq!(mix(BLACK, WHITE, 0.5), Rgba { r: 128, g: 128, b: 128, a: 255 });
}

#[test]
fn high_contrast_uses_the_system_colors() {
    let hc = crate::syslook::HighContrast { window: BLACK, text: WHITE, highlight: Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 }, highlight_text: BLACK, gray_text: crate::syslook::GRAY_TEXT };
    let rest = Look::default();
    // At rest: an outline of WindowText 1 on a face, nothing on a glyph
    // button; hovered: Highlight 2 inside; on or pressed: Highlight with
    // HighlightText.
    assert_eq!(hc_tone(&hc, rest, true), (WHITE, Some(1.0), WHITE));
    assert_eq!(hc_tone(&hc, rest, false).0.a, 0);
    assert_eq!(hc_tone(&hc, Look { hover: true, ..rest }, true), (hc.highlight, Some(2.0), WHITE));
    assert_eq!(hc_tone(&hc, Look { on: true, ..rest }, true), (hc.highlight, None, BLACK));
    assert_eq!(hc_tone(&hc, Look { hover: true, pressed: true, ..rest }, false), (hc.highlight, None, BLACK));
    // The source keeps the old kit shadows and stepped fills out.
    let body = &sources();
    for gone in ["Shade::", "soft_shadow(", "0.66", "CardHover", "under_mark", "side_mark", "run_accordion"] {
        assert!(!body.contains(gone), "{gone} is back");
    }
}

#[test]
fn on_follows_the_accent() {
    use crate::syslook::SysLook;
    let blue = Rgba { r: 0x00, g: 0x78, b: 0xD4, a: 255 };
    let ink = crate::theme::on_color(blue);
    let mut raw = crate::theme::Theme::graphite();
    let neutral = tones_of(&raw.resolve(&SysLook { light: true, accent: blue, ..Default::default() }));
    assert_eq!(neutral.on.r, neutral.on.g);
    assert_eq!(neutral.on.g, neutral.on.b);
    // Explicit system accent: Windows' accent on what is on.
    raw.accent = "system".into();
    let light = tones_of(&raw.resolve(&SysLook { light: true, accent: blue, ..Default::default() }));
    assert_eq!((light.on, light.on_ink, light.fill), (blue, ink, blue));
    assert_eq!(raised_tone(&light, Look { on: true, ..Look::default() }), (blue, ink));
    assert!(light.on_hover != blue && light.on_press != blue);
    // Dark, by default: the achromatic "on" as before.
    let t = raw.resolve(&SysLook { light: false, accent: blue, ..Default::default() });
    let dark = tones_of(&t);
    assert_eq!(dark.on, t.ink());
    assert_eq!(dark.on, tones(false, t.card_states(), t.track(), t.ink(), t.text_secondary(), t.popup_tint(), None).on);
    // A chosen color: in both modes, with readable ink.
    let mut mine = raw.clone();
    mine.accent = "#FFD60A".into();
    for l in [false, true] {
        let tn = tones_of(&mine.resolve(&SysLook { light: l, accent: blue, ..Default::default() }));
        assert_eq!((tn.on, tn.on_ink), (Rgba { r: 0xFF, g: 0xD6, b: 0x0A, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 }));
    }
}

#[test]
fn on_discs_read_in_every_state() {
    use crate::syslook::SysLook;
    use crate::theme::contrast;
    let states = [Look { on: true, ..Look::default() }, Look { on: true, hover: true, ..Look::default() }, Look { on: true, hover: true, pressed: true }];
    // The review's case: Windows' default blue under white (4.53:1).
    // Its hover used to lighten to #1A86D8 under the same white (3.86).
    let blue = Rgba { r: 0x00, g: 0x78, b: 0xD4, a: 255 };
    let mut raw = crate::theme::Theme::graphite();
    raw.accent = "#0078D4".into();
    let tn = tones_of(&raw.resolve_fx(&SysLook { light: true, ..Default::default() }, true));
    let seen: Vec<(Rgba, Rgba)> = states.iter().map(|l| raised_tone(&tn, *l)).collect();
    assert_eq!(seen[0], (blue, WHITE));
    for (fill, ink) in &seen {
        assert_eq!(*ink, WHITE);
        assert!(contrast(*ink, *fill) >= DISC_CONTRAST, "{fill:?}");
    }
    assert!(seen[0].0 != seen[1].0 && seen[1].0 != seen[2].0, "hover and press still show");
    // Every accent, both modes, both finishes, every state: one
    // background per state (no sheen over the disc), 4.5 against it,
    // one ink across the states, white whenever white reads.
    let mut glass = crate::theme::Theme::graphite();
    glass.finish = crate::theme::Finish::Glass;
    let steps = || (0..=255u32).step_by(17);
    for light in [false, true] {
        for finish in [crate::theme::Finish::Flat, crate::theme::Finish::Glass] {
            let mut raw = crate::theme::Theme::graphite();
            raw.finish = finish;
            let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
            assert_eq!(t.glass(), finish == crate::theme::Finish::Glass);
            let base = tones_of(&t);
            for (r, g, b) in steps().flat_map(|r| steps().flat_map(move |g| steps().map(move |b| (r, g, b)))) {
                let on = Rgba { r: r as u8, g: g as u8, b: b as u8, a: 255 };
                let tn = tones(light, (base.tile, base.tile_hover, base.tile_press), base.bar, base.ink, base.dim, t.popup_tint(), Some((on, crate::theme::on_color(on))));
                let seen: Vec<(Rgba, Rgba)> = states.iter().map(|l| raised_tone(&tn, *l)).collect();
                for (l, (fill, ink)) in states.iter().zip(&seen) {
                    assert_eq!(fill.a, 255, "an opaque disc");
                    assert!(contrast(*ink, *fill) >= DISC_CONTRAST, "on {on:?} {l:?} light {light} {finish:?}: {}", contrast(*ink, *fill));
                }
                assert!(seen.iter().all(|(_, i)| *i == seen[0].1), "one glyph color across states ({on:?})");
                if contrast(WHITE, on) >= DISC_CONTRAST {
                    assert_eq!(seen[0].1, WHITE, "white first ({on:?})");
                }
            }
        }
    }
    // The review's case (Glass light, #0064B4, 6.03:1 under white):
    // white in every state, never black for a sheen that is gone.
    let mut deep = glass.clone();
    deep.accent = "#0064B4".into();
    let tn = tones_of(&deep.resolve_fx(&SysLook { light: true, ..Default::default() }, true));
    for l in states {
        let (fill, ink) = raised_tone(&tn, l);
        assert_eq!(ink, WHITE, "{l:?}");
        assert!(contrast(ink, fill) >= 6.0, "{l:?}: {}", contrast(ink, fill));
    }
    // The achromatic "on" keeps the kit's numbers, in both finishes.
    for light in [false, true] {
        for raw in [crate::theme::Theme::graphite(), glass.clone()] {
            let t = tones_of(&raw.resolve_fx(&SysLook { light, ..Default::default() }, true));
            assert_eq!(raised_tone(&t, states[1]), (t.on_hover, t.on_ink));
            assert_eq!(raised_tone(&t, states[2]), (t.on_press, t.on_ink));
            for l in states {
                let (fill, ink) = raised_tone(&t, l);
                assert!(contrast(ink, fill) >= DISC_CONTRAST, "light {light} {l:?}");
            }
        }
    }
}

#[test]
fn glass_tones_read_on_the_thin_panel() {
    use crate::syslook::SysLook;
    let lum = |c: Rgba| crate::theme::luminance(Rgba { a: 255, ..c });
    for light in [false, true] {
        let mut raw = crate::theme::Theme::graphite();
        raw.finish = crate::theme::Finish::Glass;
        let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
        assert!(t.glass());
        let tn = tones_of(&t);
        // The tile is a see-through white fill, hovered and pressed as
        // the theme's tile fills (fill_tile, card_states); the track is
        // fill_track, see-through too: on the tile it stands off it.
        assert!(tn.tile.a < 255 && (tn.tile.r, tn.tile.g, tn.tile.b) == (255, 255, 255));
        let (_, hover, pressed) = t.card_states();
        assert_eq!((tn.tile_hover, tn.tile_press), (hover, pressed));
        assert_eq!((tn.tile, tn.tile_hover, tn.tile_press), (t.fill_tile(State::Rest), t.fill_tile(State::Hover), t.fill_tile(State::Pressed)));
        assert_eq!(tn.bar, t.fill_track());
        let panel = Rgba { a: 255, ..t.popup_tint() };
        let tile = crate::theme::over(tn.tile, panel);
        let track = crate::theme::over(tn.bar, tile);
        assert!((lum(track) - lum(tile)).abs() > 0.02, "track stands off the tile (light {light})");
        assert!(lum(tn.fill).max(lum(track)) / lum(tn.fill).min(lum(track)).max(0.001) > 1.5, "fill reads on its track (light {light})");
        // Still achromatic.
        for c in [tn.tile_hover, tn.tile_press, tn.bar, tn.dim] {
            assert!(c.r == c.g && c.g == c.b, "{c:?}");
        }
    }
}

/// What `c` (straight alpha) looks like over the opaque `bottom`.
fn seen(c: Rgba, bottom: Rgba) -> Rgba {
    crate::theme::over(c, Rgba { a: 255, ..bottom })
}

#[test]
fn secondary_text_reads_on_every_tile_state_over_any_wallpaper() {
    // The design B1 review's P2: opaque text_dim() fell to 3.6-3.9:1 on
    // Glass over white or black wallpapers. The panel's secondary text
    // (statuses, card lines, values) is the translucent token now and
    // keeps 4.5:1 on the panel and on every tile state, both finishes.
    use crate::syslook::SysLook;
    use crate::theme::contrast;
    // Glass v3 review 3: against the panel as text sees it (its top
    // light and grain at their bounds, every fallback level: Theme::
    // plate_seen) over the greys and the full colors, every finish.
    let mut worst = f32::MAX;
    for light in [false, true] {
        for finish in [crate::theme::Finish::Flat, crate::theme::Finish::Glass] {
            let mut raw = crate::theme::Theme::graphite();
            raw.finish = finish;
            let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
            let tn = tones_of(&t);
            // The translucent token, raised only where it must be.
            let sec = t.text_secondary();
            assert_eq!((tn.dim.r, tn.dim.g, tn.dim.b), (sec.r, sec.g, sec.b));
            assert!(tn.dim.a >= sec.a && tn.dim.a <= t.text_primary().a, "{:?}", tn.dim);
            if finish == crate::theme::Finish::Flat {
                assert_eq!(tn.dim, sec, "Flat keeps the token as it is");
            }
            for (wp, need, panel) in crate::theme::backdrops().flat_map(|(wp, need)| t.plate_seen(crate::theme::Material::Panel, wp).into_iter().map(move |p| (wp, need, p))) {
                for (what, under) in [("panel", panel), ("tile", seen(tn.tile, panel)), ("hover", seen(tn.tile_hover, panel)), ("pressed", seen(tn.tile_press, panel))] {
                    let c = contrast(seen(tn.dim, under), under);
                    worst = worst.min(c);
                    assert!(c >= need, "light {light} {finish:?} wp {wp:?} {what}: {c}");
                    let ink = contrast(seen(tn.ink, under), under);
                    assert!(ink >= need, "primary text, light {light} {finish:?} wp {wp:?} {what}: {ink}");
                }
                // The tertiary (faint: the list cards' group heads) on the
                // panel and the tile at rest.
                for (what, under) in [("panel", panel), ("tile", seen(tn.tile, panel))] {
                    let c = contrast(seen(tn.faint, under), under);
                    assert!(c >= need, "faint, light {light} {finish:?} wp {wp:?} {what}: {c}");
                }
            }
        }
    }
    eprintln!("worst secondary contrast {worst}");
}

/// Every edge of `r` on the 4 DIP grid.
fn on_grid(r: &D2D_RECT_F) -> bool {
    [r.left, r.top, r.right, r.bottom].iter().all(|v| (v / 4.0).fract().abs() < 1e-4)
}

#[test]
fn quick_settings_metrics_are_dpi_independent() {
    assert_eq!((W, UNIT, COLS, PANEL_CORNER, TILE_CORNER), (328.0, 72.0, 4, 8.0, 4.0));
    assert_eq!(BASE_SIZE, 1.0);
    let layout = layout(&shape(false, 0));
    for (_, r) in &layout.items {
        assert!(on_grid(r), "{r:?}");
    }
    let px = |v: f32, s: f32| (v * s * BASE_SIZE).ceil() as u32;
    assert_eq!([px(W, 1.0), px(W, 1.5), px(W, 2.0)], [328, 492, 656]);
}

#[test]
fn corners_come_from_the_theme_and_nest() {
    // Glass: the panel 22, PAD 8 in: its tiles and cards 14; the
    // connections' rows and a list's sit 4 inside their card: 10. Flat:
    // crisp, the panel 10, tiles 8, controls 6.
    let mut raw = crate::theme::Theme::graphite();
    raw.finish = crate::theme::Finish::Glass;
    let r = raw.resolve_fx(&crate::syslook::SysLook { transparency: true, ..Default::default() }, true).radii();
    assert_eq!((r.panel, r.tile, r.control), (22.0, 14.0, 14.0));
    assert_eq!(crate::theme::concentric(r.tile, LIST_PAD), 10.0);
    let f = crate::theme::Theme::graphite().resolve(&crate::syslook::SysLook::default()).radii();
    assert_eq!((f.panel, f.tile, f.control), (10.0, 8.0, 6.0));
    // Sliders, chips, glyph buttons and tracks are capsules.
    for h in [CAPSULE, ROUND_L, ROUND_S, MUTE, MEDIA_BTN, PLAY_BTN, EJECT_BTN, TRACK_H] {
        assert_eq!(crate::theme::capsule(h), h / 2.0);
    }
    // No literal radii in the drawing code.
    let body = &sources();
    for gone in ["PANEL_R:", "CARD_R:", "MEDIA_R:", "LINE_R:", "ART_R:"] {
        assert!(!body.contains(gone), "{gone}");
    }
}

#[test]
fn type_is_the_ramp_s_without_bold() {
    use crate::theme::ramp;
    let ramp_sizes: Vec<f32> = ramp::RAMP.iter().map(|s| s.size).collect();
    for size in [T_NAME, T_BODY, T_SMALL, T_CAPTION, T_HEAD] {
        assert!(ramp_sizes.contains(&size), "{size}");
    }
    // Names Body Strong 13, rows Body 13, values Footnote 12, an app's
    // percent Caption 11 (a number), a list's title Headline 15.
    assert_eq!((T_NAME, T_BODY, T_SMALL, T_CAPTION, T_HEAD), (ramp::BODY_STRONG.size, ramp::BODY.size, ramp::FOOTNOTE.size, ramp::CAPTION.size, ramp::HEADLINE.size));
    let body = &sources();
    assert!(!body.contains("DWRITE_FONT_WEIGHT_BOLD"), "no Bold");
}

#[test]
fn tile_and_row_columns_fit() {
    let strip = rect(PAD, 300.0, W - PAD, 360.0);
    let (gx, x, maxw, b) = eject_columns(strip);
    assert_eq!(gx - TILE_GLYPH / 2.0, strip.left + STRIP_IN);
    assert_eq!(b.right, strip.right - MEDIA_IN);
    assert!(x + maxw <= b.left - MEDIA_IN && maxw >= 180.0, "{maxw}");
    // An app row in a list: name, track, percent, apart.
    let lr = rect(PAD + LIST_PAD, 0.0, W - PAD - LIST_PAD, APP_ROW_H);
    let c = app_columns(lr);
    assert_eq!(c.name, lr.left + ROW_IN);
    assert_eq!(c.track.0, c.name + APP_NAME_W + ROW_IN);
    assert_eq!(c.track.1 + ROW_IN + APP_VALUE_W + ROW_IN, lr.right);
    assert!(c.track.1 - c.track.0 >= 120.0, "{c:?}");
}

#[test]
fn art_is_bounded_scaled_then_cropped() {
    let crop = |p: ArtPlan| {
        let c = p.crop();
        (c.X, c.Y, c.Width, c.Height)
    };
    // Square: shrunk to the cap, nothing cropped.
    let p = art_plan(500, 500).unwrap();
    assert_eq!((p.w, p.h, p.px), (ART_PX, ART_PX, ART_PX));
    assert_eq!(crop(p), (0, 0, 128, 128));
    // Wide: the shorter side scaled to the cap, then the middle square.
    let p = art_plan(1280, 720).unwrap();
    assert_eq!((p.w, p.h, p.px), (228, 128, 128));
    assert_eq!(crop(p), (50, 0, 128, 128));
    // Tall and tiny: never scaled up.
    let p = art_plan(60, 90).unwrap();
    assert_eq!((p.w, p.h, p.px), (60, 90, 60));
    assert_eq!(crop(p), (0, 15, 60, 60));
    // The source is bounded before decoding: empty, too many pixels,
    // a strip, or sizes that would overflow.
    assert!(art_plan(0, 10).is_none() && art_plan(10, 0).is_none());
    assert!(art_plan(4096, 4096).is_some());
    assert!(art_plan(4097, 4096).is_none());
    assert!(art_plan(40_000, 30_000).is_none());
    assert!(art_plan(u32::MAX, u32::MAX).is_none());
    assert!(art_plan(1000, 100).is_none() && art_plan(800, 100).is_some());
    // Sharp at the card's 64 DIP up to 200 %.
    assert!(ART_PX as f32 >= ART_DIP * 2.0);
}

#[test]
fn art_cache_reads_and_decodes_only_when_needed() {
    use std::cell::Cell;
    let key = |t: &str| ("app".to_string(), t.to_string(), "artist".to_string(), "album".to_string(), 1);
    let art = |v: u8| Art { px: 1, pixels: vec![v; 4] };
    let reads = Cell::new(0);
    let decodes = Cell::new(0);
    let mut c = ArtCache::default();
    let step = |c: &mut ArtCache, k, fresh, f: Fetch| {
        c.resolve(
            k,
            fresh,
            || {
                reads.set(reads.get() + 1);
                f
            },
            |b| {
                decodes.set(decodes.get() + 1);
                Some(art(b[0]))
            },
        )
    };
    // First read decodes; playback events reuse it without reading.
    assert_eq!(step(&mut c, key("a"), true, Fetch::Bytes(vec![1])).unwrap().pixels[0], 1);
    assert!(step(&mut c, key("a"), false, Fetch::Absent).is_some());
    assert_eq!((reads.get(), decodes.get()), (1, 1));
    // Properties changed, same bytes: read again, not decoded again.
    step(&mut c, key("a"), true, Fetch::Bytes(vec![1]));
    assert_eq!((reads.get(), decodes.get()), (2, 1));
    // Same title, new art: decoded.
    assert_eq!(step(&mut c, key("a"), true, Fetch::Bytes(vec![2])).unwrap().pixels[0], 2);
    assert_eq!(decodes.get(), 2);
    // A passing failure keeps what is shown, and is retried, then given up.
    let mut c = ArtCache::default();
    assert!(step(&mut c, key("b"), true, Fetch::Failed).is_none());
    assert!(!c.settled);
    let n = reads.get();
    assert!(step(&mut c, key("b"), false, Fetch::Failed).is_none());
    assert_eq!(reads.get(), n + 1);
    assert!(c.settled);
    step(&mut c, key("b"), true, Fetch::Bytes(vec![3]));
    assert_eq!(reads.get(), n + 1, "gave up after {ART_RETRIES} tries");
    // ... and a success after one failure is kept.
    let mut c = ArtCache::default();
    step(&mut c, key("c"), true, Fetch::Failed);
    assert_eq!(step(&mut c, key("c"), true, Fetch::Bytes(vec![4])).unwrap().pixels[0], 4);
    // No thumbnail yet: asked again on the next event of any kind.
    let mut c = ArtCache::default();
    assert!(step(&mut c, key("d"), true, Fetch::Absent).is_none());
    assert!(step(&mut c, key("d"), false, Fetch::Bytes(vec![5])).is_some());
    // Undecodable for sure: no more reads on playback events.
    let mut c = ArtCache::default();
    step(&mut c, key("e"), true, Fetch::Bad);
    let n = reads.get();
    assert!(step(&mut c, key("e"), false, Fetch::Bytes(vec![6])).is_none());
    assert_eq!(reads.get(), n);
    // Another track starts over.
    assert!(step(&mut c, key("f"), false, Fetch::Bytes(vec![7])).is_some());
}

#[test]
fn focus_wraps_both_ways() {
    let o = [Hit::WifiToggle, Hit::Volume, Hit::Outputs];
    assert_eq!(step_focus(&o, None, 1), Some(Hit::WifiToggle));
    assert_eq!(step_focus(&o, None, -1), Some(Hit::Outputs));
    assert_eq!(step_focus(&o, Some(Hit::Outputs), 1), Some(Hit::WifiToggle));
    assert_eq!(step_focus(&o, Some(Hit::WifiToggle), -1), Some(Hit::Outputs));
    // A control that went away (a card hidden): start over.
    assert_eq!(step_focus(&o, Some(Hit::Prev), 1), Some(Hit::WifiToggle));
    assert_eq!(step_focus(&[], None, 1), None);
    // Which controls are sliders.
    assert!(Hit::Volume.slider() && Hit::Brightness.slider() && Hit::App(1).slider() && !Hit::Mute.slider() && !Hit::Back.slider());
}

#[test]
fn radio_states() {
    assert!(RadioView::On.on() && RadioView::Locked(true).on());
    assert!(!RadioView::Off.on() && !RadioView::Loading.on() && !RadioView::Missing.on() && !RadioView::Locked(false).on());
    assert!(RadioView::Denied(true).on() && !RadioView::Denied(false).on());
    // A refused radio is shown as such, not as a switch.
    assert_eq!(view_of(Some(RadioState::On), false), RadioView::On);
    assert_eq!(view_of(Some(RadioState::Off), false), RadioView::Off);
    assert_eq!(view_of(Some(RadioState::On), true), RadioView::Denied(true));
    assert_eq!(view_of(Some(RadioState::Off), true), RadioView::Denied(false));
    assert_eq!(view_of(Some(RadioState::Disabled), false), RadioView::Locked(false));
    assert_eq!(view_of(None, false), RadioView::Locked(false));
}

#[test]
fn radios_say_why_they_cannot_switch() {
    // Each cause reads differently and leads to its own page.
    let causes = [RadioView::Missing, RadioView::Denied(false), RadioView::Locked(false)];
    let reasons: Vec<_> = causes.iter().map(|v| radio_reason(*v).unwrap()).collect();
    assert_eq!(reasons, ["control.radio.missing", "control.radio.denied", "control.radio.locked"]);
    assert_eq!(radio_fix(true, RadioView::Denied(true)), Some("ms-settings:privacy-radios"));
    assert_eq!(radio_fix(true, RadioView::Missing), Some("ms-settings:network-wifi"));
    assert_eq!(radio_fix(false, RadioView::Locked(false)), Some("ms-settings:bluetooth"));
    for v in [RadioView::On, RadioView::Off, RadioView::Loading] {
        assert_eq!((radio_reason(v), radio_fix(true, v)), (None, None), "{v:?} switches here");
    }
}

#[test]
fn rtl_transform_reflects_and_unmirrors() {
    let m = mirrored(2.0, 20.0, 20.0);
    // DIP x maps to ox + (W - x) * s.
    assert_eq!(m.M11 * 0.0 + m.M31, 20.0 + W * 2.0);
    assert_eq!(m.M11 * W + m.M31, 20.0);
    let p = unmirror(m);
    assert_eq!((p.M11, p.M22, p.M31, p.M32), (2.0, 2.0, 20.0, 20.0));
    assert_eq!(unmirror(p).M31, 20.0, "a plain transform stays");
}

/// The panel's width and height in px for `height` kit units at size
/// `size` on a monitor at `scale` (as place makes them).
fn px(kit: f32, scale: f32, size: f32) -> i32 {
    (kit * scale * size).ceil() as i32
}

/// The grid for a desktop (no battery, one display), playing or not, with
/// `removable` devices.
fn shape(media: bool, removable: usize) -> Shape {
    shape_of(&Remote { media: media.then(Media::default), ..Default::default() }, None, removable)
}

/// The panel at rest with `n` removable devices.
fn rest_n(n: usize) -> f32 {
    rest_h(&shape(true, n))
}

#[test]
fn panel_stays_in_the_work_area() {
    let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
    let work = r(0, 28, 1920, 1080);
    let g = crate::placement::popup_gap(1.0);
    // Normal: the popup gap under the button and in from the work
    // area's right end, wherever the button is (the calendar's spot
    // too), at the kit's proportions (room to spare: not shrunk).
    let (p, size) = place(r(1700, 0, 1730, 28), work, 1.0, 1.0, rest_n(0), 300.0, false, false);
    assert_eq!((p.right, p.top, size), (1920 - g, 28 + g, BASE_SIZE));
    assert_eq!((p.bottom - p.top, p.right - p.left), (px(300.0, 1.0, BASE_SIZE), px(W, 1.0, BASE_SIZE)));
    for b in [r(1900, 0, 1930, 28), r(900, 0, 930, 28)] {
        assert_eq!(place(b, work, 1.0, 1.0, rest_n(0), 300.0, false, false).0.right, 1920 - g);
    }
    // Right to left: the gap in from the left end, wherever the button is.
    for b in [r(190, 0, 220, 28), r(-10, 0, 20, 28), r(1900, 0, 1930, 28)] {
        let (p, _) = place(b, work, 1.0, 1.0, rest_n(0), 300.0, true, false);
        assert_eq!((p.left, p.right), (g, g + px(W, 1.0, BASE_SIZE)));
    }
    // A small screen (768 px at 200 %): shrunk, inside.
    let small = r(0, 56, 1366, 768);
    let (p, size) = place(r(1300, 0, 1340, 56), small, 2.0, 1.0, rest_n(0), 400.0, false, false);
    assert!(size < BASE_SIZE);
    assert!(p.top >= small.top && p.bottom <= small.bottom, "{p:?}");
    // Fits the screen, but not below the button: shrunk to the room
    // under it rather than moved up over the gap.
    let (p, size) = place(r(1300, 500, 1340, 550), r(0, 0, 1366, 700), 1.5, 1.0, rest_n(0), 400.0, false, false);
    assert_eq!(p.top, 550 + crate::placement::popup_gap(1.5));
    assert!(p.bottom <= 700 && size < BASE_SIZE, "{p:?} {size}");
}

#[test]
fn panel_opens_upward_over_a_bottom_bar() {
    let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
    // The bar reserves the bottom 28 px.
    let work = r(0, 0, 1920, 1052);
    let button = r(1700, 1052, 1730, 1080);
    let gap = crate::placement::popup_gap(1.0);
    let (p, size) = place(button, work, 1.0, 1.0, rest_n(0), 300.0, false, true);
    assert_eq!((p.right, p.bottom, size), (1920 - gap, 1052 - gap, BASE_SIZE));
    assert_eq!(p.bottom - p.top, px(300.0, 1.0, BASE_SIZE));
    // Right to left: at the left end, still above.
    let (p, _) = place(button, work, 1.0, 1.0, rest_n(0), 300.0, true, true);
    assert_eq!((p.bottom, p.left), (1052 - gap, gap));
    // Grows upward: a taller panel (a long list) keeps its bottom on the
    // button.
    let (q, _) = place(button, work, 1.0, 1.0, rest_n(0), 520.0, false, true);
    assert_eq!((q.bottom, q.top), (1052 - gap, 1052 - gap - px(520.0, 1.0, BASE_SIZE)));
    // A small screen: shrunk, inside the work area.
    let small = r(0, 0, 1366, 768 - 56);
    let (p, size) = place(r(1300, 712, 1340, 768), small, 2.0, 1.0, rest_n(0), 400.0, false, true);
    assert!(size < BASE_SIZE);
    assert!(p.top >= small.top && p.bottom <= small.bottom, "{p:?}");
}

#[test]
fn a_full_panel_fits_a_small_screen_with_the_bar_at_either_edge() {
    let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
    let full = layout(&shape(true, 0)).height;
    assert_eq!(full, rest_n(0));
    // 1920 x 1080 and 1366 x 768 at 100 %, a bar at the top or at the
    // bottom: the kit's proportions, inside the work area.
    let (_, size) = place(r(1800, 0, 1840, 40), r(0, 40, 1920, 1080), 1.0, 1.0, full, full, false, false);
    assert_eq!(size, BASE_SIZE);
    for (button, work, up) in [(r(1300, 0, 1340, 28), r(0, 28, 1366, 768), false), (r(1300, 740, 1340, 768), r(0, 0, 1366, 740), true)] {
        let (p, size) = place(button, work, 1.0, 1.0, full, full, false, up);
        assert!(p.top >= work.top && p.bottom <= work.bottom && p.left >= work.left && p.right <= work.right, "{p:?}");
        assert_eq!(size, panel_size(1366.0, 740.0, 1.0, full, full));
    }
    // A long sound list and two removable devices, at 150 % on 1080:
    // inside at both edges.
    let r_ = Remote { outputs: Some((0..12).map(|i| ep(&i.to_string(), i == 0)).collect()), inputs: Some(vec![ep("m", true)]), ..Default::default() };
    let sec = sub_h(&section_cards(Section::Sound, &r_));
    let tall = layout(&Shape { section: Some((Section::Sound, sec)), ..shape(true, 2) }).height;
    for (button, work, up) in [(r(1800, 0, 1840, 42), r(0, 42, 1920, 1080), false), (r(1800, 1038, 1840, 1080), r(0, 0, 1920, 1038), true)] {
        let (p, size) = place(button, work, 1.5, 1.0, rest_n(2), tall, false, up);
        let visible = viewport_height(tall, p, 1.5 * size);
        assert!(visible * size * 1.5 <= (work.bottom - work.top) as f32 + 1.0);
        assert!(p.top >= work.top && p.bottom <= work.bottom, "{p:?}");
    }
}

#[test]
fn the_panel_scrolls_when_it_reaches_its_minimum_readable_size() {
    let height = 1_200.0;
    for (w, h, dpi) in [(2560, 1600, 1.5), (3840, 2160, 1.0), (1440, 2560, 1.0), (2560, 1440, 1.0), (1366, 480, 1.5)] {
        let button = RECT { left: w - 96, top: 0, right: w - 48, bottom: 39 };
        let work = RECT { left: 0, top: 39, right: w, bottom: h };
        let (window, size) = place(button, work, dpi, 1.0, height, height, false, false);
        let visible = viewport_height(height, window, dpi * size);
        assert!(size >= MIN_SIZE && window.bottom <= h);
        let room = (h - 39 - crate::placement::popup_gap(dpi)) as f32;
        if room < height * dpi * MIN_SIZE {
            assert!(visible < height);
            let last = rect(0.0, height - LINE_H, W, height);
            let offset = reveal_offset(last, 0.0, visible, 0.0, height - visible);
            let moved = shifted_y(last, -offset);
            assert_eq!(clip_y(moved, 0.0, visible), Some(moved));
        } else {
            assert_eq!(visible, height);
        }
    }
}

#[test]
fn the_panel_has_its_nominal_width_with_room() {
    // At the default text size, with room: 328 DIP wide, 72 DIP cells.
    let size = panel_size(1920.0, 1040.0, 1.0, rest_n(0), rest_n(0));
    assert_eq!(size, BASE_SIZE);
    assert_eq!((W * size, UNIT * size), (328.0, 72.0));
    // The same dimensions scale at 200% when there is room.
    let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
    let (p, size) = place(r(2400, 0, 2440, 48), r(0, 48, 2560, 2080), 2.0, 1.0, rest_n(0), rest_n(0), false, false);
    assert_eq!(size, BASE_SIZE);
    assert_eq!((p.right - p.left, p.bottom - p.top), (px(W, 2.0, size), px(rest_n(0), 2.0, size)));
}

#[test]
fn the_panel_reserves_the_media_height_at_each_reviewed_work_area() {
    let mut r = Remote::default();
    let rest = |r: &Remote| layout(&shape_of(r, None, 0)).rest;
    let basic = rest(&r);
    r.brightness = Some(Some(60));
    let with_brightness = rest(&r);
    r.studio = Some("camera".into());
    r.hotspot = RadioView::On;
    r.displays = Displays::Many(None);
    let heights = [basic, with_brightness, rest(&r)];
    assert_eq!(heights, [408.0, 488.0, 568.0]);
    for (width, height, dpi, expected) in [
        (1920.0, 1048.0, 1.0, [1.0, 1.0, 1.0]),
        (1920.0, 1168.0, 1.5, [1.0, 1.0, 1.0]),
        (2560.0, 1408.0, 1.25, [1.0, 1.0, 1.0]),
        (1366.0, 736.0, 1.0, [1.0, 1.0, 0.9718]),
    ] {
        let actual = heights.map(|h| panel_size(width / dpi, height / dpi, 1.0, h, h));
        for (value, expected) in actual.into_iter().zip(expected) {
            assert!((value - expected).abs() < 0.0001, "{width}x{height}@{dpi}: {value}");
        }
    }
}

#[test]
fn the_panel_keeps_its_proportion_on_every_monitor() {
    let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
    let rest = rest_n(0);
    // The same screen in DIP at 100, 150 and 200 %: the same size, the
    // same share of the screen.
    let mut shares = Vec::new();
    for scale in [1.0f32, 1.5, 2.0] {
        let (w, h) = ((1280.0 * scale) as i32, (800.0 * scale) as i32);
        let bar = (24.0 * scale) as i32;
        let (p, size) = place(r(w - 60, 0, w - 20, bar), r(0, bar, w, h), scale, 1.0, rest, rest, false, false);
        assert_eq!(size, panel_size(1280.0, 776.0, 1.0, rest, rest), "{scale}");
        shares.push(((p.right - p.left) as f32 / w as f32, (p.bottom - p.top) as f32 / (h - bar) as f32));
    }
    for s in &shares[1..] {
        assert!((s.0 - shares[0].0).abs() < 0.002 && (s.1 - shares[0].1).abs() < 0.002, "{shares:?}");
    }
    // A short screen: the panel at rest is held to REST_SHARE_H of the
    // work area's height, shrunk whole.
    let size = panel_size(1280.0, 500.0, 1.0, rest, rest);
    assert!(size < BASE_SIZE);
    assert!((rest * size - 500.0 * REST_SHARE_H).abs() < 0.01, "{}", rest * size);
    assert_eq!(panel_size(1280.0, 500.0, 1.0, rest, rest + 40.0), size);
    // A narrow screen: held to REST_SHARE_W of its width.
    let size = panel_size(560.0, 1400.0, 1.0, rest, rest);
    assert!((W * size - 560.0 * REST_SHARE_W).abs() < 0.01, "{}", W * size);
    // Smaller still: never under MIN_SIZE...
    let size = panel_size(400.0, 480.0, 1.0, rest, rest);
    assert_eq!(size, MIN_SIZE);
    // ...unless the work area itself is smaller than that.
    let tall = rest + 500.0;
    let size = panel_size(400.0, 300.0, 1.0, rest, tall);
    assert!(tall * size <= 300.0 + 0.01 && W * size <= 400.0);
}

#[test]
fn the_text_size_grows_the_panel_whole_within_the_clamp() {
    let rest = rest_n(0);
    for (i, text) in crate::fonts::TEXT_SCALES.into_iter().enumerate() {
        let size = panel_size(1920.0, 1600.0, text, rest, rest);
        assert!((size - BASE_SIZE * text).abs() < 1e-6, "size {i}");
    }
    // Large text on a short screen: no bigger than the clamp allows.
    let large = crate::fonts::TEXT_SCALES[2];
    assert_eq!(panel_size(1280.0, 480.0, large, rest, rest), panel_size(1280.0, 480.0, 1.0, rest, rest));
}

#[test]
fn removable_strips_count_in_the_panel_at_rest() {
    // The panel at rest is the grid with what is playing and the
    // removable strips shown, no list; every layout with the same strips
    // is held to it, whatever plays or is open.
    for n in 0..=3 {
        assert_eq!(layout(&shape(true, n)).height, rest_n(n), "{n}");
        for media in [false, true] {
            for section in [None, Some((Section::Sound, 300.0))] {
                assert_eq!(layout(&Shape { section, ..shape(media, n) }).rest, rest_n(n), "{n} {media} {section:?}");
            }
        }
    }
    let lay = layout(&shape(true, 2));
    assert_eq!(lay.height - rest_n(0), 2.0 * (UNIT + GAP));
    let size = panel_size(1366.0, 700.0, 1.0, lay.rest, lay.height);
    assert!((lay.height * size - 700.0 * REST_SHARE_H).abs() < 0.01);
    assert!(size > MIN_SIZE);
}

#[test]
fn the_same_height_with_other_removable_strips_is_sized_again() {
    // Adding a removable device changes the size constraints while the list stays open.
    let r = |l, t, rr, b| RECT { left: l, top: t, right: rr, bottom: b };
    let work = r(0, 28, 1366, 648);
    let button = r(1300, 0, 1340, 28);
    let before = layout(&Shape { section: Some((Section::Sound, 520.0)), ..shape(true, 0) });
    let after = layout(&Shape { section: Some((Section::Sound, 520.0)), ..shape(true, 1) });
    assert_eq!((before.height, after.height), (520.0, 520.0));
    assert_eq!(after.rest - before.rest, UNIT + GAP);
    let (rect, size) = place(button, work, 1.0, 1.0, before.rest, before.height, false, false);
    let (want_rect, want_size) = place(button, work, 1.0, 1.0, after.rest, after.height, false, false);
    assert!(want_size < size, "{want_size} {size}");
    let now = (before.height, rect, size);
    assert!(refits(false, now, (after.height, want_rect, want_size)));
    // Nothing changed: nothing to fit (unless forced).
    assert!(!refits(false, now, now));
    assert!(refits(true, now, now));
    assert!(refits(false, now, (before.height + 1.0, rect, size)));
}

#[test]
fn the_panel_at_rest_keeps_its_share_with_removable_strips() {
    let works = [(1920.0, 1040.0), (1366.0, 740.0), (1280.0, 692.0), (1024.0, 600.0), (683.0, 480.0), (800.0, 400.0), (683.0, 356.0)];
    let sections = [Some((Section::Project, 200.0)), Some((Section::Sound, 640.0))];
    let mut held = 0;
    for (w, h) in works {
        for n in 0..=3 {
            for media in [false, true] {
                let closed = layout(&shape(media, n));
                let size = panel_size(w, h, 1.0, closed.rest, closed.height);
                // Inside the work area always.
                assert!(closed.height * size <= h + 0.01 && W * size <= w + 0.01, "{w}x{h} {n} {media}");
                // Within 75 % (and 50 % of the width) wherever it can stay
                // MIN_SIZE that small.
                let fits = (h * REST_SHARE_H / rest_n(n)).min(w * REST_SHARE_W / W) >= MIN_SIZE;
                if fits {
                    held += 1;
                    assert!(closed.height * size <= h * REST_SHARE_H + 0.01, "{w}x{h} {n} {media}: {}", closed.height * size);
                    assert!(W * size <= w * REST_SHARE_W + 0.01, "{w}x{h} {n} {media}");
                } else {
                    assert!(size <= MIN_SIZE + 1e-3, "{w}x{h} {n} {media}");
                }
                // A list does not shrink the panel again: only the work
                // area itself holds what is open.
                for section in sections {
                    let open = layout(&Shape { section, ..shape(media, n) });
                    let s2 = panel_size(w, h, 1.0, open.rest, open.fit);
                    assert_eq!(s2, size.min(h / open.fit), "{w}x{h} {n} {media} {section:?}");
                    assert!(open.height * s2 <= h + 0.01, "{w}x{h} {n} {media} {section:?}");
                }
            }
        }
    }
    assert!(held > 0);
}

#[test]
fn the_focus_comes_back_to_what_went_into_the_list() {
    let dev = Hit::Device(Flow::Output, 3);
    for (h, card) in [(dev, Hit::Outputs), (Hit::App(1), Hit::Outputs), (Hit::SoundSettings, Hit::Outputs), (Hit::Mode(Topology::Extend), Hit::Project)] {
        assert_eq!(focus_home(Some(h), None, |_| false), Some(card), "{h:?}");
        // Still shown: it stays.
        assert_eq!(focus_home(Some(h), None, |_| true), Some(h), "{h:?}");
    }
    // The back button goes to what went into the list it was in.
    assert_eq!(focus_home(Some(Hit::Back), Some(Section::Sound), |_| false), Some(Hit::Outputs));
    assert_eq!(focus_home(Some(Hit::Back), Some(Section::Project), |_| false), Some(Hit::Project));
    // Anything else stays.
    assert_eq!(focus_home(Some(Hit::Prev), None, |_| false), Some(Hit::Prev));
    assert_eq!(focus_home(None, None, |_| false), None);
    assert_eq!(section_card(Section::Sound), Hit::Outputs);
}

#[test]
fn screen_readers_hear_only_of_a_focus_they_can_find() {
    let tree = |shown: &[Hit], focus: Option<Hit>| Tree {
        role: Role::Group,
        name: "cc".into(),
        nodes: shown.iter().map(|h| Node::new(hit_key(*h), Role::Button, "x", RECT::default())).collect(),
        focus: reader_focus(focus, shown),
        focusable: true,
        active: true,
        focus_in: None,
    };
    let dev = Hit::Device(Flow::Output, 3);
    // In the list: the focused device is published.
    let list = tree(&[Hit::Back, dev], Some(dev));
    assert_eq!(list.focus, Some(hit_key(dev)));
    // Back on the grid: the focus on the output button, told anew.
    let grid = tree(&[Hit::Volume, Hit::Outputs], focus_home(Some(dev), None, |_| false));
    assert_eq!(grid.focus, Some(hit_key(Hit::Outputs)));
    assert!(a11y::diff(&list, &grid).contains(&a11y::Change::Focus(hit_key(Hit::Outputs))));
    // Whatever is shown, the focus told is one of the nodes.
    for (shown, f) in [(vec![], Some(dev)), (vec![Hit::Volume], Some(dev)), (vec![dev], Some(dev)), (vec![dev], None)] {
        let t = tree(&shown, f);
        assert!(t.focus.as_ref().is_none_or(|k| t.nodes.iter().any(|n| &n.key == k)), "{shown:?} {f:?}");
    }
}

#[test]
fn the_next_icon_read_waits_on_the_reader_s_word_not_its_thread() {
    let (said_tx, said_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (end_tx, end_rx) = std::sync::mpsc::channel::<()>();
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    let thread = std::thread::spawn(move || {
        // Reading until told to finish.
        let _ = go_rx.recv();
        d.store(true, Ordering::SeqCst);
        // The ask for a new read...
        let _ = said_tx.send(());
        // ...answered before this thread returns.
        let _ = end_rx.recv();
    });
    let mut icons = Icons { reader: Some(Reader { quit: Arc::new(AtomicBool::new(false)), done, thread }), ..Default::default() };
    assert!(icons.reading());
    go_tx.send(()).unwrap();
    said_rx.recv().unwrap();
    // The thread has not returned, yet the next read may start.
    assert!(!icons.reader.as_ref().unwrap().thread.is_finished());
    assert!(!icons.reading());
    // And the reader is let go without waiting on it.
    icons.stop();
    assert!(icons.reader.is_none() && !icons.reading());
    end_tx.send(()).unwrap();
}

#[test]
fn two_lines_sit_around_their_middle() {
    for cy in [36.0f32, 300.0] {
        let (title, value) = two_lines(cy);
        assert_eq!(value - title, TITLE_H);
        assert!(((title + value + ARTIST_H) / 2.0 - cy).abs() < 0.01, "centered");
        assert!(title >= cy - UNIT / 2.0 + 8.0, "inside a 72 strip");
    }
}

#[test]
fn display_modes_follow_the_displays() {
    use Topology::*;
    // One display connected: no card, whatever the mode.
    assert_eq!(displays_of(0, None), Displays::Single);
    assert_eq!(displays_of(1, Some(Internal)), Displays::Single);
    assert_eq!(displays_of(2, Some(Extend)), Displays::Many(Some(Extend)));
    assert_eq!(displays_of(2, None), Displays::Many(None));
    // The mode from the active paths: (source, internal target).
    assert_eq!(active_topology(&[(1, true)], true), Some(Internal));
    assert_eq!(active_topology(&[(2, false)], true), Some(External));
    assert_eq!(active_topology(&[(2, false)], false), None, "a desktop's one screen: no PC screen to tell from");
    assert_eq!(active_topology(&[(1, true), (2, false)], true), Some(Extend));
    assert_eq!(active_topology(&[(1, true), (1, false)], true), Some(Clone));
    assert_eq!(active_topology(&[(1, false), (1, false), (1, false)], false), Some(Clone));
    assert_eq!(active_topology(&[(1, true), (1, false), (3, false)], true), None, "mixed: no check");
    assert_eq!(active_topology(&[], true), None);
    // Saved Extend, an app's temporary Duplicate active: Duplicate is
    // checked and picking Extend switches; and the other way round.
    let now = displays_of(2, active_topology(&[(1, true), (1, false)], true));
    assert_eq!(now, Displays::Many(Some(Clone)));
    assert!(mode_switches(now, Extend) && !mode_switches(now, Clone));
    let now = displays_of(2, active_topology(&[(1, true), (2, false)], true));
    assert!(mode_switches(now, Clone) && !mode_switches(now, Extend));
    // Unknown: no check, every pick applies.
    for m in Topology::ALL {
        assert!(mode_switches(Displays::Many(None), m));
    }
    assert_eq!(Topology::ALL.map(Topology::sdc), [1, 2, 4, 8], "SDC_TOPOLOGY_*");
}

#[test]
fn screen_readers_get_the_lines_where_they_are_drawn() {
    // Three controls; a line after the first (an error under a list),
    // two after the last (the track's title and artist).
    assert_eq!(drawing_order(3, &[1, 3, 3]), vec![(false, 0), (true, 0), (false, 1), (false, 2), (true, 1), (true, 2)]);
    assert_eq!(drawing_order(0, &[0]), vec![(true, 0)]);
    assert_eq!(drawing_order(2, &[]), vec![(false, 0), (false, 1)]);
    assert_eq!(drawing_order(1, &[5]), vec![(false, 0), (true, 0)]);
}

#[test]
fn a_switching_hotspot_is_followed_until_it_settles() {
    let (on, off, moving) = ((RadioView::On, false), (RadioView::Off, false), (RadioView::Off, true));
    // Switching when the panel opened (nobody's switch of ours): read
    // again, then shown as it settled, clickable again.
    assert_eq!(settle(moving, None, false), None);
    assert_eq!(settle(on, None, false), Some((RadioView::On, false)));
    // Out of time while still switching: shown as read, not busy.
    assert_eq!(settle(moving, None, true), Some((RadioView::Off, false)));
    // Our switch to on: accepted, still switching, then on.
    assert_eq!(settle(moving, Some(true), false), None);
    assert_eq!(settle(off, Some(true), false), None, "accepted, not shown yet");
    assert_eq!(settle(on, Some(true), false), Some((RadioView::On, false)));
    // Never shows: a failure, as Windows has it.
    assert_eq!(settle(off, Some(true), true), Some((RadioView::Off, true)));
    assert_eq!(settle(moving, Some(false), true), Some((RadioView::Off, true)));
    // The connection went away: no toggle; a switch of ours failed.
    assert_eq!(settle((RadioView::Missing, false), Some(true), false), Some((RadioView::Missing, true)));
    assert_eq!(settle((RadioView::Missing, false), None, false), Some((RadioView::Missing, false)));
    // Bounded: SETTLE_FOR of looks every SETTLE_EVERY.
    assert!(SETTLE_FOR.as_millis() / SETTLE_EVERY.as_millis() <= 30);
}

#[test]
fn hotspot_states() {
    assert_eq!(hotspot_of(Some(TetheringOperationalState::On)), (RadioView::On, false));
    assert_eq!(hotspot_of(Some(TetheringOperationalState::Off)), (RadioView::Off, false));
    assert_eq!(hotspot_of(Some(TetheringOperationalState::InTransition)), (RadioView::Off, true));
    assert_eq!(hotspot_of(Some(TetheringOperationalState::Unknown)), (RadioView::Missing, false));
    assert_eq!(hotspot_of(None), (RadioView::Missing, false));
}

#[test]
fn every_control_has_its_own_key() {
    let hits = [
        Hit::WifiToggle,
        Hit::WifiName,
        Hit::Quick(Quick::Airplane),
        Hit::Project,
        Hit::Outputs,
        Hit::Mode(Topology::Extend),
        Hit::Mode(Topology::Clone),
        Hit::Device(Flow::Output, 1),
        Hit::Device(Flow::Input, 1),
        Hit::App(1),
        Hit::App(2),
        Hit::Mute,
        Hit::Volume,
    ];
    let keys: std::collections::HashSet<String> = hits.iter().map(|h| hit_key(*h)).collect();
    assert_eq!(keys.len(), hits.len());
    assert_eq!(mix_key(7, &["a"]), mix_key(7, &["a"]));
    assert_ne!(mix_key(7, &["a"]), mix_key(8, &["a"]));
}

#[test]
fn a_shared_session_belongs_to_no_single_app() {
    use windows::Win32::Foundation::{E_FAIL, S_OK};
    assert_eq!(sole_process(S_OK, 42), Some(42));
    // A success too, with the first process's ID: still no single app.
    assert_eq!(sole_process(NO_SINGLE_PROCESS, 42), None);
    assert_eq!(sole_process(S_OK, 0), None, "the system's");
    assert_eq!(sole_process(E_FAIL, 42), None);
}

#[test]
fn mixer_rows_are_keyed_by_process_and_session() {
    let s = |pid: u32, id: &str| (pid, id.to_string());
    // Two sessions of one process are one row; another process its own.
    let rows = mix_rows(&[s(10, "a|10"), s(11, "b|11"), s(10, "c|10")]);
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].1, rows[0].2.clone()), (10, vec![0, 2]));
    assert_eq!((rows[1].1, rows[1].2.clone()), (11, vec![1]));
    assert_eq!(rows[0].0, mix_key(10, &["a|10", "c|10"]));
    // The order the sessions are found in does not move the key.
    assert_eq!(mix_key(10, &["c|10", "a|10"]), rows[0].0);
    // The same process ID with sessions of its own (another process
    // given the old one's ID): another row, so nothing of the old one
    // (name, focus, drag, a write on its way) reaches it.
    let reused = mix_rows(&[s(10, "z|10")]);
    assert_ne!(reused[0].0, rows[0].0);
    assert!(mix_rows(&[]).is_empty());
}

#[test]
fn names_of_rows_gone_are_dropped() {
    let (old, other) = (mix_key(10, &["a|10"]), mix_key(11, &["b|11"]));
    let mut names: std::collections::HashMap<u64, &str> = [(old, "App A"), (other, "App B")].into_iter().collect();
    // Process 10 ended and its ID went to a new process.
    let new = mix_key(10, &["z|10"]);
    prune_rows(&mut names, &[other, new]);
    assert_eq!(names.len(), 1);
    assert_eq!(names.get(&other), Some(&"App B"));
    assert!(!names.contains_key(&old) && !names.contains_key(&new), "the new process is named afresh");
}

#[test]
fn a_read_begun_before_a_write_does_not_undo_it() {
    let mut row = app(10, true);
    let k = row.key;
    let mut r = Remote::default();
    // The panel shows 80 at once: write 1.
    row.level = 80;
    r.apps = Some(vec![row.clone(), app(11, true)]);
    r.app_seq = 1;
    r.app_wrote.insert(k, 1);
    // A read the worker began before carrying it out still finds 50:
    // the row keeps 80, the other row takes what was read.
    let mut stale = vec![app(10, true), app(11, true)];
    stale[0].level = 50;
    stale[1].level = 30;
    take_read(&mut r, stale, 0);
    let apps = r.apps.clone().unwrap();
    assert_eq!((apps[0].level, apps[1].level), (80, 30));
    assert!(r.app_wrote.contains_key(&k), "still on its way");
    // Carried out; its own change event (MIX_CTX) reads 80.
    let mut fresh = vec![app(10, true), app(11, true)];
    fresh[0].level = 80;
    take_read(&mut r, fresh, 1);
    assert_eq!(r.apps.as_ref().unwrap()[0].level, 80);
    assert!(r.app_wrote.is_empty(), "forgotten once carried out");
    // Someone else changes it later: shown as read.
    let mut later = vec![app(10, true)];
    later[0].level = 20;
    take_read(&mut r, later, 1);
    assert_eq!(r.apps.as_ref().unwrap()[0].level, 20);
    // merge_apps alone: a write newer than the read keeps the mute too.
    let mut now = app(10, true);
    now.muted = false;
    let mut read = app(10, true);
    read.muted = true;
    let wrote: std::collections::HashMap<u64, u64> = [(k, 3)].into_iter().collect();
    assert!(!merge_apps(vec![read.clone()], Some(&[now.clone()]), &wrote, 2)[0].muted);
    assert!(merge_apps(vec![read], Some(&[now]), &wrote, 3)[0].muted);
}

#[test]
fn the_icon_reader_stops_between_icons_once_closed() {
    let quit = AtomicBool::new(false);
    let mut kept = Vec::new();
    let n = read_icons(
        vec!["a.exe".into(), "b.exe".into(), "c.exe".into()],
        &quit,
        |exe| {
            // The panel closes while the first icon is read.
            if exe == "a.exe" {
                quit.store(true, Ordering::SeqCst);
            }
            None
        },
        |exe, _| kept.push(exe),
    );
    assert_eq!(n, 1);
    assert_eq!(kept, vec!["a.exe".to_string()]);
    // Not closed: all of them.
    let open = AtomicBool::new(false);
    assert_eq!(read_icons(vec!["a.exe".into(), "b.exe".into()], &open, |_| None, |_, _| {}), 2);
}

#[test]
fn one_icon_reader_at_a_time_and_the_next_is_nudged() {
    let mut turn = IconTurn { busy: false, waiting: None };
    let (a, _ra) = channel();
    let (b, rb) = channel();
    assert!(turn.take(&a), "free: ours");
    // Another panel's worker while it reads: waits, to be nudged.
    assert!(!turn.take(&b));
    let next = turn.free().expect("the waiting worker");
    let _ = next.send(AudioCmd::Apps);
    assert!(matches!(rb.try_recv(), Ok(AudioCmd::Apps)));
    assert!(turn.free().is_none(), "nudged once");
    assert!(turn.take(&b), "free again");
}

#[test]
fn the_time_stops_ticking_at_the_end() {
    let now = Instant::now();
    let tl = |pos: i64| Timeline { start_ms: 0, end_ms: 10_000, pos_ms: pos, at: now, rate: 1.0, seekable: false, seek_min_ms: 0, seek_max_ms: 0 };
    assert_eq!(tick_in(&tl(4_500), true, now), Some(500));
    assert_eq!(tick_in(&tl(4_500), false, now), None, "paused");
    assert_eq!(tick_in(&tl(10_000), true, now), None, "at the end");
    // Played on past the end since the report (the player still says
    // Playing): no more wake-ups.
    assert_eq!(tick_in(&tl(9_900), true, now + Duration::from_millis(500)), None);
    let mut endless = tl(0);
    endless.end_ms = 0;
    assert_eq!(tick_in(&endless, true, now), None, "no bar");
}

#[test]
fn screen_readers_get_the_headings_and_each_device_s_direction() {
    let _l = crate::i18n::test_lock("en");
    let r = Remote { outputs: Some(vec![ep("out", true)]), inputs: Some(vec![ep("in", true)]), apps: Some(vec![app(10, true), app(12, true)]), ..Default::default() };
    let heads: Vec<&str> = section_cards(Section::Sound, &r)
        .concat()
        .into_iter()
        .filter_map(|l| match l {
            Line::Head(k) => Some(head_node(k)),
            _ => None,
        })
        .collect();
    assert_eq!(heads, vec!["text:head_output", "text:head_input", "text:head_app_volume"]);
    // An output and an input of one headset are told apart.
    assert_eq!(device_name(Flow::Output, "Headset"), "Output: Headset");
    assert_eq!(device_name(Flow::Input, "Headset"), "Input: Headset");
}

#[test]
fn strings_exist() {
    let _l = crate::i18n::test_lock("ko");
    for k in [
        "control.title",
        "control.accessibility",
        "control.accessibility_settings",
        "control.access.magnifier",
        "control.access.color_filters",
        "control.access.narrator",
        "control.access.mono_audio",
        "control.access.live_captions",
        "control.access.sticky_keys",
        "control.settings",
        "control.battery",
        "control.wifi_list",
        "control.bluetooth_list",
        "control.bluetooth_settings",
        "control.wifi_settings",
        "control.connected",
        "control.connect",
        "control.disconnect",
        "control.studio_effects",
        "control.scroll_up",
        "control.scroll_down",
        "control.wifi",
        "control.bluetooth",
        "control.on",
        "control.off",
        "control.not_connected",
        "control.radio.missing",
        "control.radio.denied",
        "control.radio.locked",
        "control.audio.switching",
        "control.audio.failed",
        "control.audio.gone",
        "control.audio.open_settings",
        "control.dark_mode",
        "control.screen_cast",
        "control.cast.find",
        "control.brightness",
        "control.volume",
        "control.app_volume",
        "control.muted",
        "control.no_output",
        "control.now_playing",
        "control.output",
        "control.input",
        "control.device_flow",
        "control.no_default",
        "control.unknown_device",
        "control.airplane",
        "control.night_light",
        "control.dnd",
        "control.energy_saver",
        "control.hotspot",
        "control.keep_awake",
        "control.project",
        "control.project.internal",
        "control.project.clone",
        "control.project.extend",
        "control.project.external",
        "control.in_settings",
        "control.opens_settings",
        "a11y.topbar.network",
        "island.prev",
        "island.next",
        "island.play",
        "island.pause",
    ] {
        assert!(crate::i18n::get(k).is_some(), "{k}");
    }
    assert_eq!(tf("control.percent", &[("p", "40")]), "40%");
}


/// The 2026-11-11 review (P2): right to left, the thick slider's surface
/// drew its fill, glyph and mute from its left while the input reads the
/// slider from its right (pointer x mirrored back, mute hit at the
/// layout's start). Now everything inside is mirrored with it: the glyph's
/// middle on screen is inside the mute button's hit, the mute plate is
/// the hit, the fill's far end on screen is where the pointer sets the
/// value, and the value's text sits at the other end, all in both
/// directions.
#[test]
fn the_slider_track_and_mute_hit_mirror_together() {
    use crate::i18n::mirror;
    let sr = rect(PAD + STRIP_IN, 200.0, W - PAD - STRIP_IN - ROUND_L - GAP, 200.0 + CAPSULE);
    let mute = mute_in(sr);
    let (start, end) = track_span(sr);
    for rtl in [false, true] {
        let (ml, mr) = mirror::edges(rtl, mute.left, mute.right, 0.0, W);
        let glyph = mirror::x(rtl, (mute.left + mute.right) / 2.0, 0.0, W);
        assert!(glyph > ml && glyph < mr);
        for v in [0.0f32, 0.3, 1.0] {
            let thumb = mirror::x(rtl, start + (end - start) * v, 0.0, W);
            assert!((cap_value(mirror::x(rtl, thumb, 0.0, W), sr) - v).abs() < 1e-4);
        }
    }
}

/// The 2026-11-11 review (P2): a failed device switch took the panel into
/// the sound list on every update while the failure stayed, so leaving it
/// (back, Esc) was undone by the next unrelated update. Each failure leads
/// in once.
#[test]
fn a_failed_switch_leads_into_the_list_once() {
    let mut shown = 0u64;
    let mut open = None;
    // No failure: nothing.
    assert!(!fail_leads_in(None, open, &mut shown));
    // Failure 1 comes on the grid: in.
    assert!(fail_leads_in(Some(1), open, &mut shown));
    open = Some(Section::Sound);
    assert!(!fail_leads_in(Some(1), open, &mut shown));
    // Back to the grid; app sessions change, the failure is still there:
    // the grid stays.
    open = None;
    for _ in 0..3 {
        assert!(!fail_leads_in(Some(1), open, &mut shown), "left after it was shown");
    }
    // A new failure: in again.
    assert!(fail_leads_in(Some(2), open, &mut shown));
    // One that comes while the list is open is shown there: leaving after
    // it stays left.
    assert!(!fail_leads_in(Some(3), Some(Section::Sound), &mut shown));
    assert!(!fail_leads_in(Some(3), None, &mut shown));
    // One that comes while Project's list is open waits for the grid.
    assert!(!fail_leads_in(Some(4), Some(Section::Project), &mut shown));
    assert!(fail_leads_in(Some(4), None, &mut shown));
    // The failure line stays in the list (fail_for), only the leading in
    // is once.
    let r = Remote { audio_fail: Some((Flow::Output, "Headphones".into())), audio_fail_seq: 4, ..Default::default() };
    assert_eq!(fail_for(&r, Flow::Output), Some("Headphones"));
    // Each failure the worker reports counts.
    let audio = include_str!("audio.rs");
    assert_eq!(audio.matches("r.audio_fail_seq += 1;").count(), 2);
}

/// The 2026-11-11 review (P2): an eject on its way, or done ("Safe to
/// remove"), took no input and so was in no list the reader's tree is
/// made from: the device left the tree with its status. Now it is read
/// as a disabled control in every state, and takes input only when it
/// can be asked.
#[test]
fn an_eject_on_its_way_or_done_is_still_read() {
    use crate::removable::model::{Note, Refusal};
    let strip = rect(PAD, 300.0, W - PAD, 372.0);
    let b = eject_columns(strip).3;
    let h = Hit::Eject(7);
    for (note, input) in [(Note::Ready, true), (Note::Ejecting, false), (Note::Safe, false), (Note::Refused(Refusal::Failed), true), (Note::Refused(Refusal::InUse(None)), true)] {
        assert_eq!(eject_ready(&note), input, "{note:?}");
        // As the grid records it: a control before it, a text line, the
        // device's eject, a control after.
        let mut hits = vec![(rect(0.0, 0.0, 10.0, 10.0), Hit::Dark)];
        let mut inert = Vec::new();
        let texts = [hits.len()];
        add_control(&mut hits, &mut inert, b, h, eject_ready(&note));
        hits.push((rect(0.0, 400.0, 10.0, 410.0), Hit::Cast));
        // Input finds it only when it can be asked.
        assert_eq!(hits.iter().any(|x| x.1 == h), input, "{note:?}");
        // The reader's tree has it in every state, where it is drawn.
        let at: Vec<usize> = inert.iter().map(|x| x.0).collect();
        let order = reading_order(hits.len(), &texts, &at);
        let read: Vec<Hit> = order
            .iter()
            .filter_map(|r| match *r {
                Read::Hit(i) => Some(hits[i].1),
                Read::Inert(i) => Some(inert[i].2),
                Read::Text(_) => None,
            })
            .collect();
        assert_eq!(read, vec![Hit::Dark, h, Hit::Cast], "{note:?}");
        assert_eq!(order.iter().filter(|r| matches!(r, Read::Inert(_))).count(), usize::from(!input), "{note:?}");
        assert_eq!(order[1], Read::Text(0), "{note:?}: the line where it was");
    }
    // The tree reads the inert controls with their node (disabled: the
    // same eject_ready) and status.
    let reader = include_str!("reader.rs");
    assert!(reader.contains("reading_order(self.hits.len(), &text_at, &inert_at)") && reader.contains("Read::Inert(i)"));
    assert!(reader.contains("let ready = row.is_some_and(|x| eject_ready(&x.note));"));
    assert_eq!(reading_order(2, &[], &[1]), vec![Read::Hit(0), Read::Inert(0), Read::Hit(1)]);
    assert_eq!(reading_order(1, &[1], &[1, 5]), vec![Read::Hit(0), Read::Text(0), Read::Inert(0), Read::Inert(1)]);
}

/// Final audit 5: the sound list kept the grid's height (its lower half
/// empty) and the back button's focus ring all but touched the plate's
/// corner. The list is as tall as it is and the panel glides there (its
/// top at the bar, the window larger meanwhile: popup::chrome_live); the
/// ring keeps out of the panel's padding and off the title.
#[test]
fn a_list_is_its_own_height_and_the_panel_glides_there() {
    let mut sound = Remote::default();
    sound.outputs = Some(vec![ep("a", false), ep("b", true)]);
    sound.inputs = Some(vec![ep("m", true), ep("n", false)]);
    let cards = section_cards(Section::Sound, &sound);
    let lay = layout(&Shape { section: Some((Section::Sound, sub_h(&cards))), tiles: grid_tiles(&sound), ..Default::default() });
    assert_eq!(lay.height, sub_h(&cards));
    assert!(lay.height < lay.grid_h, "the sound list is shorter than the grid here");
    // Only the height changing, under a bar at the top, with full motion:
    // a glide. Anything else is fitted at once.
    let r = |t: i32, b: i32| RECT { left: 1500, top: t, right: 1828, bottom: b };
    let now = (600.0, r(40, 640), 1.0);
    assert!(glides(true, false, now, (300.0, r(40, 340), 1.0)));
    assert!(glides(true, false, now, (700.0, r(40, 740), 1.0)));
    assert!(!glides(false, false, now, (300.0, r(40, 340), 1.0)), "reduced motion");
    assert!(!glides(true, true, now, (300.0, r(340, 640), 1.0)), "a bar at the bottom: its edge moves");
    assert!(!glides(true, false, now, (300.0, r(40, 340), 0.9)), "another size");
    assert!(!glides(true, false, now, now), "nothing to glide");
    // The back button's ring stays out of the panel's padding (the other
    // controls' rings reach at most RING_OUT out of them, all PAD in).
    let (head, _) = sub_plan(&cards);
    let b = back_in(head);
    let (ring, _) = crate::gfx::parts::focus_ring_rect(b, ROUND_S / 2.0, 1.0);
    let reach = crate::theme::metrics::size::FOCUS_W / 2.0;
    assert!(ring.left - reach >= PAD - 1e-3 && ring.top - reach >= PAD - 1e-3, "{ring:?}");
    assert_eq!(SUB_HEAD_H, ROUND_S + RING_OUT * 2.0);
    // The panel builds its plate on the live height and glides it.
    let src = include_str!("../control.rs");
    assert!(src.contains("crate::popup::chrome_live(") && src.contains("spring_prop(&self.gfx.compositor, &self.v.h, \"h\""));
}

/// Final review 2 (2026-11-11): into a shorter list the plate glides and
/// the window shrinks after it (a timer). A value coming meanwhile at the
/// same height (a sound device's) went through resize, which cancelled
/// that timer and, nothing to fit, left the window taller than its plate
/// for good. Event by event through resize_step: the same height keeps
/// the shrink waiting; the plate's arrival shrinks the window; a new
/// glide while one waits covers both plates and waits again; fitting at
/// once ends any wait. The window never ends taller than its panel.
#[test]
fn a_same_height_update_leaves_the_shrink_to_come() {
    let r = |b: i32| RECT { left: 1500, top: 40, right: 1828, bottom: b };
    let (tall, short, mid, taller) = ((600.0, r(640), 1.0), (300.0, r(340), 1.0), (500.0, r(540), 1.0), (700.0, r(740), 1.0));
    let step = |fit: &mut WinFit, now: (f32, RECT, f32), want: (f32, RECT, f32)| resize_step(fit, false, true, false, now, want);

    // Into the short list: a glide, the window kept tall, a shrink waits.
    let mut fit = WinFit::at(tall.1);
    assert_eq!(step(&mut fit, tall, short), (Fit::Glide, WinStep { cancel: false, to: None, wait: true }));
    assert_eq!(fit, WinFit { win: tall.1, waiting: true });
    // The review's case: values at the same height, again and again,
    // before the plate arrives. Nothing is cancelled, nothing moved.
    for _ in 0..5 {
        assert_eq!(step(&mut fit, short, short), (Fit::Keep, WinStep::default()));
    }
    assert!(fit.waiting, "the shrink still comes");
    // The plate arrived: the window shrinks to the panel, once.
    assert_eq!(fit.arrived(short.1), Some(short.1));
    assert_eq!(fit, WinFit { win: short.1, waiting: false });
    assert_eq!(fit.arrived(short.1), None);
    assert_eq!(step(&mut fit, short, short), (Fit::Keep, WinStep::default()));

    // Quickly back and forth: tall -> short (waits) -> mid. The window
    // stays tall (the plate is still taller than mid on its way), the
    // old wait goes, a new one comes, and the window ends at mid.
    let mut fit = WinFit::at(tall.1);
    step(&mut fit, tall, short);
    assert_eq!(step(&mut fit, short, mid), (Fit::Glide, WinStep { cancel: true, to: None, wait: true }));
    assert_eq!(fit.win, tall.1);
    assert_eq!(step(&mut fit, mid, mid).1, WinStep::default());
    assert_eq!(fit.arrived(mid.1), Some(mid.1));
    // tall -> short (waits) -> taller than the window: grown now, no wait.
    let mut fit = WinFit::at(tall.1);
    step(&mut fit, tall, short);
    assert_eq!(step(&mut fit, short, taller), (Fit::Glide, WinStep { cancel: true, to: Some(taller.1), wait: false }));
    assert_eq!(fit, WinFit { win: taller.1, waiting: false });
    assert_eq!(fit.arrived(taller.1), None, "a timer cancelled too late does nothing");
    // Growing from rest: the window first, no wait.
    let mut fit = WinFit::at(short.1);
    assert_eq!(step(&mut fit, short, tall), (Fit::Glide, WinStep { cancel: false, to: Some(tall.1), wait: false }));

    // Fitted at once while a shrink waits (reduced motion, another size,
    // force): the wait ends and the window is where the panel is.
    for (full, want, force) in [(false, mid, false), (true, (500.0, r(540), 0.9), false), (true, short, true)] {
        let mut fit = WinFit::at(tall.1);
        step(&mut fit, tall, short);
        let (how, s) = resize_step(&mut fit, force, full, false, short, want);
        assert_eq!((how, s), (Fit::Now, WinStep { cancel: true, to: Some(want.1), wait: false }), "{full} {force}");
        assert_eq!(fit, WinFit { win: want.1, waiting: false });
    }
    // Nothing changed and nothing waits, yet the window is elsewhere: it
    // is fitted (never left taller than its panel).
    let mut fit = WinFit { win: tall.1, waiting: false };
    assert_eq!(step(&mut fit, short, short), (Fit::Keep, WinStep { to: Some(short.1), ..Default::default() }));

    // The panel moves its window only through these steps, and a same-
    // height update goes through them (no cancel of its own).
    let src = include_str!("../control.rs");
    assert!(src.contains("resize_step(&mut self.fit,"), "resize asks WinFit");
    let at = src.find("fn resize(&mut self, force: bool)").expect("resize");
    let end = src[at..].find("fn glide_to(").expect("glide_to") + at;
    assert!(!src[at..end].contains("motion::cancel"), "resize cancels nothing itself");
    assert_eq!(src.matches("crate::motion::cancel(").count(), 1, "the shrink is cancelled in window_step alone");
    assert_eq!(src.matches("self.move_window(").count(), 2, "window_step and shrink_arrived alone move the window");
}

/// Final audit 12: the output button beside the volume (the headphones)
/// had a disc on every finish but Flat, where its raised-tile fill was the
/// strip card's own. It is a chip, as the connections' discs on their
/// card: a disc that parts from the card on every finish, light and dark.
#[test]
fn the_output_button_is_a_disc_on_every_finish() {
    use crate::theme::{over, Finish};
    let src = include_str!("paint.rs");
    assert!(src.contains("Hit::Outputs, b, ROUND_L / 2.0, glyph, TILE_GLYPH, Face::Chip"));
    for f in [Finish::Flat, Finish::Glass] {
        for light in [false, true] {
            let mut raw = crate::theme::Theme::graphite();
            raw.finish = f;
            let theme = raw.resolve_fx(&crate::syslook::SysLook { light, ..Default::default() }, true);
            let tn = tones_of(&theme);
            let panel = Rgba { a: 255, ..theme.popup_tint() };
            let card = over(tn.tile, panel);
            let (disc, _) = chip_tone(&tn, Look { on: false, hover: false, pressed: false });
            let on_card = over(disc, card);
            let d = (on_card.r as i32 - card.r as i32).abs();
            assert!(d >= 6, "{f:?} light {light}: the disc {on_card:?} on the card {card:?}");
        }
    }
}

/// Ink on a fill of its own (an "on" disc, a slider's cap) was picked for
/// that fill and reads on it at 4.5:1 alone (DISC_CONTRAST) over every
/// look of the plate under it, on every finish, light and dark.
#[test]
fn glyphs_on_active_buttons_keep_their_contrast() {
    use crate::syslook::SysLook;
    use crate::theme::{contrast, over, Finish};
    let looks = [(false, false), (true, false), (true, true)];
    for light in [false, true] {
        for f in [Finish::Flat, Finish::Glass] {
            let mut raw = Theme::graphite();
            raw.finish = f;
            let t = raw.resolve_fx(&SysLook { light, ..Default::default() }, true);
            let tn = tones_of(&t);
            let panels = panel_extremes(&t);
            let reads_alone = |fill: Rgba, ink: Rgba, what: &str| {
                for (p, _) in &panels {
                    let ground = over(fill, *p);
                    let c = contrast(over(ink, ground), ground);
                    assert!(c >= DISC_CONTRAST, "{f:?} light {light} {what}: {c:.2} on {ground:?}");
                }
            };
            for (hover, pressed) in looks {
                let on = Look { on: true, hover, pressed };
                for (face, (fill, ink)) in [("raised", raised_tone(&tn, on)), ("chip", chip_tone(&tn, on))] {
                    let what = format!("{face} on, hover {hover} pressed {pressed}");
                    reads_alone(fill, ink, &what);
                }
            }
        }
    }
}

/// states-issues 2 (2026-11-11): a part (a disc, a thick slider, the art,
/// the position bar) on an odd DIP at 150 % stood on a half pixel and the
/// compositor drew it soft (the thick slider's edge read doubled). Placed
/// on whole device pixels at every scale; every part's SetOffset goes
/// through Panel::placed.
#[test]
fn parts_stand_on_whole_device_pixels() {
    for k in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
        for v in [0.0f32, 7.0, 13.0, 101.5, 333.333] {
            let p = super::parts::snap_dip(v, k) * k;
            assert!((p - p.round()).abs() < 1e-3, "{v} at {k}: {p}");
            assert!((super::parts::snap_dip(v, k) - v).abs() <= 0.5 / k + 1e-4);
        }
    }
    let src = include_str!("parts.rs");
    assert!(!src.contains("SetOffset(Vector3 { X: self.placed_x("), "every part placed by Panel::placed");
    assert_eq!(src.matches("let (x, y) = self.placed(").count(), 6);
}
