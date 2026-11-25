//! The dock drawn off screen for the promo film (tests only; see
//! crate::gfx::promo_kit): its tiles one by one, as the dock draws its own
//! (glyphs::tile, cut to the squircle), the whole dock at rest (the bar's
//! tint, border and lit top edge from the theme, the tiles on its
//! baseline, the running dots, the separator), and what the film needs to
//! move it as the dock moves: the sizes and the magnification's numbers
//! this module lays out with.
//!
//! The bar's blur and shadow are the compositor's on screen; the film puts
//! its own blur behind the bar and takes the tint, border and shadows from
//! `dock.json`.

use windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE;
use windows_numerics::Vector2;

use super::{dividers, Look, DOT, DOT_GAP, EDGE_GAP, PAD_AXIS, PAD_CROSS, SEP_W};
use crate::gfx::promo_kit::{self as kit, col, css, rect, Canvas};
use crate::glyphs::{Glyph, TileLook};

/// A tile's picture is this many px a side (the film magnifies them).
const TILE_PX: u32 = 256;
/// Room around the bar in the whole-dock picture (DIP).
const MARGIN: f32 = 24.0;

/// What is on the film's dock: (file name, glyph, its name, running, two
/// windows). None: the separator.
pub(super) fn items() -> Vec<Option<(&'static str, Glyph, &'static str, bool, bool)>> {
    vec![
        Some(("mark", Glyph::Mark, kit::say("런치패드", "Launchpad"), false, false)),
        Some(("folder", Glyph::Folder, kit::say("파일", "Files"), true, true)),
        Some(("globe", Glyph::Globe, kit::say("브라우저", "Browser"), true, false)),
        Some(("document", Glyph::Document, kit::say("문서", "Documents"), false, false)),
        Some(("music", Glyph::Music, kit::say("음악", "Music"), true, false)),
        Some(("image", Glyph::Image, kit::say("사진", "Photos"), false, false)),
        Some(("calendar", Glyph::Calendar, kit::say("일정", "Calendar"), false, false)),
        Some(("terminal", Glyph::Terminal, kit::say("터미널", "Terminal"), true, false)),
        Some(("calculator", Glyph::Calculator, kit::say("계산기", "Calculator"), false, false)),
        Some(("gear", Glyph::Gear, kit::say("설정", "Settings"), false, false)),
        None,
        Some(("download", Glyph::Download, kit::say("다운로드", "Downloads"), false, false)),
        Some(("trash", Glyph::Trash, kit::say("휴지통", "Recycle Bin"), false, false)),
    ]
}

/// More tiles, for the Launchpad's grid (launchpad::promo reads them).
pub(crate) fn extra_tiles() -> Vec<(&'static str, Glyph, &'static str)> {
    vec![
        ("camera", Glyph::Camera, kit::say("카메라", "Camera")),
        ("gamepad", Glyph::Gamepad, kit::say("게임", "Games")),
        ("play", Glyph::Play, kit::say("동영상", "Videos")),
        ("timer", Glyph::Timer, kit::say("타이머", "Timer")),
        ("edit", Glyph::Edit, kit::say("메모", "Notes")),
        ("keyboard", Glyph::Keyboard, kit::say("입력", "Keyboard")),
        ("monitor", Glyph::Monitor, kit::say("디스플레이", "Display")),
        ("headphones", Glyph::Headphones, kit::say("소리", "Sound")),
        ("search", Glyph::Search, kit::say("검색", "Search")),
        ("lock", Glyph::Lock, kit::say("잠금", "Lock")),
        ("sparkle", Glyph::Sparkle, kit::say("효과", "Effects")),
    ]
}

/// What the dock keeps of the work area along its edge by default (DIP;
/// Dock's AppBar reserve): the Launchpad's page is laid out above it.
pub(crate) fn reserve() -> f32 {
    Look::from_cfg(&crate::config::Config::default().dock, true).icon + PAD_CROSS * 2.0 + EDGE_GAP
}

/// Every tile the film uses: (file name, glyph, name).
pub(crate) fn all_tiles() -> Vec<(&'static str, Glyph, &'static str)> {
    let mut v: Vec<_> = items().into_iter().flatten().map(|(id, g, name, ..)| (id, g, name)).collect();
    v.extend(extra_tiles());
    v
}

/// One tile as the dock shows its own: glyphs::tile, cut to the squircle.
fn tile(folder: &str, id: &str, g: Glyph, light: bool) {
    let side = TILE_PX as f32;
    let cv = Canvas::at(side, side, 1.0);
    cv.icon(&crate::glyphs::tile(TILE_PX, g, TileLook::new(light, None)), 0.0, 0.0, side, true);
    cv.save(&format!("{folder}/tiles/{id}.png"));
}

