//! The Live Island's card drawn off screen for the promo film (tests only;
//! see crate::gfx::promo_kit): the card made as `open` makes it (its
//! window never shown) with made-up activities, and painted by its own
//! `paint` on a software target. The plate under it is the compositor's on
//! screen: the film draws it from `island.json`.

use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, LoadCursorW, RegisterClassExW, IDC_ARROW, WNDCLASSEXW};

use super::*;
use crate::gfx::promo_kit as kit;

/// What is live in the film: a song playing, a copy two thirds done.
pub(in crate::topbar) fn live() -> Vec<Activity> {
    let track = model::Track {
        title: kit::say("밤 산책", "Night Walk").into(),
        artist: kit::say("조용한 재생목록", "Quiet Playlist").into(),
        playing: true,
        can_prev: true,
        can_next: true,
        can_toggle: true,
        timeline: Some(model::Timeline { start_ms: 0, end_ms: 214_000, pos_ms: 83_000, at: Instant::now(), rate: 1.0, seekable: true, seek_min_ms: 0, seek_max_ms: 214_000 }),
        ..Default::default()
    };
    let job = model::Job { key: "promo-copy".into(), name: kit::say("파일", "Files").into(), files: true, ratio: 0.64, hwnd: 0 };
    vec![Activity::Media(track), Activity::Progress(vec![job])]
}

#[test]
#[ignore = "writes the promo film's pictures: cargo test -p dock promo_ -- --ignored"]
fn promo_island_card() {
    kit::each_lang(card_pictures);
}

fn card_pictures() {
    let stage = kit::stage();
    let mut looks = serde_json::Map::new();
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("NopBarIsland"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        for (folder, light) in kit::LOOKS {
            let theme = kit::theme(light);
            let data = Data { live: live(), icons: HashMap::new(), art: None, pip: false };
            // A pill on the main display to stand on (the window is never
            // shown).
            let shell = stage.gfx.compositor.CreateContainerVisual().unwrap().cast().unwrap();
            let pill = Pill { rect: RECT { left: 500, top: 2, right: 620, bottom: 30 }, scale: 1.0, parts: Vec::new(), shell };
            let mut card = Card::new(Request { gfx: stage.gfx.clone(), theme: theme.clone(), scale: 1.0, pill, bar: stage.hwnd, data, pinned: true }).expect("the card");
            // The picture's scale (hairlines on its pixels); the song's
            // own volume, as if its app had answered.
            card.scale = kit::scale();
            card.fit = 1.0;
            card.rtl = false;
            card.volume = Some((String::new(), 62, false));
            card.vol_answered = true;
            card.relayout();
            let cv = stage.canvas(W, card.height);
            card.paint(&cv.dc, &mut Painted::default()).expect("painted");
            cv.save(&format!("{folder}/island-card.png"));
            looks.insert(folder.into(), kit::panel_json(&theme, W, card.height, theme.radii().panel, crate::theme::Elevation::E3));
            let _ = DestroyWindow(card.hwnd);
        }
    }
    kit::save_json("island-card.json", &serde_json::json!({ "scale": kit::scale(), "looks": looks }));
    assert!(kit::out_dir().join("dark/island-card.png").is_file());
}
