//! The input indicator cell (crate::ime): 가 / A, あ / A, 中 / 英 or the
//! keyboard layout's short name in a small rounded box, with a ⇪ badge
//! while Caps Lock is on. A click switches native / Latin input in the
//! window in front; a right-click lists the keyboard layouts.
//!
//! Refreshed on foreground changes and the IME's WinEvents, plus a cheap
//! poll (one IME query on the ime worker, Caps Lock from our own key
//! state) only while the cell is on show and the bar is not hidden.

use super::*;
use crate::ime::logic;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED};
use windows::Win32::Graphics::Direct2D::{D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT};

/// One-shot: the next poll.
pub(super) const POLL_TIMER: usize = 7;
const POLL_MS: u32 = 500;
/// The indicator is one of the bar's glyphs: drawn in their box (ICON_S,
/// 16), its outline as tall as their ink (INK_PAD in from the box's top
/// and bottom, like a 16 glyph's strokes), with the same 1-DIP line. (An
/// outline filling the whole 16 looked bigger than the search and control
/// glyphs beside it.)
const GLYPH_H: f32 = crate::theme::metrics::size::ICON_S;
const INK_PAD: f32 = 1.0;
/// The label in the box: the ramp's Caption, semibold (it is a glyph of
/// the box, like the battery's number), S2 from the box's sides; the box
/// at least a glyph wide.
pub(super) const LABEL_SIZE: f32 = crate::theme::ramp::CAPTION.size;
const LABEL_PAD: f32 = crate::theme::metrics::space::S2;
const BOX_MIN_W: f32 = crate::theme::metrics::size::ICON_S;
/// The Caps Lock badge beside the box: a mark (ICON_XS, 12), a hairline
/// gap (S1) from the box.
const CAPS_W: f32 = crate::theme::metrics::size::ICON_XS;
const CAPS_GAP: f32 = crate::theme::metrics::space::S1;

/// Pure: the box's corners: a small mark's (the theme's inner radius, 4:
/// design system 1.2), never past a capsule of its height.
pub(super) fn box_radius(theme: &Theme) -> f32 {
    theme.radii().inner.min(crate::theme::capsule(GLYPH_H - 2.0 * INK_PAD))
}

/// Pure: the indicator for a label `text_w` DIP wide: (the whole width,
/// the box's left, the box's width, the badge's left), DIP. The badge
/// (`caps`) stands at the reading end: after the box, before it right to
/// left.
pub(super) fn input_box(text_w: f32, caps: bool, rtl: bool) -> (f32, f32, f32, f32) {
    let box_w = (text_w + LABEL_PAD * 2.0).max(BOX_MIN_W).ceil();
    let badge = if caps { CAPS_GAP + CAPS_W } else { 0.0 };
    let (box_x, caps_x) = if rtl { (badge, 0.0) } else { (0.0, box_w + CAPS_GAP) };
    (box_w + badge, box_x, box_w, caps_x)
}

/// What the indicator is drawn from: its label laid out (px), its
/// geometry (input_box, DIP), its colors and the scale.
pub(super) struct InputArt {
    pub layout: windows::Win32::Graphics::DirectWrite::IDWriteTextLayout,
    /// (width, box left, box width, badge left), DIP.
    pub geom: (f32, f32, f32, f32),
    pub caps: bool,
    pub radius: f32,
    pub ink: D2D1_COLOR_F,
    pub edge: D2D1_COLOR_F,
    pub scale: f32,
}

impl InputArt {
    /// The indicator's size, px.
    pub fn size_px(&self) -> (u32, u32) {
        ((self.geom.0 * self.scale).ceil() as u32, (GLYPH_H * self.scale).round().max(1.0) as u32)
    }
}

/// The indicator for `text` in the bar's `ink` at `scale`: the box a
/// little softer than the letters (full text color in high contrast).
pub(super) fn input_art(gfx: &crate::gfx::Gfx, theme: &Theme, text: &str, caps: bool, rtl: bool, ink: D2D1_COLOR_F, scale: f32) -> Option<InputArt> {
    let (layout, tw, _) = gfx.text_layout(text, &theme.font.family, LABEL_SIZE * scale, DWRITE_FONT_WEIGHT_SEMI_BOLD).ok()?;
    let edge = if theme.high_contrast { ink } else { D2D1_COLOR_F { a: ink.a * 0.9, ..ink } };
    Some(InputArt { layout, geom: input_box(tw / scale, caps, rtl), caps, radius: box_radius(theme), ink, edge, scale })
}