/// The whole dock at rest in look `light`; returns the bar's size (DIP).
fn whole(folder: &str, light: bool) -> (f32, f32) {
    let theme = kit::theme(light);
    let look = Look::from_cfg(&crate::config::Config::default().dock, true);
    let list = items();
    let total: f32 = list.iter().map(|it| if it.is_some() { look.icon } else { SEP_W } + look.gap).sum::<f32>() - look.gap;
    let (bar_w, bar_h) = (total + PAD_AXIS * 2.0, look.icon + PAD_CROSS * 2.0);
    let cv = Canvas::new(bar_w + MARGIN * 2.0, bar_h + MARGIN * 2.0);
    let (bx, by) = (MARGIN, MARGIN);
    let radius = look.radius;
    let bar = rect(bx, by, bx + bar_w, by + bar_h);
    // Dock::build's bar: the tint, the hairline border, the lit top edge.
    cv.fill_rr(bar, radius, theme.dock_tint());
    cv.stroke_rr(rect(bx + 0.5, by + 0.5, bx + bar_w - 0.5, by + bar_h - 0.5), radius - 0.5, theme.dock_border(), 1.0);
    // The rim (Dock::layout's stroke, dock::paint_highlight's brush).
    if let Some(stops) = theme.rim() {
        kit::draw_rim(&cv, bx, by, bar_w, bar_h, radius, stops);
    }
    // Dock::layout: the icons stand on the baseline, the dots under them.
    let baseline = by + PAD_CROSS + look.icon;
    let px = (look.icon * kit::scale()).round() as u32;
    let mut u = bx + PAD_AXIS;
    for it in &list {
        match it {
            Some((_, g, _, running, two)) => {
                cv.icon(&crate::glyphs::tile(px, *g, TileLook::new(light, None)), u, baseline - look.icon, look.icon, look.squircle);
                if *running {
                    let size = super::indicator_size(look.indicator, false, *two);
                    let x0 = u + (look.icon - size.X) / 2.0;
                    let cy = baseline + PAD_CROSS / 2.0;
                    unsafe {
                        let b = cv.dc.CreateSolidColorBrush(&col(theme.indicator_color()), None).unwrap();
                        for k in 0..if *two { 2 } else { 1 } {
                            let cx = x0 + DOT / 2.0 + k as f32 * (DOT + DOT_GAP);
                            cv.dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: DOT / 2.0, radiusY: DOT / 2.0 }, &b);
                        }
                    }
                }
                u += look.icon + look.gap;
            }
            None => {
                let (top, h) = dividers::line_span(baseline, look.icon);
                let x = (u + SEP_W / 2.0 - 0.5).round();
                cv.fill_rr(rect(x, top, x + 1.0, top + h), 0.0, theme.dock_separator());
                u += SEP_W + look.gap;
            }
        }
    }
    cv.save(&format!("{folder}/dock.png"));
    (bar_w, bar_h)
}

#[test]
#[ignore = "writes the promo film's pictures: cargo test -p dock promo_ -- --ignored"]
fn promo_dock() {
    kit::each_lang(dock_pictures);
}

fn dock_pictures() {
    let look = Look::from_cfg(&crate::config::Config::default().dock, true);
    let mut looks = serde_json::Map::new();
    let mut size = (0.0, 0.0);
    for (folder, light) in kit::LOOKS {
        for (id, g, _) in all_tiles() {
            tile(folder, id, g, light);
        }
        size = whole(folder, light);
        let theme = kit::theme(light);
        looks.insert(
            folder.into(),
            serde_json::json!({
                "tint": css(theme.dock_tint()),
                "border": css(theme.dock_border()),
                "highlight": theme.dock_highlight().map(css),
                "rim": kit::rim_json(&theme),
                "rimStops": crate::theme::RIM_STOPS,
                "contact": css(theme.contact()),
                "separator": css(theme.dock_separator()),
                "indicator": css(theme.indicator_color()),
                "shadows": kit::shadows_json(&theme.dock_shadows(false)),
                "radius": look.radius,
            }),
        );
    }
    let list: Vec<serde_json::Value> = items()
        .into_iter()
        .map(|it| match it {
            Some((id, _, name, running, two)) => serde_json::json!({ "id": id, "name": name, "running": running, "two": two }),
            None => serde_json::json!({ "sep": true }),
        })
        .collect();
    kit::save_json(
        "dock.json",
        &serde_json::json!({
            "scale": kit::scale(),
            "margin": MARGIN,
            "bar": { "w": size.0, "h": size.1 },
            "icon": look.icon,
            "gap": look.gap,
            "maxScale": look.max_scale,
            "range": look.range,
            "padAxis": PAD_AXIS,
            "padCross": PAD_CROSS,
            "edgeGap": EDGE_GAP,
            "sepW": SEP_W,
            "dot": DOT,
            "dotGap": DOT_GAP,
            "spring": { "k": super::layout::FOLLOW.stiffness, "c": super::layout::FOLLOW.damping },
            "items": list,
            "extra": extra_tiles().into_iter().map(|(id, _, name)| serde_json::json!({ "id": id, "name": name })).collect::<Vec<_>>(),
            "looks": looks,
        }),
    );
    assert!(kit::out_dir().join("light/dock.png").is_file() && kit::out_dir().join("dark/tiles/mark.png").is_file());
}
