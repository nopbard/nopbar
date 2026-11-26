//! The Launchpad drawn off screen for the promo film (tests only; see
//! crate::gfx::promo_kit): one page of it on a work area as wide and tall
//! as the film's desktop, laid out by the Launchpad's own grid (grid.rs:
//! regions, the cells, the icons' and names' places, the page dots), the
//! search field's plate by the overlay's own paint (paint_field_plate) with
//! its placeholder, the names as label_brush does
//! (the theme's primary text, centered, two lines at most), with the
//! dock's own drawn tiles for apps. The dock stays in view under the page
//! (the work area ends above it). The blurred desktop and the tint over
//! it are the compositor's on screen: the film draws them from
//! `launchpad.json`.

use windows::Win32::Graphics::Direct2D::{D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
use windows_numerics::Vector2;

use super::super::grid;
use super::{lines_h, paint_field_plate, FIELD_PAD, FIELD_TEXT};
use crate::gfx::promo_kit::{self as kit, col, css};
use crate::glyphs::TileLook;
use crate::theme::metrics::{size, space};
use crate::theme::{Level, State};

/// The film's desktops (DIP): 16:9 and 9:16, both shown at 1.5 px a DIP.
const STAGES: [(&str, f32, f32); 2] = [("wide", 1280.0, 720.0), ("tall", 720.0, 1280.0)];
/// The bar above the work area (the default height: config).
const BAR_H: f32 = 32.0;

fn page(stage: &kit::Stage, folder: &str, light: bool, name: &str, w: f32, h: f32) -> serde_json::Value {
    page_named(stage, folder, light, name, w, h, &[])
}

/// page, the first apps named `names` instead (empty: the tiles' own).
#[allow(clippy::too_many_arguments)]
fn page_named(stage: &kit::Stage, folder: &str, light: bool, name: &str, w: f32, h: f32, names: &[&str]) -> serde_json::Value {
    let theme = kit::theme(light).windows_shell();
    let gfx = &stage.gfx;
    let family = theme.font.family.clone();
    // The work area: the display under the bar and over the dock (both
    // keep their strip of it by default).
    let (ww, wh) = (w, h - BAR_H - crate::dock::promo::reserve());
    let two_lines = lines_h(gfx.dwrite(), &family, grid::LABEL_SIZE * crate::fonts::scale(), 2).unwrap_or(0.0);
    let regions = grid::regions(ww, wh, false);
    let g = grid::grid_in(&regions, two_lines);
    let own_icon = name == "own-icon-all-apps";
    let apps: Vec<_> = crate::dock::promo::all_tiles().into_iter().filter(|(id, ..)| *id != "trash" && (*id != "mark" || own_icon)).collect();
    let per_page = g.per_page().max(1);
    let pages = apps.len().div_ceil(per_page).max(2);
    let cv = stage.canvas(w, h);
    if name == "windows-all-apps" || own_icon {
        unsafe {
            cv.dc.Clear(Some(&col(kit::harmony_backdrop(light))));
            let brush = cv.dc.CreateSolidColorBrush(&super::plate_tint(&theme), None).unwrap();
            cv.dc.FillRectangle(&kit::rect(0.0, 0.0, w, h), &brush);
        }
    }
    let (ink, dim) = (theme.text_primary(), theme.text_secondary());
    let oy = BAR_H;
    unsafe {
        // The search field (View::paint_field): the capsule well and the
        // glass, then the placeholder in the tertiary level.
        let f = regions.field();
        kit::harmony_at(&cv, f.l, oy + f.t, |dc| paint_field_plate(dc, f.w(), f.h(), &theme, false, false, col(dim))).unwrap();
        if let Ok((l, _, th)) = gfx.text_layout(crate::i18n::t("launchpad.placeholder"), &family, FIELD_TEXT, DWRITE_FONT_WEIGHT_NORMAL) {
            let b = cv.dc.CreateSolidColorBrush(&col(theme.text_of(Level::Tertiary, State::Rest)), None).unwrap();
            cv.dc.DrawTextLayout(Vector2 { X: f.l + FIELD_PAD + size::ICON_S + space::S3, Y: oy + f.cy() - th / 2.0 }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
        }
        // The first page: icons and names in their cells.
        let ink_b = cv.dc.CreateSolidColorBrush(&col(ink), None).unwrap();
        for (slot, (_, picture, label)) in apps.iter().take(per_page).enumerate() {
            let ir = g.icon(slot, false);
            let px = (ir.w() * kit::scale()).round() as u32;
            let (icon, squircle) = match picture {
                crate::dock::promo::Picture::NopBar => {
                    let key = crate::icons::NOPBAR_ICON_KEY;
                    let icon = super::super::loader::load(key, super::super::loader::frame_px(px)).expect("Load the NopBar app entry");
                    (icon, false)
                }
                _ => (picture.draw(px, TileLook::new(light, None)), true),
            };
            cv.icon(&icon, ir.l, oy + ir.t, ir.w(), squircle);
            let lr = g.label(slot, false);
            let lw = g.label_w();
            let label: &str = names.get(slot).copied().unwrap_or(*label);
            if let Ok((l, _)) = gfx.text_layout_wrapped(label, &family, grid::LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL, lw, 2) {
                cv.dc.DrawTextLayout(Vector2 { X: (lr.cx() - lw / 2.0).round(), Y: (oy + lr.t).round() }, &l, &ink_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
        }
        // The page dots (View::draw_dots): the page shown in the primary
        // text, the others in the disabled level.
        let row = regions.dots_row(pages, 0, false);
        for p in row.shown_pages() {
            let Some(d) = row.shape(p) else { continue };
            let c = if p == 0 { ink } else { theme.text_disabled() };
            let b = cv.dc.CreateSolidColorBrush(&col(c), None).unwrap();
            cv.dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: d.cx(), Y: oy + d.cy() }, radiusX: d.w() / 2.0, radiusY: d.w() / 2.0 }, &b);
        }
    }
    if name == "windows-all-apps" || own_icon { kit::harmony_save(cv, folder, name); }
    else { cv.save(&format!("{folder}/launchpad-{name}.png")); }
    serde_json::json!({ "w": w, "h": h, "cols": g.cols, "rows": g.rows })
}

#[test]
#[ignore = "writes the promo film's pictures: cargo test -p dock promo_ -- --ignored"]
fn promo_launchpad() {
    kit::each_lang(launchpad_pictures);
}

#[test]
#[ignore = "Windows feel all apps page offscreen, synthetic apps"]
fn windows_feel_all_apps() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    for (folder, light) in kit::LOOKS {
        page_named(&stage, folder, light, "windows-all-apps", 1280.0, 720.0, &[]);
    }
}

