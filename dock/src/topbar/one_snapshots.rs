//! Offscreen WARP only. No HWND, capture or input injection.
use super::*;
use crate::gfx::promo_kit::{self as kit, Canvas};
use crate::theme::State;
use windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE;
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;

#[test]
#[ignore = "offscreen topbar one-cell state matrix"]
fn topbar_one_matrix() {
    let _lang = crate::i18n::test_lock("en");
    let stage = kit::stage();
    let root = std::env::var_os("TOPBAR_ONE_SHOTS").map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("topbar-one-shots"));
    std::fs::create_dir_all(&root).unwrap();
    for (name, light, hc) in [("dark", false, false), ("light", true, false), ("hc", false, true)] {
        let mut theme = super::super::bar_harmony::look_theme("flat", light);
        if hc {
            let white = crate::theme::Rgba { r: 255, g: 255, b: 255, a: 255 };
            let black = crate::theme::Rgba { r: 0, g: 0, b: 0, a: 255 };
            let gray = crate::theme::Rgba { r: 80, g: 80, b: 80, a: 255 };
            theme = crate::theme::Theme::graphite().resolve_fx(&crate::syslook::SysLook {
                light: false, accent: gray, transparency: false, animations: false, ui_font: 0,
                high_contrast: Some(crate::syslook::HighContrast { window: black, text: white, highlight: gray, highlight_text: white, gray_text: white }),
            }, true);
        }
        let ground = look::ground(&theme, look::Style::Solid, look::Env::default(), &look::Wall::Pending);
        for dpi in [1., 1.5, 2.] {
            for (state, art, open) in [
                ("home", picture(true, false, false), false),
                ("away", picture(true, true, false), false),
                ("notice", picture(true, false, true), false),
                ("open", picture(true, false, true), true),
                ("disabled", picture(false, false, true), false),
            ] {
                let h = crate::config::TOPBAR_HEIGHT;
                let cv = Canvas::of(stage.gfx.d2d_factory().clone(), 400., h, dpi);
                let ink = kit::col(ground.ink);
                unsafe {
                    cv.dc.Clear(Some(&kit::col(ground.tint)));
                    cv.dc.SetTransform(&scale(dpi, dpi));
                    let cell_x = 170.;
                    let cell_w = cell_width(Cell::Companion, 20.);
                    let (inset, ph, radius) = hover_pill(h);
                    if open {
                        let fill = capsule::look(&theme, ground.mark, State::Open, cell_w, ph, crate::motion::Mode::Full);
                        cv.fill_rr(kit::rect(cell_x, inset, cell_x + cell_w, inset + ph), radius, fill.fill);
                    }
                    cv.dc.SetTransform(&windows_numerics::Matrix3x2::identity());
                    paint_glyph_cell(&cv.dc, Glyph::Wifi(4), 70. * dpi, (h - GLYPH_BOX) / 2. * dpi, 16. * dpi, GLYPH_SIZE * dpi, ink).unwrap();
                    paint_glyph_cell(&cv.dc, Glyph::Volume(2), 100. * dpi, (h - GLYPH_BOX) / 2. * dpi, 16. * dpi, GLYPH_SIZE * dpi, ink).unwrap();
                    cv.dc.SetTransform(&scale(dpi, dpi));
                    let battery = battery_art(&stage.gfx, &theme, ink, 72, false, 1.);
                    paint_battery(&cv.dc, 130., (h - BATTERY_H) / 2., &battery).unwrap();
                    let x = content_x(Cell::Companion, cell_x, cell_w, 20., false);
                    cv.dc.SetTransform(&(scale(dpi, dpi) * translate(x * dpi, 4. * dpi)));
                    paint_cell(&cv.dc, art, h - 8., ground.ink, dpi, hc).unwrap();
                    cv.dc.SetTransform(&scale(dpi, dpi));
                    let (text, _, th) = stage.gfx.text_layout_tabular("Nov 17 Mon 6:12 PM", &theme.font.family, theme.font.size, DWRITE_FONT_WEIGHT_NORMAL).unwrap();
                    let metrics = stage.gfx.font_metrics(&theme.font.family, DWRITE_FONT_WEIGHT_NORMAL)
                        .map(|m| baseline::Metrics::of(m.designUnitsPerEm, m.descent, m.capHeight)).unwrap_or(baseline::Metrics::SEGOE);
                    let line = baseline::line(h, &metrics, theme.font.size * crate::fonts::scale());
                    let text_y = baseline::origin_y(line.baseline, crate::gfx::first_baseline(&text, th));
                    let brush = cv.dc.CreateSolidColorBrush(&ink, None).unwrap();
                    crate::gfx::draw_text(&cv.dc, windows_numerics::Vector2 { X: cell_x + cell_w + CLOCK_LEAD, Y: text_y }, &text, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                }
                let (px, w, h) = cv.finish();
                kit::save_png(&px, w, h, &root.join(format!("{name}-{state}-{}.png", (dpi * 100.) as u32)));
            }
        }
        // Shared paint enlarged for checking the dot's edge and the seated sprite.
        let cv = Canvas::of(stage.gfx.d2d_factory().clone(), 80., 40., 4.);
        unsafe {
            cv.dc.Clear(Some(&kit::col(ground.tint)));
            cv.dc.SetTransform(&(scale(4., 4.) * translate(40., 32.)));
            paint_cell(&cv.dc, Picture::Home { notice: true }, crate::config::TOPBAR_HEIGHT - 8., ground.ink, 4., hc).unwrap();
            cv.dc.SetTransform(&(scale(4., 4.) * translate(200., 32.)));
            paint_cell(&cv.dc, Picture::Mark, crate::config::TOPBAR_HEIGHT - 8., ground.ink, 4., hc).unwrap();
        }
        let (px, w, h) = cv.finish();
        kit::save_png(&px, w, h, &root.join(format!("{name}-detail.png")));
    }
}
