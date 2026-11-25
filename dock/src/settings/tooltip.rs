//! Tooltips: a row's description, behind the small "i" after its label.
//!
//! The pages show labels only; a description that says more than its
//! label is one hover (or Tab) away: the "i" (Hit::Info) shows it in a
//! small chip next to it, fading in and out with the motion tokens
//! (motion::QUICK_IN_MS / QUICK_OUT_MS, shorter with reduced motion).
//! The mouse waits a moment first (passing over an "i" shows nothing);
//! the keyboard shows it at once. Esc puts it away until the focus moves.
//! A screen reader hears the description as the row's help either way.
//!
//! The chip is solid (the menus' surface, elevation E1: a real soft
//! shadow, a tooltip's corners (radii.tip), no outline; high contrast: the
//! system's colors with its edge). Which row it is for, where it goes and
//! how visible it is now are pure: tested.
//!
//! Whose tip it is (design B3, settings review P2 #1-#3): an "i" asked for
//! (clicked, Enter / Space, or UIA's Invoke) first, until the pointer or
//! the focus moves; then the keyboard's "i" while the keyboard is what
//! moved last; then the one under the pointer. A scroll or a resize puts
//! any tip away (the pointer is not on the "i" it was on any more).

use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ROUNDED_RECT};

use super::{rgba, win_h, win_w, App, Hit};
use crate::motion::{self, Mode};
use crate::theme::metrics::space;

/// A card's own "i" (a heading or a card that is not a row): its tip is
/// Hit::Info(CARD_TIP + i), CARD_TIPS[i] (its title's text key, its tip's).
pub(super) const CARD_TIP: usize = 1 << 20;
pub(super) const CARD_TIPS: &[(&str, &str)] = &[
    ("settings.dock.map", "settings.dock.map.hint"),
    ("settings.dock.map", "settings.dock.map.follow"),
    ("settings.calendar.feeds", "settings.calendar.feeds.hint"),
    ("settings.input.rules", "settings.input.rules.hint"),
    ("settings.pins.title", "settings.pins.tip"),
    ("settings.pins.title", "settings.pins.shared"),
    ("about.report", "about.report.hint"),
    ("settings.section.corners", "corners.hint"),
    ("welcome.taskbar.title", "welcome.taskbar.subtitle"),
    ("about.updates.auto", "about.updates.auto.hint"),
    ("settings.section.about", "about.free"),
    ("settings.tab.agents", "settings.intro.agents"),
    ("chat.ai.title", "chat.ai.privacy"),
    ("chat.ai.title", "chat.ai.privacy.agy"),
];

/// The tip id of a card's "i" whose text is `tip` (a key of CARD_TIPS).
pub(super) fn card_tip(tip: &str) -> usize {
    CARD_TIP + CARD_TIPS.iter().position(|(_, k)| *k == tip).unwrap_or_else(|| panic!("{tip}: not a card tip"))
}

/// How long the pointer rests on an "i" before its tip shows.
pub(super) const DELAY: Duration = Duration::from_millis(250);
/// The chip: widest, text size (12/16), padding, corners, the gap to the
/// "i" and the least room kept to the window's edges.
pub(super) const MAX_W: f32 = 260.0;
pub(super) const TEXT_SIZE: f32 = super::rows::tok::FOOTNOTE;
pub(super) const PAD_X: f32 = space::S3;
pub(super) const PAD_Y: f32 = space::S2;
pub(super) const GAP: f32 = space::S3;
pub(super) const MARGIN: f32 = space::S3;

/// Whose tip should show (None: none) and whether at once: `ask`, an "i"
/// asked for, first; then the keyboard's "i" (`focus`, only while the
/// keyboard is in use: `keyboard`) when it moved last (`by_key`); then the
/// pointer's (`hover`, after the wait); then the keyboard's anyway. Pure:
/// tested.
pub(super) fn target(hover: Option<usize>, focus: Option<usize>, keyboard: bool, by_key: bool, ask: Option<usize>) -> Option<(usize, bool)> {
    if let Some(ri) = ask {
        return Some((ri, true));
    }
    let key = focus.filter(|_| keyboard);
    if by_key {
        if let Some(ri) = key {
            return Some((ri, true));
        }
    }
    hover.map(|ri| (ri, false)).or(key.map(|ri| (ri, true)))
}

