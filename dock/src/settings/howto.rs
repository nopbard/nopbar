//! A widget row's how-to card (General > Widgets): with the pointer
//! resting on the notes' or the image pieces' row
//! (HOLD, like a tooltip), or the keyboard on its switch, a small card
//! under the row (above when there is no room; never over the row and
//! its switch) plays how that widget works in a few seconds, on a loop:
//! a tiny desktop drawn by code (no pictures), achromatic, light or dark,
//! the keycaps of the widget's shortcut as set now (none when it is off).
//!
//! It is decorative: no hit, no UIA node (the row's description says it
//! in words). It goes on leaving the row, Esc, a scroll or a resize.
//! Without full motion (Windows' animations off, NopBar's small ones off,
//! Low-spec mode) it shows one still frame and nothing runs; hidden,
//! nothing runs either (the motion timer stops: App::howto_moving).
//!
//! What is drawn when is pure (`scene`): a list of shapes for a moment of
//! the loop, tested; App::paint_howto_card draws them.

use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ELLIPSE, D2D1_ROUNDED_RECT};
use windows_numerics::Vector2;

use super::tooltip;
use super::{win_h, win_w, App, Hit};
use crate::desklayer::load::Kind;
use crate::motion::Mode;

/// How long the pointer rests on a row before its card shows.
pub(super) const HOLD: Duration = Duration::from_millis(400);
/// The card (DIP).
pub(super) const W: f32 = 280.0;
pub(super) const H: f32 = 180.0;
/// One loop.
pub(super) const LOOP: Duration = Duration::from_millis(4000);
/// Between the row and the card; kept from the window's edges.
const GAP: f32 = 6.0;
const MARGIN: f32 = 12.0;
/// The row's text starts this far in (rows::tok::ROW_PAD).
const INSET: f32 = super::rows::tok::ROW_PAD;

/// A box in the card (left, top, right, bottom; DIP from its corner).
pub(super) type Bx = [f32; 4];

/// The tiny desktop's tones (App::howto_tone: greys for light or dark).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Tone {
    Desk,
    Window,
    Bar,
    Paper,
    Strip,
    Image,
    ImageHi,
    Line,
    Ink,
    Scrim,
    Flash,
}

/// One shape of a moment (`a`: how visible, 0..1).
#[derive(Clone, PartialEq, Debug)]
pub(super) enum Op {
    Fill { b: Bx, rad: f32, tone: Tone, a: f32 },
    Stroke { b: Bx, rad: f32, tone: Tone, a: f32 },
    Shadow { b: Bx, rad: f32, a: f32 },
    /// Keycaps centered on `cx` from `top`; `down`: pressed (0..1).
    Keys { cx: f32, top: f32, keys: Vec<String>, a: f32, down: f32 },
    /// The pointer as a dot; `click`: the ring of a click (0..1, none at 0).
    Pointer { x: f32, y: f32, a: f32, click: f32 },
    Clip(Bx),
    Unclip,
}

/// The tiny desktop in the card.
pub(super) const STAGE: Bx = [10.0, 10.0, W - 10.0, H - 10.0];
const KEYS_TOP: f32 = 146.0;

fn seg(t: f32, a: f32, b: f32) -> f32 {
    ((t - a) / (b - a)).clamp(0.0, 1.0)
}

