//! The top bar drawn off screen for the promo film (tests only; see
//! crate::gfx::promo_kit): the cells of a bar as wide as the film's
//! desktop, placed as TopBar::layout places them (the app's name and menus
//! from the left edge, the status cells and the clock from the right, on
//! the bar's one baseline), in the bar's text color, with the bar's own
//! glyphs. The Start cell is left out (the film shows no logo but
//! NopBar's). The blur and tint under the cells are the compositor's on
//! screen: the film draws them from `topbar.json`.

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE;
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL};
use windows_numerics::Vector2;

use super::{baseline, cell_width, content_x, status, EDGE_PAD, GLYPH_SIZE};
use crate::gfx::promo_kit::{self as kit, col, css};
use crate::glyphs::Glyph;

/// The film's desktops (DIP): 16:9 and 9:16.
pub(super) const STAGES: [(&str, f32); 2] = [("wide", 1280.0), ("tall", 720.0)];

/// What a cell of the film's bar shows.
enum Cell {
    Text(&'static str, bool),
    Clock(String),
    Glyph(Glyph),
}

/// The bar's type: the theme's family and size (the same in both looks).
fn face() -> (String, f32) {
    let theme = kit::theme(false);
    (theme.font.family.clone(), theme.font.size.clamp(10.0, 18.0))
}

fn text(gfx: &crate::gfx::Gfx, c: &Cell) -> Option<(windows::Win32::Graphics::DirectWrite::IDWriteTextLayout, f32, f32)> {
    let (family, size) = face();
    match c {
        Cell::Text(t, bold) => gfx.text_layout(t, &family, size, if *bold { DWRITE_FONT_WEIGHT_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL }).ok(),
        Cell::Clock(t) => gfx.text_layout_tabular(t, &family, size, DWRITE_FONT_WEIGHT_NORMAL).ok(),
        Cell::Glyph(_) => None,
    }
}

/// The bar's cell a film cell stands for (its pads: super::pads).
fn kind(c: &Cell) -> super::Cell {
    match c {
        Cell::Text(_, true) => super::Cell::App,
        Cell::Text(_, false) => super::Cell::Menu(0),
        Cell::Clock(_) => super::Cell::Clock,
        Cell::Glyph(_) => super::Cell::Search,
    }
}

/// The cells of a bar `width` wide in the language being drawn, each with
/// its place (x, width; DIP), the left side first.
fn cells(gfx: &crate::gfx::Gfx, width: f32) -> Vec<(Cell, f32, f32)> {
    // A Monday afternoon (nothing is read from the clock).
    let now = SYSTEMTIME { wYear: 2026, wMonth: 11, wDayOfWeek: 1, wDay: 9, wHour: 14, wMinute: 30, wSecond: 0, wMilliseconds: 0 };
    let mut left = vec![Cell::Text(kit::say("파일", "Files"), true)];
    // A narrow bar keeps the app's name and gives up its menus (narrow_plan).
    if width >= 1000.0 {
        left.extend([("편집", "Edit"), ("보기", "View"), ("이동", "Go"), ("창", "Window"), ("도움말", "Help")].map(|(ko, en)| Cell::Text(kit::say(ko, en), false)));
    }
    let right = [Cell::Glyph(Glyph::Search), Cell::Glyph(Glyph::Sliders), Cell::Clock(status::clock_text(&now, false))];
    let content = |c: &Cell| match c {
        Cell::Glyph(_) => GLYPH_SIZE,
        _ => text(gfx, c).map(|l| l.1).unwrap_or(0.0),
    };
    // TopBar::layout: the left side from EDGE_PAD, the right side back
    // from the other edge, each cell as wide as cell_width makes it.
    let mut placed = Vec::new();
    let mut x = EDGE_PAD;
    for c in left {
        let w = cell_width(kind(&c), content(&c));
        placed.push((c, x, w));
        x += w;
    }
    let mut x = width - EDGE_PAD;
    let mut back = Vec::new();
    for c in right.into_iter().rev() {
        let w = cell_width(kind(&c), content(&c));
        x -= w;
        back.push((c, x, w));
    }
    placed.extend(back.into_iter().rev());
    placed
}

/// Where the cells of a bar `width` wide sit (x, width; DIP): what the
/// Live Island's pill keeps clear of (island::model::place).
pub(super) fn cell_spans(stage: &kit::Stage, width: f32) -> Vec<(f32, f32)> {
    cells(&stage.gfx, width).into_iter().map(|(_, x, w)| (x, w)).collect()
}

fn bar(stage: &kit::Stage, folder: &str, light: bool, name: &str, width: f32) -> f32 {
    let theme = kit::theme(light);
    let gfx = &stage.gfx;
    let (family, size) = face();
    let h = crate::config::Config::default().topbar.height.clamp(20.0, 48.0);
    let ink = col(theme.topbar_text());
    let metrics = gfx.font_metrics(&family, DWRITE_FONT_WEIGHT_NORMAL).map(|m| baseline::Metrics::of(m.designUnitsPerEm, m.descent, m.capHeight)).unwrap_or(baseline::Metrics::SEGOE);
    // The bar's one line, in the picture's pixels (TopBar::bar_line).
    let line = baseline::line(h * kit::scale(), &metrics, size * kit::scale() * crate::fonts::scale());
    let cv = stage.canvas(width, h);
    for (c, x, w) in cells(gfx, width) {
        unsafe {
            match &c {
                Cell::Glyph(g) => crate::glyphs::draw(&cv.dc, *g, x + w / 2.0, h / 2.0, GLYPH_SIZE, ink).unwrap(),
                _ => {
                    let Some((l, tw, th)) = text(gfx, &c) else { continue };
                    // On the bar's baseline (TopBar::on_bar_line), on whole pixels.
                    let y = baseline::origin_y(line.baseline, crate::gfx::first_baseline(&l, th) * kit::scale()) / kit::scale();
                    let b = cv.dc.CreateSolidColorBrush(&ink, None).unwrap();
                    cv.dc.DrawTextLayout(Vector2 { X: baseline::snap_px(content_x(kind(&c), x, w, tw, false), kit::scale()), Y: y }, &l, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
            }
        }
    }
    cv.save(&format!("{folder}/topbar-{name}.png"));
    h
}

#[test]
#[ignore = "writes the promo film's pictures: cargo test -p dock promo_ -- --ignored"]
fn promo_topbar() {
    kit::each_lang(topbar_pictures);
}

fn topbar_pictures() {
    let stage = kit::stage();
    let mut looks = serde_json::Map::new();
    let mut h = 0.0;
    for (folder, light) in kit::LOOKS {
        for (name, width) in STAGES {
            h = bar(&stage, folder, light, name, width);
        }
        let theme = kit::theme(light);
        looks.insert(folder.into(), serde_json::json!({ "tint": css(theme.topbar_tint()), "text": css(theme.topbar_text()) }));
    }
    kit::save_json("topbar.json", &serde_json::json!({ "scale": kit::scale(), "h": h, "looks": looks }));
    assert!(kit::out_dir().join("dark/topbar-wide.png").is_file());
}

/// The film's wide bar in both looks, in Korean and in English, for the
/// font comparison (crate::font_compare): `topbar-wide-<lang>.png`.
pub(crate) fn font_compare() {
    let stage = kit::stage();
    for lang in ["ko", "en"] {
        let _l = crate::i18n::test_lock(lang);
        for (folder, light) in kit::LOOKS {
            bar(&stage, folder, light, &format!("wide-{lang}"), 1280.0);
        }
    }
}