/// A WM_MOUSEMOVE at `pt` (client px), the pointer last seen at `last`:
/// whether it really moved. One at the same place (Windows sends those
/// after a scroll, a resize or a click) is not the pointer moving, so it
/// takes nothing from the keyboard. A real move - even a few pixels inside
/// the same "i" - hands the tips back to the pointer: the keyboard did
/// not move last any more (`by_key`) and an "i" asked for is let go
/// (`ask`) (design B3 review P2 #2). Returns whether either changed (the
/// hit under the pointer can be the same, the tip shown is not: redraw).
/// Pure: tested.
pub(super) fn pointer_moved<T>(last: &mut Option<(i32, i32)>, pt: (i32, i32), by_key: &mut bool, ask: &mut Option<T>) -> bool {
    if *last == Some(pt) {
        return false;
    }
    *last = Some(pt);
    let changed = *by_key || ask.is_some();
    *by_key = false;
    *ask = None;
    changed
}

/// A tip shown (or on its way in or out) for row `ri`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Tip {
    pub ri: usize,
    /// When it starts fading in (later than now while the pointer waits).
    pub from: Instant,
    /// Going away: since when, and how visible it was then.
    pub hide: Option<(Instant, f32)>,
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// Where a chip `size` (w, h) goes for the "i" at `anchor`, inside
/// `bounds`: after it along the reading direction (right, or left right
/// to left), its first line level with it; on the other side when it does
/// not fit there, then pushed inside the bounds (MARGIN kept). Pure: tested.
pub(super) fn place(anchor: D2D_RECT_F, size: (f32, f32), bounds: D2D_RECT_F, rtl: bool) -> D2D_RECT_F {
    let (w, h) = size;
    let (lo, hi) = (bounds.left + MARGIN, bounds.right - MARGIN);
    let after = anchor.right + GAP;
    let before = anchor.left - GAP - w;
    let fits_after = after + w <= hi;
    let fits_before = before >= lo;
    let x = match (rtl, fits_after, fits_before) {
        (false, true, _) | (true, true, false) => after,
        (true, _, true) | (false, false, true) => before,
        // Room on neither side: under the "i", kept inside.
        _ => anchor.left,
    };
    let x = x.min(hi - w).max(lo);
    let cy = (anchor.top + anchor.bottom) / 2.0;
    // The text's first line (16 DIP) centered on the "i".
    let mut y = cy - 8.0 - PAD_Y;
    let (top, bottom) = (bounds.top + MARGIN, bounds.bottom - MARGIN);
    if !fits_after && !fits_before {
        // Below it instead (it would cover the "i").
        y = anchor.bottom + 4.0;
        if y + h > bottom {
            y = anchor.top - 4.0 - h;
        }
    }
    let y = y.min(bottom - h).max(top);
    rect(x, y, x + w, y + h)
}

/// How long a fade in / out takes in `mode` (ms).
fn in_ms(mode: Mode) -> f32 {
    motion::fade_ms(mode, motion::QUICK_IN_MS) as f32
}
fn out_ms(mode: Mode) -> f32 {
    motion::fade_ms(mode, motion::QUICK_OUT_MS) as f32
}

/// How visible `tip` is at `now` (0..1), eased. Pure: tested.
pub(super) fn alpha(tip: &Tip, now: Instant, mode: Mode) -> f32 {
    let shown = |at: Instant| -> f32 {
        if at < tip.from {
            return 0.0;
        }
        let p = ((at - tip.from).as_secs_f32() * 1000.0 / in_ms(mode).max(1.0)).clamp(0.0, 1.0);
        motion::bezier(motion::EASE_OUT, p)
    };
    match tip.hide {
        None => shown(now),
        Some((since, a0)) => {
            // Out from where it was, over its share of the fade.
            let ms = (out_ms(mode) * a0).max(1.0);
            let p = (now.saturating_duration_since(since).as_secs_f32() * 1000.0 / ms).clamp(0.0, 1.0);
            a0 * (1.0 - motion::bezier(motion::EASE_IN, p))
        }
    }
}

/// Still moving at `now` (waiting, fading in or out): the motion timer
/// keeps running. Pure: tested.
pub(super) fn moving(tip: &Tip, now: Instant, mode: Mode) -> bool {
    match tip.hide {
        None => now < tip.from + Duration::from_secs_f32(in_ms(mode) / 1000.0),
        Some(_) => alpha(tip, now, mode) > 0.0,
    }
}