/// Draws the indicator `a` with its top left at (`ox`, `oy`) px: the label
/// in its rounded box, the ⇪ badge at the reading end.
pub(super) unsafe fn paint_input(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, ox: f32, oy: f32, a: &InputArt) -> Result<()> {
    let sc = a.scale;
    let (_, box_x, box_w, _) = a.geom;
    let at = ink_origin(&a.layout, ox + (box_x + box_w / 2.0) * sc, oy + GLYPH_H / 2.0 * sc);
    let at = windows_numerics::Vector2 { X: at.X.round(), Y: at.Y.round() };
    input_ink(dc, ox, oy, a, at, a.edge, a.ink)
}

/// paint_input's ink alone: the box in `edge`, the label (at `at`) and
/// the badge in `ink`.
unsafe fn input_ink(dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, ox: f32, oy: f32, a: &InputArt, at: windows_numerics::Vector2, edge: D2D1_COLOR_F, ink: D2D1_COLOR_F) -> Result<()> {
    let sc = a.scale;
    let (_, box_x, box_w, caps_x) = a.geom;
    let r = |l: f32, t: f32, rr: f32, b: f32| D2D_RECT_F { left: ox + l * sc, top: oy + t * sc, right: ox + rr * sc, bottom: oy + b * sc };
    let edge_b = dc.CreateSolidColorBrush(&edge, None)?;
    let ink_b = dc.CreateSolidColorBrush(&ink, None)?;
    let rect = r(box_x + 0.5, INK_PAD + 0.5, box_x + box_w - 0.5, GLYPH_H - INK_PAD - 0.5);
    let rad = (a.radius - 0.5).max(0.0) * sc;
    dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: rad, radiusY: rad }, &edge_b, 1.0 * sc, None);
    dc.DrawTextLayout(at, &a.layout, &ink_b, D2D1_DRAW_TEXT_OPTIONS_NONE);
    if a.caps {
        // ⇪: an arrow head over a short bar, centered in the mark's box.
        let cx = caps_x + CAPS_W / 2.0;
        let top = (GLYPH_H - CAPS_W) / 2.0;
        let factory = dc.GetFactory()?;
        let path = factory.CreatePathGeometry()?;
        let sink = path.Open()?;
        let p = |x: f32, y: f32| windows_numerics::Vector2 { X: ox + x * sc, Y: oy + (top + y) * sc };
        sink.BeginFigure(p(cx, 0.5), D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLine(p(cx + 3.6, 5.0));
        sink.AddLine(p(cx + 1.6, 5.0));
        sink.AddLine(p(cx + 1.6, 8.0));
        sink.AddLine(p(cx - 1.6, 8.0));
        sink.AddLine(p(cx - 1.6, 5.0));
        sink.AddLine(p(cx - 3.6, 5.0));
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        dc.FillGeometry(&path, &ink_b, None);
        dc.FillRectangle(&r(cx - 1.6, top + 9.5, cx + 1.6, top + 11.3), &ink_b);
    }
    Ok(())
}

impl TopBar {
    /// This bar shows the cell (the bars that have the status cells) and
    /// the settings want it.
    pub(super) fn input_here(&self) -> bool {
        self.full() && crate::ime::indicator_wanted()
    }

    /// The cell's cache key ("" = hidden).
    pub(super) fn input_key(&self) -> String {
        if !self.input_here() {
            return String::new();
        }
        let (s, caps) = crate::ime::shown();
        format!("\u{0}input|{}|{caps}|{}", logic::label(&s), crate::i18n::rtl())
    }

