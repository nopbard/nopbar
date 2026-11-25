//! The PC status panel drawn off screen for the promo film (tests only;
//! see crate::gfx::promo_kit): the panel made as `show` makes it (its
//! window never shown), given made-up readings, and painted by its own
//! `paint` on a software target. Three pictures, one for each state:
//! `stats.png` (calm), `stats-busy.png` (memory under pressure: the list
//! by memory, "Quit" on the app using the most) and `stats-very-busy.png`
//! (the CPU). The plate under it (blur, tint, border, shadow) is the
//! compositor's on screen: the film draws it from `stats.json`.
//!
//! The scenes (`Scene`, `staged`) are the design audit's too
//! (stats_harmony.rs).

use windows::Win32::Foundation::RECT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, LoadCursorW, RegisterClassExW, IDC_ARROW, WNDCLASSEXW};

use super::*;
use crate::gfx::promo_kit as kit;

/// The made-up PC of a picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Scene {
    Calm,
    /// Memory at 86 %.
    Busy,
    /// The CPU at 97 %.
    VeryBusy,
}

/// A minute at `now` (%): `value` for the last quarter of it (so the
/// verdict's ten seconds read `value`), a little under it before, with
/// one reading at `peak`.
fn history(now: u64, value: f32, peak: f32, seed: f32) -> sysstat::Spark {
    let mut s = sysstat::Spark::new(sysstat::SPARK_SPAN_MS);
    for i in 0..=60u64 {
        let t = i as f32;
        let wave = (t * 0.37 + seed).sin() * 0.5 + (t * 1.1 + seed * 2.0).sin() * 0.5;
        let v = if i == 22 {
            peak
        } else if i >= 45 {
            value
        } else {
            (value - (peak - value).min(6.0) * (0.5 + 0.5 * wave)).max(0.0)
        };
        s.push(now - (60 - i) * 1000, Some(v));
    }
    s
}

/// A made-up app (never one of this PC's): no icon, no process.
pub(super) fn made_up(name: &str, window: bool, share: f32) -> Hog {
    Hog { key: name.to_lowercase(), name: name.into(), share, icon: None, procs: Vec::new(), window }
}

/// Readings for the picture (nothing is measured): each resource as
/// (now, last minute's peak).
pub(super) fn readings(scene: Scene, now: u64) -> Snapshot {
    const GB: u64 = 1 << 30;
    let say = kit::say;
    let ([cpu, mem, gpu, disk], used) = match scene {
        Scene::Calm => ([(12.0, 31.0), (41.0, 42.0), (7.0, 18.0), (3.0, 9.0)], 13 * GB + GB / 8),
        Scene::Busy => ([(34.0, 58.0), (86.0, 87.0), (12.0, 20.0), (9.0, 24.0)], 27 * GB + GB / 2),
        Scene::VeryBusy => ([(97.0, 100.0), (72.0, 73.0), (41.0, 55.0), (22.0, 40.0)], 23 * GB),
    };
    let (browser, editor, music, chat, video) = (say("브라우저", "Browser"), say("편집기", "Editor"), say("음악", "Music"), say("메신저", "Messenger"), say("영상 변환", "Video Export"));
    let app = |name: &str| made_up(name, true, 0.0);
    let (by_cpu, by_mem): (Vec<(&str, f32)>, Vec<(&str, u64)>) = match scene {
        Scene::Calm => (vec![(browser, 4.2), (editor, 2.8), (music, 1.1)], vec![(browser, 3 * GB + GB / 10), (editor, 2 * GB - GB / 5), (chat, GB - GB / 10)]),
        Scene::Busy => (vec![(browser, 18.0), (editor, 9.4), (chat, 2.2)], vec![(browser, 10 * GB - GB / 5), (editor, 3 * GB + GB / 10), (chat, GB + 2 * GB / 5)]),
        Scene::VeryBusy => (vec![(video, 71.0), (browser, 12.0), (editor, 4.1)], vec![(browser, 6 * GB + GB / 5), (video, 4 * GB + 2 * GB / 5), (editor, 3 * GB - GB / 10)]),
    };
    Snapshot {
        cpu: Some(cpu.0),
        history: history(now, cpu.0, cpu.1, 0.3),
        mem_history: history(now, mem.0, mem.1, 1.1),
        gpu_history: history(now, gpu.0, gpu.1, 2.4),
        disk_history: history(now, disk.0, disk.1, 3.7),
        at: now,
        memory: Some((used, 32 * GB)),
        gpu: Some(gpu.0),
        cores: vec![18.0, 9.0, 14.0, 6.0, 21.0, 8.0, 11.0, 5.0],
        disk: Some(disk.0),
        net: Some((2_400_000.0, 310_000.0)),
        apps_cpu: by_cpu.into_iter().map(|(n, p)| (app(n), p)).collect(),
        apps_mem: by_mem.into_iter().map(|(n, b)| (app(n), b)).collect(),
        own: Some(sysstat::Own { cpu: Some(0.1), memory: Some(48 << 20), procs: 3 }),
        uptime_ms: (3 * 24 + 4) * 3_600_000,
        seq: 61,
        ..Snapshot::default()
    }
}