/// What the tip becomes at `now` when row `want` should show one (None:
/// no row): the same tip goes on; another one starts (at once when
/// `quick`, else after DELAY; at once too when a tip is still up, so
/// moving between "i"s does not wait again); none fades out from where it
/// is (gone at once if it had not shown yet). Pure: tested.
pub(super) fn step(cur: Option<Tip>, want: Option<usize>, quick: bool, now: Instant, mode: Mode) -> Option<Tip> {
    step_after(cur, want, quick, now, mode, DELAY)
}

/// step, the pointer waiting `delay` (a widget's how-to card waits
/// longer than an "i": howto.rs). Pure: tested through step.
pub(super) fn step_after(cur: Option<Tip>, want: Option<usize>, quick: bool, now: Instant, mode: Mode, delay: Duration) -> Option<Tip> {
    let up = cur.map(|t| alpha(&t, now, mode)).unwrap_or(0.0);
    match (cur, want) {
        (Some(t), Some(ri)) if t.ri == ri => match t.hide {
            // Still waiting and now asked for at once (clicked, or the
            // keyboard came to it): from now (settings review P3 #4). One
            // already showing keeps its fade.
            None if quick && t.from > now => Some(Tip { from: now, ..t }),
            None => Some(t),
            // Coming back while it fades: in again from where it is.
            Some(_) => {
                let back = Duration::from_secs_f32(in_ms(mode) / 1000.0 * up);
                Some(Tip { ri, from: now.checked_sub(back).unwrap_or(now), hide: None })
            }
        },
        (_, Some(ri)) => {
            let wait = if quick || up > 0.0 { Duration::ZERO } else { delay };
            Some(Tip { ri, from: now + wait, hide: None })
        }
        (Some(t), None) => match t.hide {
            None if up <= 0.0 => None,
            None => Some(Tip { hide: Some((now, up)), ..t }),
            Some(_) if up <= 0.0 => None,
            Some(_) => Some(t),
        },
        (None, None) => None,
    }
}

impl App {
    /// What tip `id` says: a row's description, or a card's (CARD_TIPS).
    pub(super) fn tip_text(&self, id: usize) -> String {
        if let Some((_, k)) = id.checked_sub(CARD_TIP).and_then(|i| CARD_TIPS.get(i)) {
            return crate::i18n::t(k).to_string();
        }
        self.rows().get(id).map(|r| self.row_hint(r).to_string()).unwrap_or_default()
    }

    /// What tip `id`'s "i" is about (its row's label, its card's title).
    pub(super) fn tip_title(&self, id: usize) -> String {
        if let Some((k, _)) = id.checked_sub(CARD_TIP).and_then(|i| CARD_TIPS.get(i)) {
            return crate::i18n::t(k).to_string();
        }
        self.rows().get(id).map(|r| self.row_label(r)).unwrap_or_default()
    }

    /// The row whose tip should show now (None: none), before Esc's
    /// "put it away": the "i" under the pointer, else the one the keyboard
    /// is on; never under an open list, a question or the first-run flow.
    fn tip_wanted(&self) -> Option<(usize, bool)> {
        if self.list_open() || self.recording.is_some() {
            return None;
        }
        let info = |h: Option<Hit>| match h {
            Some(Hit::Info(ri)) => Some(ri),
            _ => None,
        };
        target(info(self.hover), info(self.focus), self.keyboard, self.tip_key, self.tip_ask.map(|a| a.0))
    }

    /// Brings the tip up to date (each redraw), and keeps the motion
    /// timer running while it moves.
    pub(super) fn sync_tip(&mut self) {
        let now = Instant::now();
        let mode = self.motion_mode();
        // An "i" asked for holds while the pointer and the focus stay
        // where they were when it was asked (noted at its first sync).
        if let Some((ri, at)) = self.tip_ask {
            let here = (self.hover, self.focus);
            match at {
                None => self.tip_ask = Some((ri, Some(here))),
                Some(was) if was != here => self.tip_ask = None,
                Some(_) => {}
            }
        }
        let want = self.tip_wanted();
        // Esc's "put it away" lasts until another tip is wanted.
        if want.map(|w| w.0) != self.tip_quiet {
            self.tip_quiet = None;
        }
        let want = want.filter(|w| Some(w.0) != self.tip_quiet);
        let quick = want.is_some_and(|w| w.1) || self.tip_now;
        self.tip_now = false;
        self.tip = step(self.tip, want.map(|w| w.0), quick, now, mode);
        if self.tip_moving() {
            self.anim_timer();
        }
    }

