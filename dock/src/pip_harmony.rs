//! PiP's controls drawn off screen for the design audit (tests only; see
//! crate::gfx::promo_kit, "harmony"): the panel popup::chrome gives them
//! (drawn here) and their own picture (paint_bar). The pointer's fill is a
//! visual on screen (crate::wash::Hover, a capsule: smaller pressed):
//! drawn here from the same numbers. The picture under them (a DWM
//! thumbnail of another app's window) is not drawn.
//!
//! The scenes (design system 12.4): at rest (200, 100 and 150 %), the
//! pointer on "Go back to the app", holding it down, the keyboard's ring on
//! "Close", without play / pause; Glass; high contrast (the pointer on one,
//! another held down, the ring on the third).

use super::*;
use crate::gfx::promo_kit as kit;
use crate::wash::harmony as shots;

/// The controls showing `look`, `order` of them, to `<name>.png` in both
/// looks at `scales`.
unsafe fn shoot(name: &str, order: &[Btn], look: BarLook, theme_of: fn(bool) -> Theme, scales: &[f32]) {
    let stage = kit::stage();
    let (w, h) = bar_size(order.len());
    for (folder, light) in kit::LOOKS {
        let theme = theme_of(light);
        for &s in scales {
            let cv = shots::canvas(&stage, &theme, light, w, h, s, theme.radii().panel, crate::theme::Elevation::E3);
            if let Some(lit) = look.lit().and_then(|(t, down)| btn_shape(order, t, false).map(|shape| Lit { t, down, shape })) {
                shots::hover(&cv, &theme, &lit, true);
            }
            shots::on(&cv, s, |dc| paint_bar(dc, &theme, order, look, false)).expect("painted");
            shots::save(cv, folder, &shots::file(name, s));
        }
    }
}

#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_pip() {
    let all = buttons(true);
    let two = [kit::scale()];
    let playing = BarLook { playing: true, ..Default::default() };
    unsafe {
        shoot("pip-bar", &all, playing, shots::plain, &[kit::scale(), 1.0, 1.5]);
        shoot("pip-bar-hover", &all, BarLook { hover: Some(Btn::Go), ..playing }, shots::plain, &two);
        shoot("pip-bar-pressed", &all, BarLook { hover: Some(Btn::Go), pressed: Some(Btn::Go), ..playing }, shots::plain, &two);
        shoot("pip-bar-focus", &all, BarLook { focus: Some(Btn::Close), ..playing }, shots::plain, &two);
        shoot("pip-bar-paused", &buttons(false), BarLook::default(), shots::plain, &two);
        shoot("pip-bar-glass", &all, BarLook { hover: Some(Btn::Toggle), ..playing }, shots::glass, &[1.5]);
        let hc = BarLook { hover: Some(Btn::Toggle), pressed: Some(Btn::Go), focus: Some(Btn::Close), playing: true };
        shoot("pip-bar-hc", &all, hc, shots::hc, &[1.5]);
    }
}

#[test]
fn the_controls_are_capsules_on_the_container_line() {
    // Glyph capsules 32 on the panel's container line (8), 4 apart; the
    // glyphs 16 (design system 3.2).
    assert_eq!((BTN, BAR_PAD, BAR_GAP, GLYPH), (32.0, size::PANEL_PAD, 4.0, 16.0));
    let order = buttons(true);
    let s = btn_shape(&order, Btn::Go, false).unwrap();
    assert_eq!((s.x, s.y, s.w, s.h, s.rad), (BAR_PAD + BTN + BAR_GAP, BAR_PAD, BTN, BTN, BTN / 2.0));
    assert_eq!(btn_shape(&buttons(false), Btn::Toggle, false), None);
    // One fill: the button held, else the one under the pointer.
    let look = BarLook { hover: Some(Btn::Close), pressed: Some(Btn::Go), ..Default::default() };
    assert_eq!(look.lit(), Some((Btn::Go, true)));
    assert_eq!((look.state(Btn::Go), look.state(Btn::Close), look.state(Btn::Toggle)), (State::Pressed, State::Hover, State::Rest));
    assert_eq!(BarLook { hover: Some(Btn::Close), ..Default::default() }.lit(), Some((Btn::Close, false)));
    // They fade in 90 and out 120 (60 straight with reduced motion).
    assert_eq!(bar_fade(true), (crate::motion::FADE_IN_MS, crate::motion::EASE_OUT));
    assert_eq!(bar_fade(false), (crate::motion::FADE_OUT_MS, crate::motion::EASE_IN));
    for (ms, curve) in [bar_fade(true), bar_fade(false)] {
        assert_eq!(crate::motion::fade_plan(crate::motion::Mode::Reduced, ms, curve, 4.0), (crate::motion::REDUCED_MS, crate::motion::LINEAR));
    }
    // From the screen's edges: EDGE_GAP.
    assert_eq!(MARGIN, size::EDGE_GAP);
    // Nothing here picks a color or branches on high contrast itself.
    let src = include_str!("pip.rs");
    let code = &src[..src.find("#[cfg(test)]").unwrap()];
    for gone in ["accent()", "fill_row(", "text_secondary()", ".hc()", "FOCUS_W", "effects::ms(crate::motion::close_ms", "400,"] {
        assert!(!code.contains(gone), "{gone} is still in pip.rs");
    }
    for used in ["fill_of(", "outline_of(", "text_of(", "crate::gfx::focus_ring(", "crate::motion::fade_to(", "fade_plan("] {
        assert!(code.contains(used), "{used}");
    }
}

/// The states inventory (final/states-inventory.md, 2026-11-11): the bar
/// right to left (the buttons mirrored), the keyboard's ring on one, and
/// the pointer on another.
#[test]
#[ignore = "writes the design audit's pictures: cargo test -p dock harmony_ -- --ignored"]
fn harmony_pip_rtl() {
    let _l = crate::i18n::test_lock("ar");
    let all = buttons(true);
    let stage = kit::stage();
    let (w, h) = bar_size(all.len());
    let look = BarLook { playing: true, focus: Some(Btn::Close), hover: Some(Btn::Go), ..Default::default() };
    unsafe {
        for (folder, light) in kit::LOOKS {
            let theme = shots::plain(light);
            let s = kit::scale();
            let cv = shots::canvas(&stage, &theme, light, w, h, s, theme.radii().panel, crate::theme::Elevation::E3);
            if let Some(lit) = look.lit().and_then(|(t, down)| btn_shape(&all, t, true).map(|shape| Lit { t, down, shape })) {
                shots::hover(&cv, &theme, &lit, true);
            }
            shots::on(&cv, s, |dc| paint_bar(dc, &theme, &all, look, true)).expect("painted");
            shots::save(cv, folder, &shots::file("pip-bar-rtl", s));
        }
    }
}