#[test]
#[ignore = "writes the bundled app icon in the All Apps grid"]
fn render_own_icon_all_apps() {
    let out = std::env::var_os("NOPBAR_OWN_ICON_RENDER_DIR").expect("own icon render output directory");
    let _lang = crate::i18n::test_lock("en");
    let stage = kit::stage();
    kit::with_shot_root(out.into(), || {
        for (folder, light) in kit::LOOKS {
            page(&stage, folder, light, "own-icon-all-apps", 1280.0, 720.0);
        }
    });
}

fn launchpad_pictures() {
    let stage = kit::stage();
    let mut looks = serde_json::Map::new();
    let mut stages = serde_json::Map::new();
    for (folder, light) in kit::LOOKS {
        for (name, w, h) in STAGES {
            stages.insert(name.into(), page(&stage, folder, light, name, w, h));
        }
        let theme = kit::theme(light);
        // The plate over the film's blurred desktop: the scrim's tint
        // (Theme::scrim, as view::plate_tint has it over a blur).
        looks.insert(folder.into(), serde_json::json!({ "tint": css(theme.scrim()) }));
    }
    kit::save_json("launchpad.json", &serde_json::json!({ "scale": kit::scale(), "barH": BAR_H, "dockReserve": crate::dock::promo::reserve(), "stages": stages, "looks": looks }));
    assert!(kit::out_dir().join("light/launchpad-wide.png").is_file());
}

/// One page with real-world names (Korean, English, long ones that wrap)
/// in both looks, for the font comparison (crate::font_compare):
/// `launchpad-wide.png`.
pub(crate) fn font_compare() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let names = [
        "Visual Studio Code", "카카오톡", "Adobe Photoshop 2026", "Microsoft Teams (work or school)", "한컴오피스 한글 2024", "Google Chrome", "네이버 웨일", "Windows 터미널",
        "Steam", "디스코드", "Spotify", "알약 - 바이러스 검사와 PC 최적화 도우미", "Notion", "Microsoft Excel", "곰플레이어", "OBS Studio", "원드라이브", "Figma", "Blender 4.2", "메모장",
        "계산기", "작업 관리자", "명령 프롬프트", "파일 탐색기", "Xbox Game Bar", "알캡처", "반디집", "Zoom Workplace", "사진", "설정", "Microsoft Store", "카메라",
        "시계", "녹음기", "Paint 3D", "Clipchamp",
    ];
    for (folder, light) in kit::LOOKS {
        page_named(&stage, folder, light, "wide", 1280.0, 720.0, &names);
    }
}