    /// The tip waits or fades (the motion timer runs on).
    pub(super) fn tip_moving(&self) -> bool {
        self.tip.is_some_and(|t| moving(&t, Instant::now(), self.motion_mode()))
    }

    /// A tip is up (and not going away): Esc puts it away first.
    pub(super) fn tip_up(&self) -> bool {
        self.tip.is_some_and(|t| t.hide.is_none() && alpha(&t, Instant::now(), self.motion_mode()) > 0.0)
    }

    /// Esc on a tip: away until the focus or pointer moves on.
    pub(super) fn quiet_tip(&mut self) {
        self.tip_quiet = self.tip.map(|t| t.ri);
    }

    /// An "i" clicked (or Enter / Space on it, or UIA's Invoke on its
    /// node): row `ri`'s tip at once, whatever the pointer or the focus is
    /// on (settings review P2 #3).
    pub(super) fn show_tip(&mut self, ri: usize) {
        if self.tip_quiet == Some(ri) {
            self.tip_quiet = None;
        }
        self.tip_ask = Some((ri, None));
        self.tip_now = true;
    }

    /// A WM_MOUSEMOVE at `pt` (client px): a real move gives the tips back
    /// to the pointer (pointer_moved). Returns whether to redraw for it.
    pub(super) fn pointer_at(&mut self, pt: (i32, i32)) -> bool {
        pointer_moved(&mut self.last_pt, pt, &mut self.tip_key, &mut self.tip_ask)
    }

    /// The page moved under the pointer (a scroll, a resize): the "i" the
    /// tip was for is not under it any more. The pointer's "i" is
    /// forgotten until it moves (a waiting tip goes at once, one shown
    /// fades), and a keyboard tip is put away until the focus moves (its
    /// "i" may have scrolled out of sight) (settings review P2 #2).
    pub(super) fn page_moved(&mut self) {
        if matches!(self.hover, Some(Hit::Info(_))) {
            self.hover = None;
        }
        self.tip_ask = None;
        if let Some(t) = self.tip {
            self.tip_quiet = Some(t.ri);
        }
        self.tip = step(self.tip, None, false, Instant::now(), self.motion_mode());
        self.howto_page_moved();
    }

    /// The tip over everything (after the open list): the chip next to
    /// its "i" as drawn now (`hits`); nothing when the "i" is out of sight.
    pub(super) unsafe fn paint_tip(&self, dc: &ID2D1DeviceContext, hits: &[(D2D_RECT_F, Hit)]) -> Result<()> {
        let Some(tip) = self.tip else { return Ok(()) };
        let a = alpha(&tip, Instant::now(), self.motion_mode());
        if a <= 0.001 {
            return Ok(());
        }
        let Some(anchor) = hits.iter().find(|(_, h)| *h == Hit::Info(tip.ri)).map(|(r, _)| *r) else { return Ok(()) };
        let text = self.tip_text(tip.ri);
        let text = text.as_str();
        if text.is_empty() {
            return Ok(());
        }
        let rtl = crate::i18n::rtl();
        let size = self.chip_size(text);
        let r = place(anchor, size, rect(0.0, 0.0, win_w(), win_h()), rtl);
        self.chip(dc, r, text, a)
    }

    /// A tip chip's size for `text` (wrapped at MAX_W).
    pub(super) fn chip_size(&self, text: &str) -> (f32, f32) {
        let tw = (self.text_w(text, TEXT_SIZE, false) + 2.0).min(MAX_W - PAD_X * 2.0);
        (tw + PAD_X * 2.0, self.para_h(text, TEXT_SIZE, tw) + PAD_Y * 2.0)
    }