    /// The label in its rounded box, the badge at the reading end.
    pub(super) fn input_surface(&mut self) -> Option<(CompositionSurfaceBrush, f32, f32)> {
        let (s, caps) = crate::ime::shown();
        let text = logic::label(&s);
        let rtl = crate::i18n::rtl();
        let k = (Style::Glyph, format!("\u{0}input|{text}|{caps}|{rtl}"));
        if let Some(x) = self.surfaces.get(&k) {
            return Some(x.clone());
        }
        let sc = self.scale;
        let art = input_art(&self.gfx, &self.theme, &text, caps, rtl, self.text_color(), sc)?;
        let (w_px, h_px) = art.size_px();
        let surface = self.gfx.draw(w_px, h_px, |dc, ox, oy| unsafe { paint_input(dc, ox, oy, &art) }).ok()?;
        let brush = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface).ok()?;
        let _ = brush.SetStretch(CompositionStretch::Fill);
        let size = (w_px as f32 / sc, h_px as f32 / sc);
        self.surfaces.insert(k, (brush.clone(), size.0, size.1));
        Some((brush, size.0, size.1))
    }

    /// The poll runs (on the primary bar) only while the cell is on show.
    fn input_polls(&self) -> bool {
        self.primary && !self.hidden_full && self.input_here()
    }

    /// Arms the next poll, or stops polling (one-shot timer; arming again
    /// only moves it).
    pub(super) fn input_arm(&self) {
        if self.primary {
            // The ime engine works (hook, queries) only while it shows.
            crate::ime::set_visible(self.input_polls());
        }
        unsafe {
            if self.input_polls() {
                SetTimer(Some(self.hwnd), POLL_TIMER, POLL_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), POLL_TIMER);
            }
        }
    }

    /// The poll: the mode (on the ime worker) and Caps Lock (here).
    pub(super) fn input_tick(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), POLL_TIMER);
        }
        if !self.input_polls() {
            return;
        }
        crate::ime::poll();
        if crate::ime::read_caps() {
            // Every bar redraws (and this one re-arms) on the message.
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), crate::ime::WM_IME_STATUS, WPARAM(0), LPARAM(0));
            }
        }
        self.input_arm();
    }

    /// The screen reader's name: "Input: Hangul, Caps Lock on".
    pub(super) fn input_name(&self) -> String {
        let (s, caps) = crate::ime::shown();
        let mode = match logic::mode_key(&s) {
            Some(k) => crate::i18n::t(k).to_string(),
            None if s.lang != 0 => crate::ime::language_name(s.lang),
            None => logic::label(&s),
        };
        crate::i18n::tf(if caps { "a11y.topbar.input.caps" } else { "a11y.topbar.input" }, &[("mode", &mode)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_indicator_is_a_16_glyph_with_a_marks_corners() {
        use crate::theme::metrics::{size, space};
        let t = Theme::graphite();
        // In the glyphs' box, its outline as tall as their ink; a small
        // mark's corners (4), not a control's.
        assert_eq!((GLYPH_H, GLYPH_H - 2.0 * INK_PAD), (size::ICON_S, 14.0));
        assert_eq!(box_radius(&t), t.radii().inner);
        assert_eq!(box_radius(&t), 4.0);
        // A short label: the box a glyph wide; a longer one: S2 each side.
        assert_eq!(input_box(6.0, false, false), (16.0, 0.0, 16.0, 16.0 + space::S1));
        assert_eq!(input_box(20.0, false, false).2, 20.0 + 2.0 * space::S2);
        // The badge: a 12 mark, S1 from the box, at the reading end.
        assert_eq!(CAPS_W, size::ICON_XS);
        assert_eq!(input_box(6.0, true, false), (16.0 + 2.0 + 12.0, 0.0, 16.0, 18.0));
        assert_eq!(input_box(6.0, true, true), (30.0, 14.0, 16.0, 0.0));
        // Nothing off the grid is left in the drawing's sizes (design
        // system 5: no 10 or 14 DIP glyphs).
        let src = include_str!("input.rs");
        let body = src.split("#[cfg(test)]").next().unwrap();
        assert!(!body.contains("= 14.0;") && !body.contains("= 10.0;") && !body.contains("radii().control"));
    }

    /// Timers and messages of the bar window must not share a number
    /// (another branch adding one must pick a free one).
    #[test]
    fn bar_timers_and_messages_are_distinct() {
        let timers = [CLOCK_TIMER, ACTION_TIMER, TRAY_TIMER, TIP_TIMER, POLL_TIMER, island::HOVER_TIMER, island::LEAVE_TIMER, island::NOTICE_TIMER, island::TICK_TIMER];
        let msgs = [
            WM_MENU_READY,
            WM_BAR_RETRY,
            WM_MOUSELEAVE_MSG,
            crate::vdesk::WM_DESKTOPS,
            crate::tray::WM_TRAY_CHANGED,
            status::WM_STATUS_NET,
            status::WM_STATUS_VOL,
            crate::ime::WM_IME_STATUS,
            island::WM_ISLAND_MEDIA,
            island::WM_ISLAND_ACT,
            island::privacy::WM_ISLAND_PRIVACY,
        ];
        for (i, a) in timers.iter().enumerate() {
            assert!(!timers[i + 1..].contains(a), "timer {a} twice");
        }
        for (i, a) in msgs.iter().enumerate() {
            assert!(!msgs[i + 1..].contains(a), "message {a:#x} twice");
        }
    }
}