impl Panel {
    /// A picture's panel: this snapshot in, judged from scratch at `now`
    /// (`opened`: when the wait began), everything at rest.
    pub(super) fn stage(&mut self, snap: Snapshot, now: u64, opened: u64) {
        (self.levels, self.seen, self.opened, self.samples) = ([None; 4], false, opened, 61);
        self.judge(&snap);
        self.snap = snap;
        self.view = View::blank();
        self.view = self.compute(now, true);
        self.anim.map.clear();
        (self.old_list, self.old_word) = (None, None);
        self.shown_h = self.layout_at(now).height;
    }
}

/// The window class the panel is made in (as `show` registers it).
pub(super) unsafe fn class() {
    let hinst = GetModuleHandleW(None).unwrap();
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarStats"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassExW(&wc);
}

/// A panel for a picture in `theme`: made as `show` makes it (far off
/// every display; its window is never shown), then set to a plugged-in
/// desktop PC with `scene`'s readings: no battery, details shut, nothing
/// picked, pointed at or focused. The caller changes what its picture is
/// about and stages again.
pub(super) unsafe fn staged(stage: &kit::Stage, theme: &Theme, scene: Scene) -> (Panel, u64) {
    let button = RECT { left: -32000, top: -32000, right: -31960, bottom: -31972 };
    let mut p = Panel::new(&stage.gfx, theme, button, 1.0, false, false).expect("the panel");
    // A minute ahead of the process' clock (it starts at 0 with the test).
    let now = sysstat::clock_ms() + 61_000;
    p.scale = kit::scale();
    p.fit = 1.0;
    p.rtl = false;
    p.dec = '.';
    p.battery = false;
    p.on_battery = false;
    p.vram_row = false;
    p.power = Vec::new();
    p.gpu = Vec::new();
    p.measured = true;
    p.stage(readings(scene, now), now, now - 61_000);
    (p, now)
}

#[test]
#[ignore = "writes the promo film's pictures: cargo test -p dock promo_ -- --ignored"]
fn promo_stats() {
    kit::each_lang(stats_pictures);
}

fn stats_pictures() {
    let stage = kit::stage();
    let mut looks = serde_json::Map::new();
    let mut states = serde_json::Map::new();
    unsafe {
        class();
        for (name, key, scene) in [("stats", "calm", Scene::Calm), ("stats-busy", "busy", Scene::Busy), ("stats-very-busy", "veryBusy", Scene::VeryBusy)] {
            let mut per_look = serde_json::Map::new();
            for (folder, light) in kit::LOOKS {
                let theme = kit::theme(light);
                let (p, now) = staged(&stage, &theme, scene);
                // The whole panel, down to the Task Manager button.
                let h = p.shown_h;
                let cv = stage.canvas(W, h);
                let mut hits = Vec::new();
                p.paint(&cv.dc, &mut hits, now).expect("painted");
                cv.save(&format!("{folder}/{name}.png"));
                let plate = kit::panel_json(&theme, W, h, theme.radii().panel, crate::theme::Elevation::E3);
                if scene == Scene::Calm {
                    looks.insert(folder.into(), plate.clone());
                }
                per_look.insert(folder.into(), plate);
                let _ = DestroyWindow(p.hwnd);
            }
            states.insert(key.into(), serde_json::json!({ "file": format!("{name}.png"), "looks": per_look }));
        }
    }
    // `looks`: the calm panel's plate (stats.png, as before); `states`:
    // each state's picture and plate.
    kit::save_json("stats.json", &serde_json::json!({ "scale": kit::scale(), "looks": looks, "states": states }));
    for name in ["stats", "stats-busy", "stats-very-busy"] {
        assert!(kit::out_dir().join(format!("dark/{name}.png")).is_file(), "{name}");
    }
}