    /// A tip's chip with `text` in `r`, `a` visible: the menus' surface,
    /// opaque, on its shadow (the "i"s' tips, the top bar picture's
    /// names).
    pub(super) unsafe fn chip(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, text: &str, a: f32) -> Result<()> {
        let rtl = crate::i18n::rtl();
        let tw = r.right - r.left - PAD_X * 2.0;
        let fade = |c: D2D1_COLOR_F| D2D1_COLOR_F { a: c.a * a, ..c };
        // The menus' surface, opaque (the dock's name label's colors).
        let (bg, ink) = match self.hc {
            Some(h) => (rgba(h.window), rgba(h.text)),
            None => (rgba(self.look.label_colors().0), self.ink()),
        };
        let radius = self.look.radii().tip;
        // Elevation E1, a real blur (none in high contrast or with the
        // "Shadows" effect off: the shared tooltip's), faded with it.
        let layers: Vec<_> = crate::topbar::tip_shadows(&self.look, crate::effects::items().shadows).into_iter().map(|(x, y, b, c)| (x, y, b, crate::theme::Rgba { a: (c.a as f32 * a).round() as u8, ..c })).collect();
        crate::gfx::soft_shadows(dc, r, radius, &layers)?;
        self.fill_round(dc, fade(bg), r, radius)?;
        if let Some(h) = self.hc {
            let b = dc.CreateSolidColorBrush(&fade(rgba(h.text)), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), radiusX: radius, radiusY: radius }, &b, 1.0, None);
        }
        self.para_dir(dc, text, r.left + PAD_X, r.top + PAD_Y, TEXT_SIZE, false, tw, fade(ink), rtl)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: D2D_RECT_F = D2D_RECT_F { left: 0.0, top: 0.0, right: 820.0, bottom: 620.0 };

    fn at(x: f32, y: f32) -> D2D_RECT_F {
        rect(x - 10.0, y - 10.0, x + 10.0, y + 10.0)
    }

    #[test]
    fn a_tip_goes_after_its_i_along_the_reading_direction() {
        let r = place(at(400.0, 200.0), (200.0, 40.0), WIN, false);
        assert_eq!(r.left, 410.0 + GAP, "after the i");
        // Its first line level with the i.
        assert_eq!(r.top, 200.0 - 8.0 - PAD_Y);
        assert_eq!((r.right - r.left, r.bottom - r.top), (200.0, 40.0));
        // Right to left: before it (to its left).
        let r = place(at(400.0, 200.0), (200.0, 40.0), WIN, true);
        assert_eq!(r.right, 390.0 - GAP);
    }

    #[test]
    fn a_tip_flips_and_stays_inside_the_window() {
        // No room after the i: before it.
        let r = place(at(760.0, 200.0), (200.0, 40.0), WIN, false);
        assert_eq!(r.right, 750.0 - GAP);
        // Right to left near the left edge: after it.
        let r = place(at(60.0, 200.0), (200.0, 40.0), WIN, true);
        assert_eq!(r.left, 70.0 + GAP);
        // Near the top and the bottom: pushed in.
        let r = place(at(400.0, 4.0), (200.0, 40.0), WIN, false);
        assert_eq!(r.top, MARGIN);
        let r = place(at(400.0, 615.0), (200.0, 60.0), WIN, false);
        assert_eq!(r.bottom, 620.0 - MARGIN);
        // Too wide for either side: under the i, inside the window.
        let r = place(at(400.0, 200.0), (600.0, 40.0), WIN, false);
        assert!(r.left >= MARGIN && r.right <= 820.0 - MARGIN, "{r:?}");
        assert_eq!(r.top, 210.0 + 4.0, "under it, not over it");
        let r = place(at(400.0, 600.0), (600.0, 40.0), WIN, false);
        assert_eq!(r.bottom, 590.0 - 4.0, "over it when there is no room under");
        // Wider than the window: still starts inside.
        let r = place(at(100.0, 200.0), (900.0, 40.0), WIN, false);
        assert_eq!(r.left, MARGIN);
    }

    #[test]
    fn a_tip_waits_for_the_pointer_then_fades_in_and_out() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // The pointer: nothing during the wait, then in over 90 ms.
        let tip = step(None, Some(3), false, t0, Mode::Full).unwrap();
        assert_eq!(tip.from, t0 + DELAY);
        assert_eq!(alpha(&tip, ms(100), Mode::Full), 0.0);
        assert!(moving(&tip, ms(100), Mode::Full), "the timer runs through the wait");
        let half = alpha(&tip, ms(250 + 45), Mode::Full);
        assert!(half > 0.3 && half < 1.0, "{half}");
        assert_eq!(alpha(&tip, ms(250 + 95), Mode::Full), 1.0);
        assert!(!moving(&tip, ms(250 + 95), Mode::Full));
        // The keyboard: at once.
        let tip = step(None, Some(3), true, t0, Mode::Full).unwrap();
        assert_eq!(tip.from, t0);
        // Left before it showed: gone at once.
        let waiting = step(None, Some(3), false, t0, Mode::Full);
        assert_eq!(step(waiting, None, false, ms(100), Mode::Full), None);
        // Left once shown: out over 120 ms from where it was.
        let shown = Some(Tip { ri: 3, from: t0, hide: None });
        let going = step(shown, None, false, ms(200), Mode::Full).unwrap();
        assert_eq!(going.hide, Some((ms(200), 1.0)));
        assert_eq!(alpha(&going, ms(200), Mode::Full), 1.0);
        assert!(alpha(&going, ms(260), Mode::Full) < 1.0);
        assert_eq!(alpha(&going, ms(325), Mode::Full), 0.0);
        assert!(!moving(&going, ms(325), Mode::Full));
        assert_eq!(step(Some(going), None, false, ms(325), Mode::Full), None, "then dropped");
        // Back on it while it fades: in again from where it is, no wait.
        let back = step(Some(going), Some(3), false, ms(260), Mode::Full).unwrap();
        assert!(back.hide.is_none() && back.from <= ms(260));
        // Over to another i while one is up: no wait either.
        let next = step(shown, Some(5), false, ms(200), Mode::Full).unwrap();
        assert_eq!((next.ri, next.from), (5, ms(200)));
        // The same i: unchanged.
        assert_eq!(step(shown, Some(3), false, ms(500), Mode::Full), shown);
    }

    #[test]
    fn the_keyboard_s_i_wins_over_a_pointer_left_on_another() {
        // Review P2 #1: the pointer rests on A (row 1), Tab goes to B's "i"
        // (row 4): B's tip, at once.
        assert_eq!(target(Some(1), Some(4), true, true, None), Some((4, true)));
        // The pointer moved last (onto C): C's, after the wait.
        assert_eq!(target(Some(7), Some(4), true, false, None), Some((7, false)));
        // Keyboard in use, pointer nowhere: the focus's.
        assert_eq!(target(None, Some(4), true, false, None), Some((4, true)));
        // Focus on an "i" but the keyboard not in use (no ring): nothing.
        assert_eq!(target(None, Some(4), false, false, None), None);
        // Tab to something that is not an "i": the pointer's stays.
        assert_eq!(target(Some(1), None, true, true, None), Some((1, false)));
        // An "i" asked for wins over both.
        assert_eq!(target(Some(1), Some(4), true, true, Some(9)), Some((9, true)));
        assert_eq!(target(None, None, false, false, Some(9)), Some((9, true)), "UIA Invoke, no hover, no focus");
    }

    #[test]
    fn moving_the_pointer_inside_the_same_i_takes_the_tip_back() {
        // Design B3 review P2 #2, as the window's WM_MOUSEMOVE runs it
        // (pointer_at, then tip_wanted's target).
        let (mut last, mut by_key, mut ask): (Option<(i32, i32)>, bool, Option<usize>) = (None, false, None);
        let want = |by_key: bool, ask: Option<usize>| target(Some(1), Some(4), true, by_key, ask);
        // The pointer comes to A's "i" (row 1).
        assert!(!pointer_moved(&mut last, (100, 50), &mut by_key, &mut ask), "nothing to hand back");
        // Tab to B's "i" (row 4): B's tip.
        by_key = true;
        assert_eq!(want(by_key, ask), Some((4, true)));
        // A move to the same place (after a scroll or a click): still B's.
        assert!(!pointer_moved(&mut last, (100, 50), &mut by_key, &mut ask));
        assert_eq!(want(by_key, ask), Some((4, true)));
        // A few pixels inside A's "i" (the hit is the same): A's, redrawn.
        assert!(pointer_moved(&mut last, (103, 51), &mut by_key, &mut ask));
        assert_eq!(want(by_key, ask), Some((1, false)));
        // Moving on: nothing more to hand back.
        assert!(!pointer_moved(&mut last, (104, 51), &mut by_key, &mut ask));
        // Enter on B's "i" (asked for): B's, until the pointer really moves.
        ask = Some(4);
        assert!(!pointer_moved(&mut last, (104, 51), &mut by_key, &mut ask));
        assert_eq!(want(by_key, ask), Some((4, true)));
        assert!(pointer_moved(&mut last, (106, 52), &mut by_key, &mut ask));
        assert_eq!((ask, want(by_key, ask)), (None, Some((1, false))));
        // Back after leaving the window (last forgotten): a real move.
        let mut gone = None;
        by_key = true;
        assert!(pointer_moved(&mut gone, (106, 52), &mut by_key, &mut ask));
    }

    #[test]
    fn esc_puts_one_tip_away_and_the_next_i_shows() {
        // Review P2 #1: Esc on A's tip, then Tab to B: B is wanted, and is
        // not A (the quiet one), so it shows.
        let quiet = Some(1);
        let want = target(Some(1), Some(4), true, true, None);
        assert_ne!(want.map(|w| w.0), quiet);
        // Still on A (nothing moved): A stays away.
        assert_eq!(target(Some(1), None, false, false, None).map(|w| w.0), quiet);
    }

    #[test]
    fn a_click_on_a_waiting_i_shows_it_now() {
        // Review P3 #4: entered at 0 (shows at 250), clicked at 50.
        let t0 = Instant::now();
        let waiting = step(None, Some(3), false, t0, Mode::Full);
        let at50 = t0 + Duration::from_millis(50);
        let now = step(waiting, Some(3), true, at50, Mode::Full).unwrap();
        assert_eq!(now.from, at50, "no more wait");
        assert!(alpha(&now, at50 + Duration::from_millis(100), Mode::Full) > 0.9);
        // The keyboard coming to the same waiting "i": the same.
        assert_eq!(step(waiting, Some(3), true, at50, Mode::Full).unwrap().from, at50);
        // One already showing keeps its fade (no restart).
        let shown = Some(Tip { ri: 3, from: t0, hide: None });
        assert_eq!(step(shown, Some(3), true, t0 + Duration::from_millis(40), Mode::Full), shown);
    }

    #[test]
    fn a_scroll_drops_a_waiting_tip_and_fades_a_shown_one() {
        // Review P2 #2, as page_moved runs it: the wanted row goes (None).
        let t0 = Instant::now();
        let waiting = step(None, Some(3), false, t0, Mode::Full);
        assert_eq!(step(waiting, None, false, t0 + Duration::from_millis(100), Mode::Full), None, "never shows later");
        let shown = Some(Tip { ri: 3, from: t0, hide: None });
        let going = step(shown, None, false, t0 + Duration::from_millis(400), Mode::Full).unwrap();
        assert!(going.hide.is_some(), "fades from where it was");
        // Back over the same "i" after the scroll: it waits again (a new
        // tip once the old one is gone), it does not come straight back.
        let gone = step(Some(going), None, false, t0 + Duration::from_millis(600), Mode::Full);
        assert_eq!(gone, None);
        let again = step(gone, Some(3), false, t0 + Duration::from_millis(700), Mode::Full).unwrap();
        assert_eq!(again.from, t0 + Duration::from_millis(700) + DELAY);
    }

    #[test]
    fn the_chip_s_shadow_follows_the_shadows_effect() {
        // Design B2 review P3 #3, here too: the shared tooltip's shadows
        // (none with "Shadows" off), read from the effect when painted.
        let body = include_str!("tooltip.rs").split("#[cfg(test)]").next().unwrap().to_string();
        assert!(body.contains("crate::topbar::tip_shadows(&self.look, crate::effects::items().shadows)"));
        assert!(!body.contains(".elevation(crate::theme::Elevation::E1)"), "not E1 whatever the effect");
    }

    #[test]
    fn the_chip_is_on_the_grid() {
        for v in [PAD_X, PAD_Y, GAP, MARGIN] {
            assert_eq!(v % 4.0, 0.0, "{v}");
        }
        assert_eq!(TEXT_SIZE, crate::theme::ramp::FOOTNOTE.size);
    }

    #[test]
    fn reduced_motion_keeps_the_fades_short() {
        let t0 = Instant::now();
        let tip = Tip { ri: 0, from: t0, hide: None };
        let r = motion::REDUCED_MS as u64 + 5;
        assert_eq!(alpha(&tip, t0 + Duration::from_millis(r), Mode::Reduced), 1.0);
        let going = Tip { hide: Some((t0, 1.0)), ..tip };
        assert_eq!(alpha(&going, t0 + Duration::from_millis(r), Mode::Reduced), 0.0);
    }
}