/// Calm in and out (smoothstep).
fn ease(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

/// In over a..b, out over c..d (eased).
fn pulse(t: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    ease(seg(t, a, b)) * (1.0 - ease(seg(t, c, d)))
}

fn lerp(a: f32, b: f32, p: f32) -> f32 {
    a + (b - a) * p
}

fn lerp_b(a: Bx, b: Bx, p: f32) -> Bx {
    [lerp(a[0], b[0], p), lerp(a[1], b[1], p), lerp(a[2], b[2], p), lerp(a[3], b[3], p)]
}

fn at(b: Bx, fx0: f32, fy0: f32, fx1: f32, fy1: f32) -> Bx {
    let (w, h) = (b[2] - b[0], b[3] - b[1]);
    [b[0] + w * fx0, b[1] + h * fy0, b[0] + w * fx1, b[1] + h * fy1]
}

fn moved(b: Bx, dx: f32, dy: f32) -> Bx {
    [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy]
}

fn fill(ops: &mut Vec<Op>, b: Bx, rad: f32, tone: Tone, a: f32) {
    if a > 0.002 {
        ops.push(Op::Fill { b, rad, tone, a });
    }
}

fn shadow(ops: &mut Vec<Op>, b: Bx, rad: f32, a: f32) {
    if a > 0.002 {
        ops.push(Op::Shadow { b, rad, a });
    }
}

/// An app's window: its title bar a step darker.
fn window(ops: &mut Vec<Op>, w: Bx, a: f32) {
    fill(ops, w, 6.0, Tone::Window, a);
    fill(ops, [w[0], w[1], w[2], w[1] + 12.0], 6.0, Tone::Bar, a);
    fill(ops, [w[0], w[1] + 8.0, w[2], w[1] + 12.0], 0.0, Tone::Bar, a);
}

/// Lines of text from (x, y), one every 12 DIP.
fn lines(ops: &mut Vec<Op>, x: f32, y: f32, widths: &[f32], a: f32) {
    for (i, w) in widths.iter().enumerate() {
        let y = y + i as f32 * 12.0;
        fill(ops, [x, y, x + w, y + 4.0], 2.0, Tone::Line, a);
    }
}

/// The shortcut's keycaps (none when it is off).
fn keys(ops: &mut Vec<Op>, cx: f32, k: &[String], a: f32, down: f32) {
    if !k.is_empty() && a > 0.002 {
        ops.push(Op::Keys { cx, top: KEYS_TOP, keys: k.to_vec(), a, down });
    }
}

/// A key's press: down over a..b, up over c..d.
fn press(t: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    pulse(t, a, b, c, d)
}

/// What the card shows at `t` (0..1 of the loop) for widget `k`, its
/// shortcut's keys `hot` (empty: off, no keycaps). Pure: tested.
pub(super) fn scene(k: Kind, t: f32, hot: &[String]) -> Vec<Op> {
    let t = t.rem_euclid(1.0);
    let mut ops = Vec::new();
    fill(&mut ops, STAGE, 8.0, Tone::Desk, 1.0);
    match k {
        Kind::Notes => notes(&mut ops, t, hot),
        Kind::Pieces => pieces(&mut ops, t, hot),
    }
    ops
}

/// A sticky note in `b`: its lines (`typed`: of the two written now, how
/// much; `fresh`: how visible those two are).
fn note(ops: &mut Vec<Op>, b: Bx, lift: f32, typed: f32, fresh: f32, caret: f32) {
    shadow(ops, b, 4.0, 0.45 + 0.55 * lift);
    fill(ops, b, 4.0, Tone::Paper, 1.0);
    fill(ops, at(b, 0.0, 0.0, 1.0, 0.16), 4.0, Tone::Strip, 1.0);
    fill(ops, at(b, 0.0, 0.10, 1.0, 0.16), 0.0, Tone::Strip, 1.0);
    let widths = [0.72, 0.52, 0.78, 0.46];
    for (i, w) in widths.iter().enumerate() {
        let y = 0.30 + i as f32 * 0.15;
        let (part, a) = match i {
            0 | 1 => (1.0, 1.0),
            _ => ((typed * 2.0 - (i - 2) as f32).clamp(0.0, 1.0), fresh),
        };
        if part > 0.0 {
            fill(ops, at(b, 0.1, y, 0.1 + w * part, y + 0.06), 1.5, Tone::Line, a * 1.6);
        }
        // The caret at the end of the line being written.
        if i >= 2 && part > 0.0 && part < 1.0 && caret > 0.0 {
            let x = at(b, 0.1 + w * part, y - 0.03, 0.1 + w * part, y + 0.09);
            fill(ops, [x[0] + 0.5, x[1], x[0] + 1.7, x[3]], 0.0, Tone::Ink, caret);
        }
    }
}

/// Notes: the shortcut, a note rises from the desktop to the front, a
/// few lines are written in it, it goes back down to the desktop.
fn notes(ops: &mut Vec<Op>, t: f32, hot: &[String]) {
    let win: Bx = [52.0, 22.0, 246.0, 134.0];
    let rest: Bx = [16.0, 98.0, 70.0, 140.0];
    let front: Bx = [92.0, 34.0, 200.0, 120.0];
    let up = ease(seg(t, 0.16, 0.34)) - ease(seg(t, 0.72, 0.90));
    let b = lerp_b(rest, front, up);
    let typed = seg(t, 0.38, 0.64);
    let fresh = 1.0 - ease(seg(t, 0.92, 0.99));
    let caret = if (0.38..0.66).contains(&t) { 0.8 } else { 0.0 };
    // On the desktop it is under the windows; called up, over them.
    let over = up > 0.35;
    if !over {
        note(ops, b, up, typed, fresh, caret);
    }
    window(ops, win, 1.0);
    lines(ops, 64.0, 46.0, &[140.0, 110.0, 150.0, 92.0, 124.0, 70.0], 1.0);
    if over {
        note(ops, b, up, typed, fresh, caret);
    }
    keys(ops, 140.0, hot, pulse(t, 0.02, 0.08, 0.20, 0.26), press(t, 0.10, 0.13, 0.17, 0.20));
}

/// The window the piece is cut from, its content moved by (dx, dy).
fn piece_window(ops: &mut Vec<Op>, dx: f32, dy: f32, a: f32) {
    window(ops, moved([28.0, 22.0, 232.0, 140.0], dx, dy), a);
    fill(ops, moved([42.0, 40.0, 122.0, 98.0], dx, dy), 3.0, Tone::Image, a);
    fill(ops, moved([90.0, 48.0, 106.0, 64.0], dx, dy), 8.0, Tone::ImageHi, a);
    fill(ops, moved([48.0, 76.0, 92.0, 98.0], dx, dy), 2.0, Tone::ImageHi, a * 0.8);
    lines(ops, 134.0 + dx, 44.0 + dy, &[84.0, 64.0, 80.0, 52.0, 72.0], a);
    lines(ops, 42.0 + dx, 110.0 + dy, &[150.0, 110.0], a);
}

/// Image pieces: the shortcut, the screen dims and a part of it is
/// picked; the piece stays pinned where it was, the window goes on.
fn pieces(ops: &mut Vec<Op>, t: f32, hot: &[String]) {
    let from = (36.0, 34.0);
    let to = (128.0, 106.0);
    let back = ease(seg(t, 0.90, 0.98));
    let win_a = (1.0 - ease(seg(t, 0.62, 0.74)) + back).min(1.0);
    piece_window(ops, 0.0, 0.0, win_a);
    // Picking: the screen dims, the part dragged out stays clear.
    let drag = ease(seg(t, 0.24, 0.48));
    let sel: Bx = [from.0, from.1, lerp(from.0, to.0, drag), lerp(from.1, to.1, drag)];
    let picking = (0.24..0.58).contains(&t);
    let dim = 0.42 * ease(seg(t, 0.14, 0.22)) * (1.0 - ease(seg(t, 0.50, 0.58)));
    fill(ops, STAGE, 8.0, Tone::Scrim, dim);
    if picking && sel[2] > sel[0] + 0.5 {
        // The part picked: clear of the dimming (drawn again inside it).
        ops.push(Op::Clip(sel));
        fill(ops, STAGE, 8.0, Tone::Desk, 1.0);
        piece_window(ops, 0.0, 0.0, win_a);
        ops.push(Op::Unclip);
        ops.push(Op::Stroke { b: sel, rad: 0.0, tone: Tone::Flash, a: 1.0 - ease(seg(t, 0.50, 0.58)) });
    }
    // The piece: lifted a little off the window, pinned there.
    if t >= 0.50 {
        let lift = ease(seg(t, 0.50, 0.60));
        let a = 1.0 - back;
        if a > 0.002 {
            let (dx, dy) = (-1.0 * lift, -2.0 * lift);
            let p = moved([from.0, from.1, to.0, to.1], dx, dy);
            shadow(ops, p, 2.0, lift * a);
            ops.push(Op::Clip(p));
            fill(ops, p, 0.0, Tone::Window, a);
            piece_window(ops, dx, dy, a);
            ops.push(Op::Unclip);
            ops.push(Op::Stroke { b: p, rad: 2.0, tone: Tone::Line, a });
        }
    }
    if (0.24..0.50).contains(&t) {
        ops.push(Op::Pointer { x: sel[2], y: sel[3], a: pulse(t, 0.22, 0.26, 0.46, 0.50), click: 0.0 });
    }
    keys(ops, 130.0, hot, pulse(t, 0.02, 0.07, 0.18, 0.23), press(t, 0.09, 0.11, 0.14, 0.16));
}

/// The still frame shown without full motion: the widget at work.
pub(super) fn still(k: Kind) -> f32 {
    match k {
        Kind::Notes => 0.56,
        Kind::Pieces => 0.85,
    }
}

/// Where the card goes for the row drawn at `row`: under it, in line with
/// its label (right to left: its right end), else above it, else on the
/// side with more room; inside `bounds` (MARGIN kept). Never over the row
/// when there is room above or below. Pure: tested.
pub(super) fn place(row: D2D_RECT_F, size: (f32, f32), bounds: D2D_RECT_F, rtl: bool) -> D2D_RECT_F {
    let (w, h) = size;
    let x = if rtl { row.right - INSET - w } else { row.left + INSET };
    let x = x.min(bounds.right - MARGIN - w).max(bounds.left + MARGIN);
    let (top, bottom) = (bounds.top + MARGIN, bounds.bottom - MARGIN);
    let below = row.bottom + GAP;
    let above = row.top - GAP - h;
    let y = if below + h <= bottom {
        below
    } else if above >= top {
        above
    } else if bottom - row.bottom >= row.top - top {
        below.min(bottom - h)
    } else {
        above.max(top)
    };
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}

impl App {
    /// The widget row (its index) under the pointer at (`x`, `y`) (DIP).
    pub(super) fn howto_row_at(&self, x: f32, y: f32) -> Option<usize> {
        self.howto_rows.borrow().iter().find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(ri, _)| *ri)
    }

    /// The widget row whose card should show (and whether at once): the
    /// keyboard's switch while it moved last, else the row under the
    /// pointer, else the keyboard's (tooltip::target); only a row shown
    /// now (not one the search or its switch hides). None with the window
    /// not in front (howto_away), over a list, a recording, a drag, the
    /// first-run flow, or in high contrast (the card is a picture; the row
    /// says it in words).
    fn howto_wanted(&self) -> Option<(usize, bool)> {
        if self.howto_away || self.welcome.is_some() || self.list_open() || self.recording.is_some() || self.dragging.is_some() || self.hc.is_some() {
            return None;
        }
        let rows = self.rows();
        let slots = self.row_slots(rows);
        let shown = |ri: usize| matches!(slots.get(ri), Some(super::rows::Slot::Row { .. }));
        let widget = |ri: usize| rows.get(ri).filter(|r| super::widgets::kind_of(r.key).is_some() && shown(ri)).map(|_| ri);
        let focus = match self.focus {
            Some(Hit::Toggle(ri)) => widget(ri),
            _ => None,
        };
        tooltip::target(self.howto_hover.and_then(widget), focus, self.keyboard, self.tip_key, None)
    }

    /// Brings the card up to date (each redraw): waiting HOLD for the
    /// pointer, fading like a tooltip; the motion timer runs while it is
    /// up and moving.
    pub(super) fn sync_howto(&mut self) {
        self.sync_howto_at(Instant::now());
    }

    /// sync_howto at `now` (tests give the time).
    pub(super) fn sync_howto_at(&mut self, now: Instant) {
        let want = self.howto_wanted();
        if want.map(|w| w.0) != self.howto_quiet {
            self.howto_quiet = None;
        }
        let want = want.filter(|w| Some(w.0) != self.howto_quiet);
        self.howto = tooltip::step_after(self.howto, want.map(|w| w.0), want.is_some_and(|w| w.1), now, self.motion_mode(), HOLD);
        if self.howto_moving_at(now) {
            self.anim_timer();
        }
    }

    /// The window went out of front (switched away, minimized, hidden:
    /// `away`) or came back. Away, the card goes at once (no fade) and
    /// nothing of it runs; the shared motion timer stops on its own once
    /// nothing else moves (App::anim_tick). Either way the window is drawn
    /// again now: with no motion running (a still card, Reduced motion)
    /// nothing else would take the card off the window or bring the
    /// keyboard's back (confirm review P2).
    pub(super) fn set_howto_away(&mut self, away: bool) {
        if away == self.howto_away {
            return;
        }
        self.howto_away = away;
        if away {
            self.howto = None;
            self.howto_hover = None;
        }
        self.redraw();
    }

    /// The card waits, fades or plays (the motion timer runs on); never
    /// while it is gone, nor shown still.
    pub(super) fn howto_moving(&self) -> bool {
        self.howto_moving_at(Instant::now())
    }

    pub(super) fn howto_moving_at(&self, now: Instant) -> bool {
        let mode = self.motion_mode();
        self.howto.is_some_and(|t| tooltip::moving(&t, now, mode) || (t.hide.is_none() && mode == Mode::Full))
    }

    /// A card is up (Esc puts it away first).
    pub(super) fn howto_up(&self) -> bool {
        self.howto.is_some_and(|t| t.hide.is_none() && tooltip::alpha(&t, Instant::now(), self.motion_mode()) > 0.0)
    }

    /// Esc on a card: away until the pointer or the focus moves on.
    pub(super) fn quiet_howto(&mut self) {
        self.howto_quiet = self.howto.map(|t| t.ri);
    }

    /// The page moved under the pointer (a scroll, a resize): the card goes.
    pub(super) fn howto_page_moved(&mut self) {
        self.howto_hover = None;
        if let Some(t) = self.howto {
            self.howto_quiet = Some(t.ri);
        }
        self.howto = tooltip::step_after(self.howto, None, false, Instant::now(), self.motion_mode(), HOLD);
    }

    /// The card over everything, by its row as drawn now.
    pub(super) unsafe fn paint_howto(&self, dc: &ID2D1DeviceContext) -> Result<()> {
        self.howto_drawn.set(None);
        let Some(tip) = self.howto else { return Ok(()) };
        let (now, mode) = (Instant::now(), self.motion_mode());
        let a = tooltip::alpha(&tip, now, mode);
        if a <= 0.001 {
            return Ok(());
        }
        let Some(row) = self.howto_rows.borrow().iter().find(|(ri, _)| *ri == tip.ri).map(|(_, r)| *r) else { return Ok(()) };
        let Some(kind) = self.rows().get(tip.ri).and_then(|r| super::widgets::kind_of(r.key)) else { return Ok(()) };
        let r = place(row, (W, H), D2D_RECT_F { left: 0.0, top: 0.0, right: win_w(), bottom: win_h() }, crate::i18n::rtl());
        let t = match mode {
            Mode::Full => now.saturating_duration_since(tip.from).as_secs_f32() / LOOP.as_secs_f32(),
            Mode::Reduced => still(kind),
        };
        self.howto_drawn.set(Some(tip.ri));
        self.paint_howto_card(dc, r, kind, t, &self.widget_keys(kind), a)
    }

    /// A tone of the tiny desktop: greys, light or dark.
    fn howto_tone(&self, tone: Tone) -> D2D1_COLOR_F {
        let g = |l: f32, d: f32| {
            let v = if self.light { l } else { d };
            D2D1_COLOR_F { r: v, g: v, b: v, a: 1.0 }
        };
        let ink = self.ink();
        match tone {
            Tone::Desk => g(0.84, 0.10),
            Tone::Window => g(1.0, 0.30),
            Tone::Bar => g(0.91, 0.25),
            Tone::Paper => g(0.95, 0.40),
            Tone::Strip => g(0.78, 0.50),
            Tone::Image => g(0.66, 0.46),
            Tone::ImageHi => g(0.80, 0.58),
            Tone::Line => D2D1_COLOR_F { a: 0.22, ..ink },
            Tone::Ink => ink,
            Tone::Scrim => D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
            Tone::Flash => D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 },
        }
    }

    /// The card in `r` at `t` of its loop for widget `k` (`hot`: its
    /// shortcut's keys), `a` visible: the menus' surface on its shadow,
    /// the tiny desktop on it.
    pub(super) unsafe fn paint_howto_card(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, k: Kind, t: f32, hot: &[String], a: f32) -> Result<()> {
        let radius = self.card_r();
        let fade = |c: D2D1_COLOR_F, x: f32| D2D1_COLOR_F { a: (c.a * x * a).clamp(0.0, 1.0), ..c };
        let shadows = |x: f32| -> Vec<(f32, f32, f32, crate::theme::Rgba)> {
            if !crate::effects::items().shadows {
                return Vec::new();
            }
            self.look.elevation(crate::theme::Elevation::E1).into_iter().map(|(dx, dy, b, c)| (dx, dy, b, crate::theme::Rgba { a: (c.a as f32 * x * a).round() as u8, ..c })).collect()
        };
        let card_shadow = crate::topbar::tip_shadows(&self.look, crate::effects::items().shadows).into_iter().map(|(x, y, b, c)| (x, y, b, crate::theme::Rgba { a: (c.a as f32 * a).round() as u8, ..c })).collect::<Vec<_>>();
        crate::gfx::soft_shadows(dc, r, radius, &card_shadow)?;
        self.fill_round(dc, fade(super::rgba(self.look.label_colors().0), 1.0), r, radius)?;
        let abs = |b: Bx| D2D_RECT_F { left: r.left + b[0], top: r.top + b[1], right: r.left + b[2], bottom: r.top + b[3] };
        dc.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let mut clips = 0;
        let mut out = Ok(());
        for op in scene(k, t, hot) {
            out = match op {
                Op::Fill { b, rad, tone, a: x } => self.fill_round(dc, fade(self.howto_tone(tone), x), abs(b), rad),
                Op::Stroke { b, rad, tone, a: x } => {
                    let br = dc.CreateSolidColorBrush(&fade(self.howto_tone(tone), x), None);
                    br.map(|br| {
                        let q = abs(b);
                        dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: q.left + 0.5, top: q.top + 0.5, right: q.right - 0.5, bottom: q.bottom - 0.5 }, radiusX: rad, radiusY: rad }, &br, 1.0, None)
                    })
                }
                Op::Shadow { b, rad, a: x } => crate::gfx::soft_shadows(dc, abs(b), rad, &shadows(x)),
                Op::Keys { cx, top, keys, a: x, down } => self.howto_keys(dc, r.left + cx, r.top + top, &keys, x * a, down),
                Op::Pointer { x, y, a: v, click } => self.howto_pointer(dc, r.left + x, r.top + y, v * a, click),
                Op::Clip(b) => {
                    clips += 1;
                    dc.PushAxisAlignedClip(&abs(b), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    Ok(())
                }
                Op::Unclip => {
                    if clips > 0 {
                        clips -= 1;
                        dc.PopAxisAlignedClip();
                    }
                    Ok(())
                }
            };
            if out.is_err() {
                break;
            }
        }
        for _ in 0..clips {
            dc.PopAxisAlignedClip();
        }
        dc.PopAxisAlignedClip();
        out
    }

    /// Keycaps centered on `cx` from `top`, `a` visible, `down` pressed.
    unsafe fn howto_keys(&self, dc: &ID2D1DeviceContext, cx: f32, top: f32, keys: &[String], a: f32, down: f32) -> Result<()> {
        const SIZE: f32 = 10.5;
        const KH: f32 = 20.0;
        const GAP: f32 = 4.0;
        let widths: Vec<f32> = keys.iter().map(|k| (self.text_w(k, SIZE, true) + 12.0).max(KH)).collect();
        let total = widths.iter().sum::<f32>() + GAP * (keys.len().saturating_sub(1)) as f32;
        let fade = |c: D2D1_COLOR_F, x: f32| D2D1_COLOR_F { a: (c.a * x * a).clamp(0.0, 1.0), ..c };
        let ink = self.ink();
        let mut x = cx - total / 2.0;
        let dy = 1.5 * down;
        for (k, w) in keys.iter().zip(&widths) {
            // Its side under it (a step of depth), the cap on it.
            self.fill_round(dc, fade(ink, 0.20), D2D_RECT_F { left: x, top: top + 2.0, right: x + w, bottom: top + KH + 2.0 }, 5.0)?;
            let cap = D2D_RECT_F { left: x, top: top + dy, right: x + w, bottom: top + KH + dy };
            let face = super::rows::mix(self.howto_tone(Tone::Window), self.howto_tone(Tone::Bar), down);
            self.fill_round(dc, fade(face, 1.0), cap, 5.0)?;
            let b = dc.CreateSolidColorBrush(&fade(ink, 0.22), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: cap.left + 0.5, top: cap.top + 0.5, right: cap.right - 0.5, bottom: cap.bottom - 0.5 }, radiusX: 4.5, radiusY: 4.5 }, &b, 1.0, None);
            let tw = self.text_w(k, SIZE, true);
            #[cfg(test)]
            self.quiet_log.set(true);
            let r = self.label(dc, k, x + (w - tw) / 2.0, self.text_top((cap.top + cap.bottom) / 2.0, SIZE), SIZE, true, fade(ink, 0.9));
            #[cfg(test)]
            self.quiet_log.set(false);
            r?;
            x += w + GAP;
        }
        Ok(())
    }

    /// The pointer as a dot with a light ring; a click as a ring growing
    /// out of it.
    unsafe fn howto_pointer(&self, dc: &ID2D1DeviceContext, x: f32, y: f32, a: f32, click: f32) -> Result<()> {
        let ink = self.ink();
        let c = |col: D2D1_COLOR_F, v: f32| D2D1_COLOR_F { a: (col.a * v).clamp(0.0, 1.0), ..col };
        let at = Vector2 { X: x, Y: y };
        if click > 0.0 {
            let rr = 5.0 + 10.0 * click;
            let b = dc.CreateSolidColorBrush(&c(ink, a * (1.0 - click) * 0.8), None)?;
            dc.DrawEllipse(&D2D1_ELLIPSE { point: at, radiusX: rr, radiusY: rr }, &b, 1.5, None);
        }
        let ring = dc.CreateSolidColorBrush(&c(self.howto_tone(Tone::Window), a), None)?;
        dc.FillEllipse(&D2D1_ELLIPSE { point: at, radiusX: 6.0, radiusY: 6.0 }, &ring);
        let dot = dc.CreateSolidColorBrush(&c(ink, a * 0.85), None)?;
        dc.FillEllipse(&D2D1_ELLIPSE { point: at, radiusX: 4.5, radiusY: 4.5 }, &dot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hot() -> Vec<String> {
        vec!["Win".into(), "Alt".into(), "N".into()]
    }

    fn has_keys(ops: &[Op], first: &str) -> bool {
        ops.iter().any(|o| matches!(o, Op::Keys { keys, .. } if keys.first().map(|s| s.as_str()) == Some(first)))
    }

    #[test]
    fn every_widget_shows_its_shortcut_and_none_when_it_is_off() {
        for k in Kind::ALL {
            let shown = (0..100).any(|i| has_keys(&scene(k, i as f32 / 100.0, &hot()), "Win"));
            assert!(shown, "{k:?}: its keycaps");
            let none = (0..100).all(|i| !has_keys(&scene(k, i as f32 / 100.0, &[]), "Win"));
            assert!(none, "{k:?}: no shortcut, no keycaps");
        }
    }

    #[test]
    fn the_loop_meets_itself_and_stays_in_the_card() {
        for k in Kind::ALL {
            assert_eq!(scene(k, 0.0, &hot()), scene(k, 1.0, &hot()), "{k:?}");
            // The end of the loop looks like its start (nothing pops).
            let (a, b) = (scene(k, 0.0, &hot()), scene(k, 0.999, &hot()));
            assert_eq!(a.len(), b.len(), "{k:?}: {a:?}\n{b:?}");
            for i in 0..100 {
                for op in scene(k, i as f32 / 100.0, &hot()) {
                    let b = match op {
                        Op::Fill { b, .. } | Op::Stroke { b, .. } | Op::Shadow { b, .. } | Op::Clip(b) => b,
                        _ => continue,
                    };
                    assert!(b[0] >= STAGE[0] - 0.5 && b[1] >= STAGE[1] - 0.5 && b[2] <= STAGE[2] + 0.5 && b[3] <= STAGE[3] + 0.5, "{k:?} at {i}%: {b:?}");
                }
            }
        }
    }

    #[test]
    fn the_still_frame_shows_the_widget_at_work() {
        // Notes: up front with its lines; pieces: the piece pinned with
        // the window gone.
        let n = scene(Kind::Notes, still(Kind::Notes), &hot());
        assert!(n.iter().filter(|o| matches!(o, Op::Fill { tone: Tone::Paper, .. })).count() == 1);
        let p = scene(Kind::Pieces, still(Kind::Pieces), &hot());
        assert!(p.iter().any(|o| matches!(o, Op::Clip(_))), "the piece");
        assert!(!has_keys(&p, "Win") && !has_keys(&n, "Win"), "a still frame has no keys pressed");
    }

    #[test]
    fn the_card_goes_under_its_row_else_above_never_over_it() {
        let win = D2D_RECT_F { left: 0.0, top: 0.0, right: 820.0, bottom: 640.0 };
        let row = D2D_RECT_F { left: 244.0, top: 300.0, right: 796.0, bottom: 350.0 };
        let r = place(row, (W, H), win, false);
        assert_eq!((r.left, r.top), (244.0 + INSET, 350.0 + GAP), "under, in line with the label");
        let low = D2D_RECT_F { top: 520.0, bottom: 570.0, ..row };
        let r = place(low, (W, H), win, false);
        assert_eq!(r.bottom, 520.0 - GAP, "above when there is no room under it");
        let r = place(row, (W, H), win, true);
        assert_eq!(r.right, 796.0 - INSET, "right to left: from its right end");
        for top in [0.0f32, 100.0, 300.0, 460.0, 580.0] {
            let row = D2D_RECT_F { top, bottom: top + 50.0, ..row };
            let r = place(row, (W, H), win, false);
            assert!(r.top >= MARGIN - 0.1 && r.bottom <= 640.0 - MARGIN + 0.1, "{top}: inside");
            assert!(r.top >= row.bottom || r.bottom <= row.top, "{top}: never over its row");
        }
    }
}
