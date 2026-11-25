//! Harmony v2.1's tokens drawn off screen for the design audit (tests
//! only; see crate::gfx::promo_kit, "harmony"): one sheet per look (Flat,
//! Glass, high contrast) and palette. The four materials on their
//! elevations with the rim and the contact shadow (popup::chrome_at's
//! plate, as promo_kit draws it), what sits on them (a card, a button, a
//! row under the pointer and one under the keyboard's cursor), and the
//! shared parts (gfx::parts): a well with its knob, a switch, the focus
//! ring, the progress ring, the quiet states, the ramp's styles with their
//! optical-size instances and tracking.
//!
//! High contrast's system colors are fixed here: a black window, white
//! text, a sky-blue Highlight. Glass is drawn over the audit's opaque
//! backdrop: the blur is not seen, only the tints' opacity and how
//! surfaces part.

use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_ROUNDED_RECT};
use windows_numerics::Vector2;

use crate::gfx::parts::{focus_ring, knob, spinner, well, Quiet};
use crate::gfx::promo_kit as kit;
use crate::glyphs::Glyph;
use crate::syslook::{HighContrast, SysLook};
use crate::theme::metrics::size;
use crate::theme::{ramp, Elevation, Finish, Level, Rgba, Role, State, Theme};

fn col(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

fn rc(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

/// The theme of `look` ("flat", "glass", "hc") in the palette.
fn look_theme(look: &str, light: bool) -> Theme {
    let grey = if light { 0x3A } else { 0xE6 };
    let (white, black) = (Rgba { r: 255, g: 255, b: 255, a: 255 }, Rgba { r: 0, g: 0, b: 0, a: 255 });
    let hc = (look == "hc").then_some(HighContrast { window: black, text: white, highlight: Rgba { r: 0x1A, g: 0xEB, b: 0xFF, a: 255 }, highlight_text: black, gray_text: crate::syslook::GRAY_TEXT });
    let sys = SysLook { light, accent: Rgba { r: grey, g: grey, b: grey, a: 255 }, transparency: true, high_contrast: hc, animations: true, ui_font: 0 };
    let mut raw = Theme::graphite();
    if look == "glass" {
        raw.finish = Finish::Glass;
    }
    raw.resolve_fx(&sys, true)
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_tokens() {
    let _l = crate::i18n::test_lock("ko");
    let stage = kit::stage();
    let (w, h) = (780.0f32, 470.0f32);
    for look in ["flat", "glass", "hc"] {
        for (folder, light) in kit::LOOKS {
            let theme = look_theme(look, light);
            let r = theme.radii();
            let cv = kit::harmony_canvas(&stage, w, h, light);
            let m = kit::HARMONY_M;
            // ---- the materials on their elevations ----
            // M4 a tip, M3 a menu, M2 a panel, M1 the dock.
            let tip = (m, m, 132.0, 28.0);
            let menu = (m, m + 60.0, 176.0, 140.0);
            let panel = (m + 216.0, m, 240.0, 200.0);
            let dock = (m + 540.0, m + 20.0, 208.0, 68.0);
            for ((x, y, pw, ph), radius, e) in [(tip, r.tip, Elevation::E1), (menu, r.menu, Elevation::E2), (panel, r.panel, Elevation::E3), (dock, theme.dock.radius, Elevation::E4)] {
                kit::draw_plate(&cv, x, y, pw, ph, &kit::popup_plate(&theme, radius, e));
            }
            // A menu over the panel: a step lighter, a smaller shadow.
            let over = (m + 430.0, m + 28.0, 100.0, 64.0);
            kit::draw_plate(&cv, over.0, over.1, over.2, over.3, &kit::popup_plate(&theme, r.menu, Elevation::E2));
            // ---- a second panel for the parts ----
            let parts = (m, m + 236.0, 716.0, 170.0);
            kit::draw_plate(&cv, parts.0, parts.1, parts.2, parts.3, &kit::popup_plate(&theme, r.panel, Elevation::E3));
            kit::harmony_at(&cv, 0.0, 0.0, |dc| unsafe {
                let gfx = &stage.gfx;
                let fam = theme.font.family.clone();
                let fill = |rect: D2D_RECT_F, radius: f32, c: Rgba| {
                    if c.a > 0 {
                        dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius }, &dc.CreateSolidColorBrush(&col(c), None).unwrap());
                    }
                };
                let outline = |rect: D2D_RECT_F, radius: f32, role: Role, s: State| {
                    if let Some((c, wd)) = theme.outline_of(role, s) {
                        let i = wd / 2.0;
                        let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: rect.left + i, top: rect.top + i, right: rect.right - i, bottom: rect.bottom - i }, radiusX: (radius - i).max(0.0), radiusY: (radius - i).max(0.0) };
                        dc.DrawRoundedRectangle(&rr, &dc.CreateSolidColorBrush(&col(c), None).unwrap(), wd, None);
                    }
                };
                let text = |s: &str, style: ramp::TextStyle, x: f32, y: f32, c: Rgba| {
                    let (layout, tw, th) = gfx.text_style(s, &fam, style, 1.0).unwrap();
                    dc.DrawTextLayout(Vector2 { X: x, Y: y }, &layout, &dc.CreateSolidColorBrush(&col(c), None).unwrap(), D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                    (tw, th)
                };
                let ink = |l: Level, s: State| theme.text_of(l, s);
                // The tip's text.
                text("이름표 M4 · E1", ramp::FOOTNOTE, tip.0 + 8.0, tip.1 + 6.0, ink(Level::Primary, State::Rest));
                // The menu: rows at rest, under the pointer, the cursor,
                // pressed, one that cannot be used. Rows 4 in, text at 16.
                let rows = [("쉼", State::Rest), ("올림", State::Hover), ("키보드 커서", State::Cursor), ("누름", State::Pressed), ("못 씀", State::Disabled)];
                for (i, (name, s)) in rows.into_iter().enumerate() {
                    let rr = rc(menu.0 + size::MENU_PAD, menu.1 + size::MENU_PAD + i as f32 * 26.0, menu.2 - size::MENU_PAD * 2.0, 26.0);
                    let row_r = crate::theme::concentric(r.menu, size::MENU_PAD);
                    fill(rr, row_r, theme.fill_of(Role::Row, s));
                    outline(rr, row_r, Role::Row, s);
                    text(name, ramp::BODY, menu.0 + size::MENU_PAD + size::CARD_PAD, rr.top + 4.0, ink(Level::Primary, s));
                }
                // The panel: cards 8 in (radius 12), a button 4 inside one
                // (radius 8), content at 20.
                let card = rc(panel.0 + size::PANEL_PAD, panel.1 + size::PANEL_PAD, panel.2 - size::PANEL_PAD * 2.0, 84.0);
                crate::gfx::parts::card(dc, card, r.card, &theme, State::Rest, theme.fill_of(Role::Raised, State::Rest), 1.0, 1.0).unwrap();
                outline(card, r.card, Role::Raised, State::Rest);
                text("카드 M2 · E3", ramp::HEADLINE, card.left + size::CARD_PAD, card.top + size::CARD_PAD, ink(Level::Primary, State::Rest));
                text("보조 줄 12", ramp::FOOTNOTE, card.left + size::CARD_PAD, card.top + size::CARD_PAD + 22.0, ink(Level::Secondary, State::Rest));
                let states = [State::Rest, State::Hover, State::Pressed, State::Selected];
                for (i, s) in states.into_iter().enumerate() {
                    let b = rc(panel.0 + size::PANEL_PAD + i as f32 * 58.0, card.bottom + size::PANEL_PAD, 50.0, size::CONTROL_H);
                    crate::gfx::parts::card(dc, b, r.control, &theme, s, theme.fill_of(Role::Raised, s), 1.0, 1.0).unwrap();
                    outline(b, r.control, Role::Raised, s);
                    let label_c = if s == State::Selected && !theme.high_contrast { theme.on_active() } else { ink(Level::Primary, s) };
                    text(["쉼", "올림", "누름", "켜짐"][i], ramp::FOOTNOTE, b.left + 10.0, b.top + 6.0, label_c);
                }
                let wide = rc(panel.0 + size::PANEL_PAD, panel.1 + panel.3 - size::PANEL_PAD - size::BUTTON_WIDE_H, panel.2 - size::PANEL_PAD * 2.0, size::BUTTON_WIDE_H);
                crate::gfx::parts::card(dc, wide, r.card, &theme, State::Rest, theme.fill_of(Role::Raised, State::Rest), 1.0, 1.0).unwrap();
                outline(wide, r.card, Role::Raised, State::Rest);
                text("패널 폭 버튼 (12)", ramp::BODY, wide.left + size::CARD_PAD, wide.top + 7.0, ink(Level::Primary, State::Rest));
                focus_ring(dc, wide, r.card, &theme, 1.0).unwrap();
                text("메뉴 M3", ramp::FOOTNOTE, over.0 + 28.0, over.1 + 12.0, ink(Level::Primary, State::Rest));
                text("독 M1 · E4", ramp::FOOTNOTE, dock.0 + 20.0, dock.1 + 26.0, ink(Level::Primary, State::Rest));
                // ---- the parts ----
                let (px, py) = (parts.0 + size::PANEL_PAD + size::CARD_PAD, parts.1 + size::PANEL_PAD + size::CARD_PAD);
                // A slider: the well, its filled part, its knob.
                let track = rc(px, py + 6.0, 200.0, size::TRACK_H);
                well(dc, track, size::TRACK_H / 2.0, &theme, 1.0).unwrap();
                fill(rc(px, py + 6.0, 120.0, size::TRACK_H), size::TRACK_H / 2.0, theme.fill_of(Role::Well, State::Selected));
                knob(dc, px + 120.0, py + 8.0, size::KNOB / 2.0, &theme, false, 1.0).unwrap();
                // Switches: off (a well) and on.
                let off = rc(px, py + 34.0, size::TOGGLE_W, size::TOGGLE_H);
                well(dc, off, size::TOGGLE_H / 2.0, &theme, 1.0).unwrap();
                knob(dc, off.left + size::TOGGLE_H / 2.0, off.top + size::TOGGLE_H / 2.0, size::TOGGLE_KNOB / 2.0 - 1.0, &theme, false, 1.0).unwrap();
                let on = rc(px + 52.0, py + 34.0, size::TOGGLE_W, size::TOGGLE_H);
                fill(on, size::TOGGLE_H / 2.0, theme.fill_of(Role::Well, State::Selected));
                knob(dc, on.right - size::TOGGLE_H / 2.0, on.top + size::TOGGLE_H / 2.0, size::TOGGLE_KNOB / 2.0 - 1.0, &theme, true, 1.0).unwrap();
                // A field (a well) with the focus ring.
                let field = rc(px, py + 72.0, 200.0, size::CONTROL_H);
                well(dc, field, r.control, &theme, 1.0).unwrap();
                text("입력칸", ramp::BODY, field.left + size::CARD_PAD, field.top + 5.0, ink(Level::Tertiary, State::Rest));
                focus_ring(dc, field, r.control, &theme, 1.0).unwrap();
                // The progress ring, and a value not known yet.
                spinner(dc, px + 8.0, py + 124.0, 0.0, &theme, 1.0).unwrap();
                spinner(dc, px + 36.0, py + 124.0, 0.35, &theme, 1.0).unwrap();
                text("–", ramp::BODY, px + 60.0, py + 115.0, ink(Level::Tertiary, State::Rest));
                // The quiet states: empty, waiting, wrong.
                for (i, (kind, glyph, words)) in [(Quiet::Empty, Glyph::Search, "결과 없음"), (Quiet::Loading, Glyph::Search, ""), (Quiet::Error, Glyph::Folder, "열 수 없음")].into_iter().enumerate() {
                    let area = rc(px + 232.0 + i as f32 * 108.0, parts.1 + size::PANEL_PAD, 100.0, size::QUIET_MIN_H);
                    crate::gfx::parts::card(dc, area, r.card, &theme, State::Rest, theme.fill_of(Role::Raised, State::Rest), 1.0, 1.0).unwrap();
                    outline(area, r.card, Role::Raised, State::Rest);
                    gfx.quiet(dc, area, kind, glyph, words, &theme, 1.0, 0.0).unwrap();
                }
                // The ramp: each style in its own instance, with its tracking.
                let mut ty = parts.1 + size::PANEL_PAD;
                for (name, style) in [("Caption 11", ramp::CAPTION), ("Body 13", ramp::BODY), ("Headline 15", ramp::HEADLINE), ("Title 0123", ramp::TITLE), ("Large 26", ramp::LARGE_TITLE)] {
                    let (_, th) = text(name, style, px + 568.0, ty, ink(Level::Primary, State::Rest));
                    ty += th + 2.0;
                }
            });
            kit::harmony_save(cv, folder, &format!("tokens-{look}"));
            assert!(kit::harmony_dir().join(folder).join(format!("tokens-{look}.png")).is_file());
        }
    }
}
