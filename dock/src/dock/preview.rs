//! Window preview cards (spec §4.7, polish plan §3/§4, roadmap 22): hover a
//! running app for 400 ms, or click one with several windows, and a card per
//! window appears next to the dock: the app's icon and the window's title, a
//! live DWM thumbnail, and a row with the window's state ("Minimized", "Not
//! responding", "Always on top") and, on hover, its actions:
//!
//! - × asks the window to close (a posted WM_CLOSE: the app may ask to save;
//!   also offered for a window that is not responding). A middle click on a
//!   card does the same (the taskbar's convention).
//! - Always on top (on / off: the pin shows the state), Move to next
//!   monitor, Minimize / Restore. None of these is offered for a window that
//!   is not responding (nor the pin for one we may not change: ontop.rs),
//!   and none waits on the app: topmost is ontop.rs's asynchronous
//!   SetWindowPos (posted to the app's thread, checked after), the move is
//!   snap.rs's helper thread, minimize is the dock's own (genie or a posted
//!   SC_MINIMIZE), restore is input::focus (posted, with a helper thread
//!   waiting).
//! - A right click on a card: Always on top, Hide to tray (crate::traywin),
//!   Close.
//!
//! Resting on a card for PEEK_MS shows that window larger in the middle of
//! the screen over a dimmed backdrop ("Peek windows from previews",
//! Settings > Dock, on by default): one more DWM thumbnail in a click-through
//! overlay. Nothing is captured and no real window moves.
//!
//! At most MAX_CARDS cards show at once; with more, a "+N more" strip sits
//! under them, the wheel over the cards scrolls and a click on the strip
//! shows the next ones. Cards are as wide as their window's shape asks
//! (within bounds), rows wrap and are centered, everything mirrors right to
//! left. With "Preview pop-in" it opens as menus do (crate::motion::open:
//! it grows from the icon's side on GLIDE and fades in, its light after
//! it; a 60 ms fade only with reduced motion), the DWM thumbnails following
//! the same curves frame by frame. Its E2 shadow is a click-through window
//! of its own under it (the panel's window is the plate: a shadow margin
//! there would take the dock's clicks).
//!
//! The keyboard chooser (dock keyboard mode, Enter on an app with several
//! windows): arrows / Tab move, Enter picks, Delete closes the chosen window,
//! Esc goes back. Screen readers get each visible card and its buttons.
//!
//! On the dock icon itself the wheel turns the app's volume (sound.rs) and
//! Shift+wheel steps through the app's windows.
//!
//! Thumbnails are DWM's own (DwmRegisterThumbnail): the compositor draws
//! them, we pay nothing per frame.

use super::*;
use crate::glyphs::Glyph;
use windows::UI::Composition::CompositionBrush;
use windows::Win32::Graphics::Dwm::{
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};

/// The peek's fade-in with "Preview pop-in" on (ms at speed 1).
const POP_MS: i64 = 120;

// Card metrics (DIP).
/// The thumbnail's box: this tall, as wide as the window's shape asks within
/// THUMB_MIN_W..THUMB_MAX_W.
const THUMB_H: f32 = 116.0;
const THUMB_MIN_W: f32 = 164.0;
const THUMB_MAX_W: f32 = 300.0;
// On the 4 DIP grid (design B3). The panel's corners are the menus' (12);
// cards sit OUTER inside it, so theirs are concentric: 12 - 4 = 8 (card).
const TITLE_H: f32 = 32.0;
/// The state / actions row under the thumbnail.
const ACT_H: f32 = 32.0;
const CARD_PAD: f32 = 8.0;
const CARD_H: f32 = TITLE_H + THUMB_H + ACT_H;
const GAP: f32 = crate::theme::metrics::space::S2;
const OUTER: f32 = crate::theme::metrics::space::S2;
const CLOSE: f32 = 24.0;
/// The card buttons' glyphs (× and the strip's), one size.
const BTN_GLYPH: f32 = 12.0;
const BTN: f32 = 24.0;
const BTN_GAP: f32 = 2.0;
const ICON: f32 = 16.0;
const FOOTER_H: f32 = 24.0;
/// At most this many cards at once; the rest scroll ("+N more").
pub(super) const MAX_CARDS: usize = 8;
/// Hover fades (ms at speed 1).
const HOVER_MS: i64 = 83;
/// The peek's backdrop (0..255 black).
const DIM_ALPHA: u8 = 96;

const HIDE_TIMER: usize = 1;
const HIDE_MS: u32 = 200;
/// One-shot: the pointer (or the keyboard) rested on a card: peek.
const PEEK_TIMER: usize = 2;
pub(super) const PEEK_MS: u32 = 400;
/// Repeating while open: windows minimized / restored / hung meanwhile.
const STATE_TIMER: usize = 3;
const STATE_MS: u32 = 500;
/// Soon after an action: show its result.
const STATE_SOON_MS: u32 = 150;
/// Frames of the thumbnails' open and of the peek's fade.
const ANIM_TIMER: usize = 4;
const PEEK_ANIM_TIMER: usize = 5;
const FRAME_MS: u32 = 15;
/// One-shot: the pointer rested on a cut title or a button: its tooltip
/// (crate::topbar's drawn one, after its TIP_DELAY_MS).
const TIP_TIMER: usize = 6;

/// Posted to the dock window (wParam = the window, lParam = its process):
/// minimize it the dock's way (genie / plain, per the settings) if it is
/// still that process's window and one the dock lists.
pub(super) const WM_PREVIEW_MIN: u32 = WM_APP + 78;
/// Posted to the dock window: the keyboard chooser's cards or focus
/// changed without a key (wheel, "+N more"): tell screen readers.
pub(super) const WM_PREVIEW_A11Y: u32 = WM_APP + 79;

/// (x, y, w, h) in panel DIPs.
pub(super) type Rect4 = (f32, f32, f32, f32);

// ---- pure layout ------------------------------------------------------------

/// A card's width for a window of `src_w` x `src_h` (unknown: 16:9): the
/// thumbnail box keeps the window's shape at THUMB_H tall, within bounds.
pub(super) fn card_width(src_w: i32, src_h: i32) -> f32 {
    let aspect = if src_w > 0 && src_h > 0 { src_w as f32 / src_h as f32 } else { 16.0 / 9.0 };
    (THUMB_H * aspect).clamp(THUMB_MIN_W, THUMB_MAX_W) + CARD_PAD * 2.0
}

/// Cards of `widths` in rows at most `max_w` wide (at least one per row),
/// each row centered, stopping before a row that would pass `max_h` (at
/// least one row). Returns the rects of the cards that fit and the panel
/// size.
fn flow(widths: &[f32], max_w: f32, max_h: f32) -> (Vec<Rect4>, f32, f32) {
    let room = max_w - OUTER * 2.0;
    let mut rows: Vec<(usize, usize, f32)> = Vec::new();
    let mut i = 0;
    while i < widths.len() {
        let n = rows.len() as f32 + 1.0;
        if !rows.is_empty() && OUTER * 2.0 + n * CARD_H + (n - 1.0) * GAP > max_h {
            break;
        }
        let start = i;
        let mut rw = widths[i];
        i += 1;
        while i < widths.len() && rw + GAP + widths[i] <= room {
            rw += GAP + widths[i];
            i += 1;
        }
        rows.push((start, i, rw));
    }
    let content = rows.iter().map(|r| r.2).fold(0.0, f32::max);
    let mut rects = Vec::new();
    for (ri, &(a, b, rw)) in rows.iter().enumerate() {
        let mut x = OUTER + (content - rw) / 2.0;
        let y = OUTER + ri as f32 * (CARD_H + GAP);
        for &w in &widths[a..b] {
            rects.push((x, y, w, CARD_H));
            x += w + GAP;
        }
    }
    let nr = rows.len() as f32;
    let h = if rows.is_empty() { OUTER * 2.0 } else { OUTER * 2.0 + nr * CARD_H + (nr - 1.0) * GAP };
    (rects, content + OUTER * 2.0, h)
}

/// What the panel shows from card `first` on.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CardPage {
    /// `first` kept inside the list.
    pub first: usize,
    /// Cards first..first + rects.len().
    pub rects: Vec<Rect4>,
    pub w: f32,
    pub h: f32,
    /// Cards not shown (before and after): "+N more".
    pub more: usize,
    /// The "+N more" strip (when `more` > 0).
    pub footer: Option<Rect4>,
}

/// Up to MAX_CARDS cards from `first`, as many as fit `max_w` x `max_h`;
/// mirrored (first card on the right) when `rtl`.
pub(super) fn page(widths: &[f32], first: usize, max_w: f32, max_h: f32, rtl: bool) -> CardPage {
    let n = widths.len();
    let first = first.min(n.saturating_sub(1));
    let end = (first + MAX_CARDS).min(n);
    let (mut rects, mut w, mut h) = flow(&widths[first..end], max_w, max_h);
    let mut footer = None;
    if rects.len() < n {
        // Room for the strip under the cards.
        (rects, w, h) = flow(&widths[first..end], max_w, max_h - FOOTER_H);
        footer = Some((OUTER, h - OUTER, w - OUTER * 2.0, FOOTER_H));
        h += FOOTER_H;
    }
    if rtl {
        for r in &mut rects {
            r.0 = w - r.0 - r.2;
        }
    }
    CardPage { first, more: n - rects.len(), rects, w, h, footer }
}

/// The wheel over the cards: one card on (`down`) or back, never past the
/// ends (`shown` cards from `first` of `n`).
pub(super) fn scroll(first: usize, shown: usize, n: usize, down: bool) -> usize {
    if down {
        if first + shown < n { first + 1 } else { first }
    } else {
        first.saturating_sub(1)
    }
}

/// The first card that keeps card `i` in view.
pub(super) fn scroll_to(first: usize, shown: usize, i: usize) -> usize {
    if i < first {
        i
    } else if shown > 0 && i >= first + shown {
        i + 1 - shown
    } else {
        first
    }
}

/// The chosen card `k` after the page changed to `start..end`: itself if
/// still on it, else the nearest card on it.
pub(super) fn keep_on_page(k: usize, start: usize, end: usize) -> usize {
    if end <= start {
        k
    } else {
        k.clamp(start, end - 1)
    }
}

/// A click on "+N more": the next cards, then from the start again.
pub(super) fn next_page(first: usize, shown: usize, n: usize) -> usize {
    if first + shown >= n { 0 } else { first + shown }
}

/// The buttons on a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardBtn {
    Close,
    Top,
    Move,
    MinRestore,
}

const BTNS: [CardBtn; 4] = [CardBtn::Close, CardBtn::Top, CardBtn::Move, CardBtn::MinRestore];

impl CardBtn {
    /// Part of its UIA key.
    pub(super) fn key(self) -> &'static str {
        match self {
            CardBtn::Close => "close",
            CardBtn::Top => "top",
            CardBtn::Move => "move",
            CardBtn::MinRestore => "min",
        }
    }

    fn index(self) -> usize {
        BTNS.iter().position(|b| *b == self).unwrap_or(0)
    }

    /// Its glyph (crate::glyphs).
    fn glyph(self, st: WinState) -> Glyph {
        match self {
            CardBtn::Close => Glyph::Close,
            CardBtn::Top if st.topmost => Glyph::Pinned,
            CardBtn::Top => Glyph::Pin,
            CardBtn::Move => Glyph::Monitor,
            CardBtn::MinRestore if st.minimized => Glyph::Restore,
            CardBtn::MinRestore => Glyph::Minus,
        }
    }

    /// Its name (UIA; the toggle's state is told by the Toggle pattern).
    pub(super) fn name(self, st: WinState) -> &'static str {
        match self {
            CardBtn::Close => crate::i18n::t("dock.menu.close_window"),
            CardBtn::Top => crate::i18n::t("preview.topmost"),
            CardBtn::Move => crate::i18n::t("preview.move_next"),
            CardBtn::MinRestore if st.minimized => crate::i18n::t("preview.restore"),
            CardBtn::MinRestore => crate::i18n::t("preview.minimize"),
        }
    }

    /// Its tooltip: says what a click does.
    fn tip(self, st: WinState) -> &'static str {
        match self {
            CardBtn::Top if st.topmost => crate::i18n::t("preview.topmost_off"),
            _ => self.name(st),
        }
    }
}

/// What the cards know about a window (read without waiting on it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct WinState {
    pub minimized: bool,
    pub hung: bool,
    pub topmost: bool,
    /// "Always on top" cannot change it (ours, the shell, higher rights).
    pub locked: bool,
}

/// Can button `b` act on a window in state `st` (`monitors` connected)?
/// A window that is not responding only gets the close request (a posted
/// WM_CLOSE waits for nobody); moving needs a second monitor.
pub(super) fn can(b: CardBtn, st: WinState, monitors: usize) -> bool {
    match b {
        CardBtn::Close => true,
        _ if st.hung => false,
        CardBtn::Move => monitors > 1,
        CardBtn::Top => !st.locked,
        CardBtn::MinRestore => true,
    }
}

/// The card's buttons (panel DIPs): × at the end of the title row, the
/// strip (always on top, next monitor, minimize / restore) at the end of
/// the bottom row; mirrored when `rtl`.
pub(super) fn buttons(r: Rect4, rtl: bool) -> [(CardBtn, Rect4); 4] {
    let (x, y, w, _) = r;
    let m = |bx: f32, bw: f32| crate::i18n::mirror::span(rtl, bx, bw, x, x + w);
    let close = (m(x + w - CARD_PAD / 2.0 - CLOSE, CLOSE), y + (TITLE_H - CLOSE) / 2.0, CLOSE, CLOSE);
    let by = y + TITLE_H + THUMB_H + (ACT_H - BTN) / 2.0;
    let right = x + w - CARD_PAD / 2.0;
    let at = |k: f32| right - (3.0 - k) * BTN - (2.0 - k) * BTN_GAP;
    [
        (CardBtn::Close, close),
        (CardBtn::Top, (m(at(0.0), BTN), by, BTN, BTN)),
        (CardBtn::Move, (m(at(1.0), BTN), by, BTN, BTN)),
        (CardBtn::MinRestore, (m(at(2.0), BTN), by, BTN, BTN)),
    ]
}

/// Where the pointer is on the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardHit {
    /// Card (its index in the list), on one of its buttons or not.
    Card(usize, Option<CardBtn>),
    Footer,
    Nothing,
}

fn inside(r: Rect4, x: f32, y: f32, grow: f32) -> bool {
    x >= r.0 - grow && x < r.0 + r.2 + grow && y >= r.1 - grow && y < r.1 + r.3 + grow
}

/// The tooltip (id) under (x, y), window DIPs, of `tips` (id, where,
/// text). Pure: tested.
pub(super) fn tip_under(tips: &[(usize, Rect4, String)], x: f32, y: f32) -> Option<usize> {
    tips.iter().find(|(_, r, _)| inside(*r, x, y, 0.0)).map(|t| t.0)
}

/// What the tooltip does when the pointer is over tip `under` (None: no
/// tip there), tip `on` being the one it was over, `up` whether its tip
/// shows. Like the top bar's tips: a first one waits, from one to the
/// next it follows at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TipStep {
    /// The same one (or still none): nothing changes.
    Keep,
    /// Off every tip: hidden, and the next one waits again.
    Hide,
    /// A first one: after the wait.
    Wait(usize),
    /// Another one while a tip shows: at once.
    Show(usize),
}

/// Pure: see TipStep. Tested.
pub(super) fn tip_step(on: Option<usize>, up: bool, under: Option<usize>) -> TipStep {
    match under {
        _ if under == on => TipStep::Keep,
        None => TipStep::Hide,
        Some(id) if up => TipStep::Show(id),
        Some(id) => TipStep::Wait(id),
    }
}

/// `cards`: (index, rect) of the cards on screen.
pub(super) fn hit_in(cards: &[(usize, Rect4)], footer: Option<Rect4>, x: f32, y: f32, rtl: bool) -> CardHit {
    if footer.is_some_and(|f| inside(f, x, y, 0.0)) {
        return CardHit::Footer;
    }
    for &(k, r) in cards {
        if inside(r, x, y, 0.0) {
            let b = buttons(r, rtl).into_iter().find(|(_, br)| inside(*br, x, y, 2.0)).map(|(b, _)| b);
            return CardHit::Card(k, b);
        }
    }
    CardHit::Nothing
}

/// The thumbnail fitted into the card's picture box (centered, aspect kept).
pub(super) fn fit(src_w: i32, src_h: i32, box_w: f32, box_h: f32) -> (f32, f32, f32, f32) {
    if src_w <= 0 || src_h <= 0 {
        return (0.0, 0.0, box_w, box_h);
    }
    let s = (box_w / src_w as f32).min(box_h / src_h as f32);
    let (w, h) = (src_w as f32 * s, src_h as f32 * s);
    ((box_w - w) / 2.0, (box_h - h) / 2.0, w, h)
}

/// The peek: the window at most 3/4 of the work area, never larger than it
/// really is, centered (screen px). None for an unknown size.
pub(super) fn peek_rect(src_w: i32, src_h: i32, work: RECT) -> Option<RECT> {
    let (ww, wh) = (work.right - work.left, work.bottom - work.top);
    if src_w <= 0 || src_h <= 0 || ww <= 0 || wh <= 0 {
        return None;
    }
    let s = (ww as f32 * 0.75 / src_w as f32).min(wh as f32 * 0.75 / src_h as f32).min(1.0);
    let (w, h) = ((src_w as f32 * s).round() as i32, (src_h as f32 * s).round() as i32);
    let (x, y) = (work.left + (ww - w) / 2, work.top + (wh - h) / 2);
    Some(RECT { left: x, top: y, right: x + w, bottom: y + h })
}

/// Pure: the open / peek animation's length (0: none) for "Preview pop-in"
/// (`pop`), animations at all (`motion`) and POP_MS at the effects speed
/// (`scaled`, see effects::ms).
pub(super) fn open_len(pop: bool, motion: bool, scaled: i64) -> u32 {
    if pop && motion { scaled.clamp(0, 2000) as u32 } else { 0 }
}

/// The open / peek animation's length for the effects in force `r`
/// (`motion`: animations run at all).
pub(super) fn open_len_of(r: &crate::effects::Resolved, motion: bool) -> u32 {
    open_len(r.items.pops_previews(), motion, crate::effects::ms_at(POP_MS, r.speed))
}

/// The open / peek animation's length now (crate::effects).
pub(super) fn open_ms() -> u32 {
    open_len_of(&crate::effects::current(), motion())
}

/// Ease-out (fast start, soft landing), `t` in 0..1.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

// ---- the panel --------------------------------------------------------------

/// How the dock wants the cards.
pub(super) struct Opts {
    /// The app's icon (the dock icon's brush), for the title rows.
    pub icon: Option<CompositionBrush>,
    /// Peek windows from previews (Settings > Dock).
    pub peek: bool,
    /// The preset's "Preview pop-in" item (crate::effects: off in Medium
    /// and low-spec, where the thumbnails are not stepped frame by frame):
    /// it opens and closes as menus do, else at once. How it moves is
    /// motion::now()'s alone (reduced motion: a 60 ms fade).
    pub pop: bool,
}

struct Card {
    window: HWND,
    /// Its process, to tell the window from a later one with the same handle.
    pid: u32,
    thumb: Option<isize>,
    /// The window's size when the cards opened (its card's shape).
    src: (i32, i32),
    state: WinState,
    /// On the panel; zero while off the page.
    rect: Rect4,
    hover: SpriteVisual,
    /// Shown with hover: × and the strip.
    tools: Option<ContainerVisual>,
    /// Per enabled button: its hover highlight.
    hot: Vec<(CardBtn, SpriteVisual)>,
    /// Its thumbnail's place at rest (window px; empty off the page).
    dest: RECT,
}

/// The click-through window under the cards that holds their E2 shadows
/// and contact ring (popup::shade_at), SHADOW_M around them.
struct Shade {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    /// Scaled to the display, offset by the margin; the shade in it.
    outer: ContainerVisual,
}

/// How the cards open (real time): how long they grow (0: they do not)
/// and fade in, on which curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OpenPlan {
    grow: i64,
    fade: i64,
    curve: crate::motion::Curve,
}

/// Pure: the shared open (motion::open, a menu) in `mode` at animation
/// speed `speed`: GLIDE's OPEN_MS stretched by the speed, the menu fade as
/// fade_plan plays it (reduced motion: no growing, 60 ms straight).
pub(super) fn open_plan(mode: crate::motion::Mode, speed: f32) -> OpenPlan {
    use crate::motion::{fade_plan, open_lengths, Kind, EASE_OUT};
    let (grow, fade) = open_lengths(mode, Kind::Menu);
    let (fade, curve) = fade_plan(mode, fade, EASE_OUT, speed);
    OpenPlan { grow: if grow > 0 { crate::effects::ms_at(grow, speed) } else { 0 }, fade, curve }
}

/// Pure: `t` ms into opening on `p`: (scale, opacity, over). The scale is
/// motion::open's: OPEN_SCALE to 1 on GLIDE; the opacity its fade's curve.
pub(super) fn open_frame(p: OpenPlan, t: f32) -> (f32, f32, bool) {
    use crate::motion::{bezier, OPEN_MS, OPEN_SCALE, SPRING_GLIDE};
    let t = if t.is_finite() { t.max(0.0) } else { f32::MAX };
    let k = if p.grow > 0 {
        let prog = t / p.grow as f32;
        let done = if prog >= 1.0 { 1.0 } else { 1.0 - SPRING_GLIDE.left(prog * OPEN_MS as f32 / 1000.0) };
        OPEN_SCALE + (1.0 - OPEN_SCALE) * done
    } else {
        1.0
    };
    let o = if p.fade > 0 { bezier(p.curve, t / p.fade as f32) } else { 1.0 };
    (k, o, t >= p.grow.max(p.fade) as f32)
}

/// The thumbnails' open: its plan, and its clock, which starts with the
/// plate's (motion::open, once the preview is made: its windows, targets,
/// shadow and thumbnails take their time first, and none of that is the
/// open's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Opening {
    plan: OpenPlan,
    /// When the plate's open began (None: not yet; the thumbnails wait at
    /// the open's first frame).
    since: Option<Instant>,
}

impl Opening {
    pub(super) fn new(plan: OpenPlan) -> Opening {
        Opening { plan, since: None }
    }

    /// The plate's open begins at `at`: the thumbnails' clock with it.
    pub(super) fn begin(&mut self, at: Instant) {
        self.since = Some(at);
    }

    /// (scale, opacity, over) at `now` (open_frame): its first frame until
    /// it has begun.
    pub(super) fn frame(&self, now: Instant) -> (f32, f32, bool) {
        let t = self.since.map_or(0.0, |s| now.saturating_duration_since(s).as_secs_f32() * 1000.0);
        let (k, o, over) = open_frame(self.plan, t);
        (k, o, over && self.since.is_some())
    }
}

/// Pure: rect `d` (px) scaled by `k` about `o` (px), as Composition scales
/// the panel about its center point.
pub(super) fn scaled(d: RECT, o: (f32, f32), k: f32) -> RECT {
    let f = |v: i32, c: f32| (c + (v as f32 - c) * k).round() as i32;
    RECT { left: f(d.left, o.0), top: f(d.top, o.1), right: f(d.right, o.0), bottom: f(d.bottom, o.1) }
}

/// The large thumbnail over a dimmed screen.
struct Peek {
    dim: HWND,
    frame: HWND,
    thumb: isize,
    start: Instant,
    ms: u32,
}

impl Peek {
    unsafe fn close(self) {
        let _ = DwmUnregisterThumbnail(self.thumb);
        let _ = DestroyWindow(self.frame);
        let _ = DestroyWindow(self.dim);
    }

    /// One fade frame; true when done.
    unsafe fn step(&self) -> bool {
        let lvl = if self.ms == 0 { 1.0 } else { ease(self.start.elapsed().as_millis() as f32 / self.ms as f32) };
        let _ = SetLayeredWindowAttributes(self.dim, COLORREF(0), (DIM_ALPHA as f32 * lvl) as u8, LWA_ALPHA);
        let props = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_OPACITY, opacity: (255.0 * lvl) as u8, ..Default::default() };
        let _ = DwmUpdateThumbnailProperties(self.thumb, &props);
        lvl >= 1.0
    }
}

struct Preview {
    hwnd: HWND,
    _target: DesktopWindowTarget,
    root: ContainerVisual,
    gfx: Rc<Gfx>,
    theme: Theme,
    scale: f32,
    /// The dock item (app path) these cards belong to.
    app: String,
    icon: Option<CompositionBrush>,
    cards: Vec<Card>,
    file_card: Option<(HWND, u32)>,
    hovered: Option<usize>,
    /// The enabled button under the pointer.
    hot: Option<(usize, CardBtn)>,
    /// In the keyboard chooser: the chosen card's button with the focus
    /// (None: the card itself).
    kb_btn: Option<CardBtn>,
    /// Wheel delta over the cards not yet a whole notch.
    wheel_acc: i32,
    tracking: bool,
    /// Cut titles in full and the buttons' names: (id, where in window
    /// DIPs, text). Shown in the drawn tooltip the top bar and the dock's
    /// name label share (radius tip, E1, the ramp's Footnote; design B3
    /// review P3 #4): the one under the pointer (`tip_on`) once it rests
    /// there, `tip_up` while it shows.
    tips: Vec<(usize, Rect4, String)>,
    tip_on: Option<usize>,
    tip_up: bool,
    /// Opened from the dock's keyboard mode: arrows move this card,
    /// Enter picks it, Esc closes (the dock forwards the keys).
    keyboard: Option<usize>,
    /// Where it points at (screen px) and how it opens.
    anchor: POINT,
    pos: Position,
    work: RECT,
    /// The page: cards first..first + shown, and the "+N more" strip.
    first: usize,
    shown: usize,
    footer: Option<Rect4>,
    monitors: usize,
    peek_on: bool,
    peek: Option<Peek>,
    /// Its shadow's window (None: it could not be made; the cards show
    /// without it).
    shade: Option<Shade>,
    /// The panel's size (DIP) and the point it grows from (DIP, its own
    /// coordinates: the edge nearest the dock icon, motion::origin).
    size: (f32, f32),
    origin: (f32, f32),
    /// Opening: on what plan, and since when (the thumbnails follow it
    /// frame by frame; the plate is Composition's).
    anim: Option<Opening>,
    /// The (scale, opacity) last given the thumbnails: where they close
    /// from.
    applied: (f32, f32),
}

thread_local! {
    static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
}

/// What a fading preview keeps until its window goes (crate::motion::
/// retire): its visuals, and its thumbnails and peek, which go then.
struct Fading {
    thumbs: Vec<isize>,
    peek: Option<Peek>,
    _preview: Option<Preview>,
}

impl Drop for Fading {
    fn drop(&mut self) {
        unsafe {
            for &id in &self.thumbs {
                let _ = DwmUnregisterThumbnail(id);
            }
            if let Some(p) = self.peek.take() {
                p.close();
            }
        }
    }
}

fn with_preview<R>(f: impl FnOnce(&mut Preview) -> R) -> Option<R> {
    PREVIEW.with(|p| p.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub(super) fn file_card(hwnd: HWND, pt: POINT) -> Option<(HWND, u32)> {
    with_preview(|p| {
        let mut r = RECT::default();
        if p.hwnd != hwnd || unsafe { GetWindowRect(hwnd, &mut r) }.is_err() { return None; }
        let CardHit::Card(k, _) = p.hit((pt.x-r.left) as f32/p.scale, (pt.y-r.top) as f32/p.scale) else { return None };
        Some((p.cards[k].window, p.cards[k].pid))
    }).flatten()
}

pub(super) fn arm_file_card(hwnd: HWND, card: Option<(HWND, u32)>) {
    with_preview(|p| {
        if p.hwnd != hwnd { return; }
        p.file_card = card;
        unsafe {
            let _ = KillTimer(Some(hwnd), super::drag::FILE_HOVER_TIMER);
            if card.is_some() { SetTimer(Some(hwnd), super::drag::FILE_HOVER_TIMER, super::drag::FILE_HOVER_MS, None); }
        }
    });
}

pub(super) fn is_open() -> bool {
    PREVIEW.with(|p| p.try_borrow().map(|g| g.is_some()).unwrap_or(true))
}

/// The app whose cards are open.
pub(super) fn open_for() -> Option<String> {
    with_preview(|p| p.app.clone())
}

/// Closes the cards: they fade out as they faded in ("Preview pop-in",
/// crate::motion's close), at once without it.
pub(super) fn hide() {
    let old = PREVIEW.with(|p| p.borrow_mut().take());
    crate::companion::refresh();
    if let Some(p) = old {
        if crate::effects::items().preview_pop {
            unsafe { p.retire() };
        } else {
            unsafe { p.close() };
        }
    }
}

/// Closes the cards at once (another app's take their place).
fn hide_now() {
    let old = PREVIEW.with(|p| p.borrow_mut().take());
    crate::companion::refresh();
    if let Some(p) = old {
        unsafe { p.close() };
    }
}

/// The dock's window list changed: drop cards of windows that are gone
/// (or no longer the same process); close if none are left.
pub(super) fn sync(app_windows: &dyn Fn(&str) -> Vec<HWND>) {
    let empty = with_preview(|p| {
        let live = app_windows(&p.app);
        let before = p.cards.len();
        let mut gone = Vec::new();
        p.cards.retain(|c| {
            let keep = live.contains(&c.window) && pid_of(c.window) == c.pid;
            if !keep {
                gone.extend(c.thumb);
            }
            keep
        });
        for id in gone {
            unsafe {
                let _ = DwmUnregisterThumbnail(id);
            }
        }
        if p.cards.len() != before && !p.cards.is_empty() {
            p.hovered = None;
            p.hot = None;
            p.unpeek();
            // The keyboard's card stays chosen (or the last one is).
            if let Some(k) = p.keyboard {
                p.keyboard = Some(k.min(p.cards.len() - 1));
            }
            let _ = unsafe { p.build() };
            if let Some(k) = p.keyboard {
                let b = p.kb_btn;
                p.choose(k, b);
            }
        }
        p.cards.is_empty()
    });
    if empty == Some(true) {
        hide();
    }
}

/// What a key does in the keyboard chooser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KbStep {
    /// Show this card as chosen, with the focus on it (None) or on one of
    /// its buttons.
    To(usize, Option<CardBtn>),
    /// Switch to the chosen card's window.
    Pick,
    /// Run the focused button.
    Press(CardBtn),
    /// Ask the chosen window to close (the chooser stays).
    Close,
    Cancel,
    Nothing,
}

/// The chooser's keys: left, up, right, down, home, end, enter, space,
/// esc, tab, delete.
type KbKeys = [u16; 11];

/// `vk` with card `at` of `n` chosen and the focus on it (`btn` None) or on
/// one of its buttons. Reads Shift and the layout direction.
pub(super) fn kb_step(vk: u16, at: usize, btn: Option<CardBtn>, n: usize, enabled: &dyn Fn(usize, CardBtn) -> bool) -> KbStep {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP};
    if n == 0 {
        return KbStep::Cancel;
    }
    let shift = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(windows::Win32::UI::Input::KeyboardAndMouse::VK_SHIFT.0 as i32) } < 0;
    let keys = [VK_LEFT.0, VK_UP.0, VK_RIGHT.0, VK_DOWN.0, VK_HOME.0, VK_END.0, VK_RETURN.0, VK_SPACE.0, VK_ESCAPE.0, VK_TAB.0, VK_DELETE.0];
    kb_step_with(vk, shift, crate::i18n::rtl(), at, btn, n, enabled, keys)
}

/// The Tab stops of card `k`: the card, then its usable buttons in order.
fn tab_stops(k: usize, enabled: &dyn Fn(usize, CardBtn) -> bool) -> Vec<Option<CardBtn>> {
    std::iter::once(None).chain(BTNS.into_iter().filter(|b| enabled(k, *b)).map(Some)).collect()
}

/// Pure kb_step. Arrows move between cards (wrapping, like Alt+Tab); right
/// to left, Left and Right follow the mirrored cards. Tab / Shift+Tab walk
/// card, its buttons, next card (logical order, also right to left). Enter
/// / Space run the focused button, else pick the card. Esc on a button goes
/// back to its card, else closes the chooser.
#[allow(clippy::too_many_arguments)]
fn kb_step_with(vk: u16, shift: bool, rtl: bool, at: usize, btn: Option<CardBtn>, n: usize, enabled: &dyn Fn(usize, CardBtn) -> bool, keys: KbKeys) -> KbStep {
    let [left, up, right, down, home, end, enter, space, esc, tab, delete] = keys;
    let at = at.min(n - 1);
    let (prev, next) = ((at + n - 1) % n, (at + 1) % n);
    match vk {
        v if v == up || (v == left && !rtl) || (v == right && rtl) => KbStep::To(prev, None),
        v if v == down || (v == right && !rtl) || (v == left && rtl) => KbStep::To(next, None),
        v if v == tab => {
            let stops = tab_stops(at, enabled);
            let i = stops.iter().position(|s| *s == btn).unwrap_or(0);
            if !shift {
                match stops.get(i + 1) {
                    Some(s) => KbStep::To(at, *s),
                    None => KbStep::To(next, None),
                }
            } else if i > 0 {
                KbStep::To(at, stops[i - 1])
            } else {
                KbStep::To(prev, tab_stops(prev, enabled).last().copied().flatten())
            }
        }
        v if v == home => KbStep::To(0, None),
        v if v == end => KbStep::To(n - 1, None),
        v if v == enter || v == space => match btn {
            Some(b) => KbStep::Press(b),
            None => KbStep::Pick,
        },
        v if v == delete => KbStep::Close,
        v if v == esc && btn.is_some() => KbStep::To(at, None),
        v if v == esc => KbStep::Cancel,
        _ => KbStep::Nothing,
    }
}

/// A card button for screen readers.
pub(super) struct KbButton {
    pub key: String,
    pub name: String,
    pub rect: RECT,
    pub enabled: bool,
    /// Toggle state (Always on top).
    pub on: Option<bool>,
    /// It has the keyboard focus.
    pub focused: bool,
}

/// A chooser card for screen readers.
pub(super) struct KbCard {
    pub window: HWND,
    /// The window's full title.
    pub title: String,
    /// Its state ("Minimized", "Not responding"), or empty.
    pub help: String,
    /// On screen, px.
    pub rect: RECT,
    pub chosen: bool,
    pub buttons: Vec<KbButton>,
}

/// The keyboard chooser's cards on screen. Empty when it is not up.
pub(super) fn kb_cards() -> Vec<KbCard> {
    with_preview(|p| {
        let Some(chosen) = p.keyboard else { return Vec::new() };
        let mut wr = RECT::default();
        unsafe {
            let _ = GetWindowRect(p.hwnd, &mut wr);
        }
        let s = p.scale;
        let px = |r: Rect4| RECT {
            left: wr.left + (r.0 * s) as i32,
            top: wr.top + (r.1 * s) as i32,
            right: wr.left + ((r.0 + r.2) * s) as i32,
            bottom: wr.top + ((r.1 + r.3) * s) as i32,
        };
        let rtl = crate::i18n::rtl();
        p.on_page()
            .map(|k| {
                let c = &p.cards[k];
                let key = card_key(c.window);
                let buttons = buttons(c.rect, rtl)
                    .into_iter()
                    .map(|(b, r)| KbButton {
                        key: format!("{key}:{}", b.key()),
                        name: b.name(c.state).to_string(),
                        rect: px(r),
                        enabled: can(b, c.state, p.monitors),
                        on: (b == CardBtn::Top).then_some(c.state.topmost),
                        focused: k == chosen && p.kb_btn == Some(b),
                    })
                    .collect();
                KbCard { window: c.window, title: title_of(c.window), help: state_text(c.state).to_string(), rect: px(c.rect), chosen: k == chosen, buttons }
            })
            .collect()
    })
    .unwrap_or_default()
}

/// The UIA key of a chooser card.
pub(super) fn card_key(h: HWND) -> String {
    format!("win:{:x}", h.0 as usize)
}

/// Card index and button (None: the card itself) of UIA key `key`.
fn find_key(p: &Preview, key: &str) -> Option<(usize, Option<CardBtn>)> {
    p.on_page().find_map(|k| {
        let base = card_key(p.cards[k].window);
        let rest = key.strip_prefix(base.as_str())?;
        if rest.is_empty() {
            return Some((k, None));
        }
        let rest = rest.strip_prefix(':')?;
        BTNS.into_iter().find(|b| b.key() == rest).map(|b| (k, Some(b)))
    })
}

/// A screen reader pressed card button `key` (from kb_cards): true if it
/// was one (the action is under way).
pub(super) fn press(key: &str) -> bool {
    let found = with_preview(|p| {
        let (k, b) = find_key(p, key)?;
        let c = &p.cards[k];
        b.map(|b| (c.window, c.pid, b, c.state, p.monitors))
    })
    .flatten();
    match found {
        Some((w, pid, b, st, mons)) => {
            unsafe { act(w, pid, b, st, mons) };
            soon_refresh();
            true
        }
        None => false,
    }
}

/// A screen reader moved the focus to card or button `key` (from
/// kb_cards): true if it was one of them.
pub(super) fn kb_focus(key: &str) -> bool {
    with_preview(|p| {
        let Some((k, b)) = find_key(p, key) else { return false };
        if p.keyboard.is_none() {
            return false;
        }
        p.choose(k, b);
        true
    })
    .unwrap_or(false)
}

/// The keyboard chooser is up.
pub(super) fn kb_active() -> bool {
    with_preview(|p| p.keyboard.is_some()).unwrap_or(false)
}

/// Opens the cards for the keyboard (after show()): the first card chosen.
pub(super) fn kb_start(first: usize) {
    with_preview(|p| {
        let n = p.cards.len();
        if n > 0 {
            p.hovered = None;
            p.choose(first.min(n - 1), None);
        }
    });
}

/// What a key in the chooser leads to, done outside the borrow.
enum KbOut {
    Switch(HWND, u32),
    Act(HWND, u32, CardBtn, WinState, usize),
    Cancel,
    Nothing,
}

/// A key while choosing: Some(window) to switch to (the cards close).
pub(super) fn kb_key(vk: u16) -> Option<HWND> {
    let out = with_preview(|p| {
        let Some(at) = p.keyboard else { return KbOut::Nothing };
        let (mons, btn) = (p.monitors, p.kb_btn);
        let states: Vec<WinState> = p.cards.iter().map(|c| c.state).collect();
        let enabled = |k: usize, b: CardBtn| states.get(k).is_some_and(|s| can(b, *s, mons));
        match kb_step(vk, at, btn, p.cards.len(), &enabled) {
            KbStep::To(i, b) => {
                p.choose(i, b);
                KbOut::Nothing
            }
            KbStep::Pick => p.cards.get(at).map_or(KbOut::Nothing, |c| KbOut::Switch(c.window, c.pid)),
            KbStep::Press(b) => p.cards.get(at).map_or(KbOut::Nothing, |c| KbOut::Act(c.window, c.pid, b, c.state, mons)),
            KbStep::Close => p.cards.get(at).map_or(KbOut::Nothing, |c| KbOut::Act(c.window, c.pid, CardBtn::Close, c.state, mons)),
            KbStep::Cancel => KbOut::Cancel,
            KbStep::Nothing => KbOut::Nothing,
        }
    })
    .unwrap_or(KbOut::Nothing);
    match out {
        // Ask it to close etc.; its card goes when the window really does.
        KbOut::Act(w, pid, b, st, mons) => {
            with_preview(|p| p.unpeek());
            unsafe { act(w, pid, b, st, mons) };
            soon_refresh();
            None
        }
        KbOut::Switch(w, pid) => {
            hide();
            same(w, pid).then_some(w)
        }
        KbOut::Cancel => {
            hide();
            None
        }
        KbOut::Nothing => None,
    }
}

/// The windows the cards show, for comparing with the dock's.
pub(super) fn windows() -> Vec<HWND> {
    with_preview(|p| p.cards.iter().map(|c| c.window).collect()).unwrap_or_default()
}

/// Close soon unless the pointer is back on the dock or the cards.
pub(super) fn schedule_hide() {
    with_preview(|p| unsafe {
        SetTimer(Some(p.hwnd), HIDE_TIMER, HIDE_MS, None);
    });
}

/// Pure: the room (DIP) a panel has next to `anchor` inside `work`: on
/// the side away from the dock, up to the work area's far edge (the
/// panel never needs to slide back over the gap), and across the dock
/// the work area less a margin.
fn room_dip(pos: Position, anchor: POINT, work: RECT, scale: f32) -> (f32, f32) {
    let s = scale.max(0.1);
    let (ww, wh) = ((work.right - work.left) as f32 / s - 16.0, (work.bottom - work.top) as f32 / s - 16.0);
    let px = |n: i32| (n.max(1) as f32 / s).floor();
    match pos {
        Position::Bottom => (ww, px(anchor.y - work.top)),
        Position::Left => (px(work.right - anchor.x), wh),
        Position::Right => (px(anchor.x - work.left), wh),
    }
}

/// Pure: the panel's top-left corner (screen px) for a panel `w_px` x
/// `h_px` (the window is the panel: no shadow margin) next to the icon,
/// away from the screen edge: its near side on `anchor` (the dock's
/// menu::menu_point, the popup gap already in it), centered on it along
/// the dock, kept inside `work`.
fn panel_origin(pos: Position, anchor: POINT, w_px: i32, h_px: i32, work: RECT) -> (i32, i32) {
    let (x, y) = match pos {
        Position::Bottom => (anchor.x - w_px / 2, anchor.y - h_px),
        Position::Left => (anchor.x, anchor.y - h_px / 2),
        Position::Right => (anchor.x - w_px, anchor.y - h_px / 2),
    };
    (x.clamp(work.left, (work.right - w_px).max(work.left)), y.clamp(work.top, (work.bottom - h_px).max(work.top)))
}

/// Show cards for `windows` of app `app`, their near side on `anchor`
/// (screen px: the popup gap off the dock, menu::menu_point).
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn show(gfx: &Rc<Gfx>, theme: &Theme, scale: f32, app: &str, windows: &[HWND], anchor: POINT, pos: Position, work: RECT, opts: Opts) {
    // Fresh (nothing open): it opens; from one app to the next: the new
    // one crosses in at full size.
    let fresh = PREVIEW.with(|p| p.try_borrow().map(|g| g.is_none()).unwrap_or(false));
    hide_now();
    if windows.is_empty() {
        return;
    }
    let pop = opts.pop;
    let anim = (fresh && pop).then(|| Opening::new(open_plan(crate::motion::now(), crate::effects::current().speed)));
    match Preview::new(gfx, theme, scale, app, windows, anchor, pos, work, opts, anim) {
        Ok(mut p) => {
            let h = p.hwnd;
            if crate::taskbar::hidden_by_us() { super::drag::register_preview(h); }
            let c = p.gfx.compositor.clone();
            // "Preview pop-in" (crate::effects). Opening fresh: the plate
            // and its shadow open as menus do (motion::open, from the
            // icon's side); the thumbnails follow frame by frame
            // (anim_step) on a clock that starts here with them (7.4: one
            // cause, one frame), not when the preview began to be made.
            // From one app to the next: a cross at full size
            // (motion::swap; the DWM thumbnails show at once).
            if let Some(a) = p.anim.as_mut() {
                a.begin(Instant::now());
                let _ = crate::motion::open(&c, &p.root, p.size, p.origin, crate::motion::Kind::Menu, p.scale);
                if let Some(sh) = &p.shade {
                    let _ = crate::motion::open(&c, &sh.outer, p.size, p.origin, crate::motion::Kind::Menu, p.scale);
                }
            } else if pop {
                let _ = crate::motion::swap(&c, None, &p.root);
                if let Some(sh) = &p.shade {
                    let _ = crate::motion::swap(&c, None, &sh.outer);
                }
            }
            let shade = p.shade.as_ref().map(|sh| sh.hwnd);
            PREVIEW.with(|x| *x.borrow_mut() = Some(p));
            crate::companion::refresh();
            let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
            if let Some(sh) = shade {
                let _ = ShowWindow(sh, SW_SHOWNOACTIVATE);
                seat_shade(sh, h);
            }
            SetTimer(Some(h), STATE_TIMER, STATE_MS, None);
            if anim.is_some() {
                SetTimer(Some(h), ANIM_TIMER, FRAME_MS, None);
            }
        }
        Err(e) => crate::log!("preview: {e}"),
    }
}

/// The shadow window's picture for cards `w` x `h` DIP at `s` px per DIP:
/// what a menu casts (E2: popup::shade_at, the menus' corners), in `outer`
/// (scaled, offset by the margin) in place of what was there.
fn fill_shade(gfx: &Gfx, theme: &Theme, outer: &ContainerVisual, w: f32, h: f32, s: f32) -> Result<()> {
    outer.Children()?.RemoveAll()?;
    let v = crate::popup::shade_at(gfx, theme, w, h, s, theme.radii().menu, crate::theme::Elevation::E2)?;
    outer.Children()?.InsertAtTop(&v)
}

/// The shadow's window right under the cards' (z order), whatever came
/// between (the peek's windows go under the dock).
unsafe fn seat_shade(shade: HWND, cards: HWND) {
    let _ = SetWindowPos(shade, Some(cards), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
}

/// The shadow's window: click-through for every app (layered and
/// transparent, as the dock's own shadow), never activated, its picture a
/// composition target scaled `scale` and offset by the shadow margin.
unsafe fn make_shade(gfx: &Gfx, scale: f32) -> Result<Shade> {
    let hinst = GetModuleHandleW(None)?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(peek_proc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarPreviewShade"),
        ..Default::default()
    };
    RegisterClassExW(&wc);
    let ex = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED;
    let hwnd = CreateWindowExW(ex, w!("NopBarPreviewShade"), PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None)?;
    let made = (|| -> Result<Shade> {
        // (Layered at full alpha: the composition content as drawn.)
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)?;
        let c = &gfx.compositor;
        let interop: ICompositorDesktopInterop = c.cast()?;
        let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
        let outer = c.CreateContainerVisual()?;
        let m = (crate::theme::metrics::SHADOW_M * scale).ceil();
        outer.SetScale(Vector3 { X: scale, Y: scale, Z: 1.0 })?;
        outer.SetOffset(Vector3 { X: m, Y: m, Z: 0.0 })?;
        target.SetRoot(&outer)?;
        Ok(Shade { hwnd, _target: target, outer })
    })();
    if made.is_err() {
        let _ = DestroyWindow(hwnd);
    }
    made
}

/// The widest a tooltip gets before it wraps (a long title in full, on
/// several lines): a third of the work area, at least 200 px (what the
/// system tooltip it replaces did). Pure: tested.
pub(super) fn tip_max_px(work: RECT) -> f32 {
    ((work.right - work.left) as f32 / 3.0).max(200.0)
}

fn title_of(h: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn pid_of(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// Still the same window of the same process?
fn same(w: HWND, pid: u32) -> bool {
    let alive = unsafe { IsWindow(Some(w)).as_bool() };
    alive && pid_of(w) == pid
}

/// Reads the window's state; none of these calls waits on its app.
fn state_of(h: HWND) -> WinState {
    unsafe {
        WinState {
            minimized: IsIconic(h).as_bool(),
            hung: IsHungAppWindow(h).as_bool(),
            topmost: super::ontop::is_on_top(h),
            locked: matches!(super::ontop::available(h), Err(r) if r != crate::traywin::Refusal::Hung),
        }
    }
}

/// The state row's text.
fn state_text(st: WinState) -> &'static str {
    if st.hung {
        crate::i18n::t("a11y.dock.not_responding")
    } else if st.minimized {
        crate::i18n::t("preview.minimized")
    } else if st.topmost {
        crate::i18n::t("preview.topmost")
    } else {
        ""
    }
}

/// Runs card button `b` on window `w`. Nothing here waits on its app: a
/// posted close, an asynchronous topmost change, the snap helper thread,
/// the dock's minimize (posted to it), a posted restore (input::focus).
unsafe fn act(w: HWND, pid: u32, b: CardBtn, st: WinState, monitors: usize) {
    if !same(w, pid) || !can(b, st, monitors) {
        return;
    }
    // It may have changed since the cards were drawn (hung meanwhile).
    let now = state_of(w);
    if !can(b, now, monitors) {
        return;
    }
    // The dock the cards were opened from.
    let dock = super::views::ui_hwnd();
    crate::log!("preview: {} on window {:#x}", b.key(), w.0 as isize);
    match b {
        CardBtn::Close => {
            let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        CardBtn::Top => {
            // Posted to the window's thread, checked a moment later.
            if let Err(r) = super::ontop::set(w, Some(!now.topmost), dock) {
                crate::log!("preview: keep on top refused for {:#x}: {r:?}", w.0 as isize);
            }
        }
        // The helper re-checks the process before every step.
        CardBtn::Move => snap::to_next_monitor(w, pid, dock, WM_SNAP_DONE),
        CardBtn::MinRestore if now.minimized => input::focus(w),
        // The dock re-checks the window and its process when it gets this.
        CardBtn::MinRestore => {
            let _ = PostMessageW(Some(dock), WM_PREVIEW_MIN, WPARAM(w.0 as usize), LPARAM(pid as isize));
        }
    }
}

/// Draws card button `b` at `br` into `tch`: its glyph (crate::glyphs) in
/// the state `st` (the pin filled while the window is on top, on the
/// theme's "on" tile), dimmed when it cannot act (`enabled` false). Returns
/// its hover highlight (enabled buttons only). Colors and radii come from
/// the theme's ui tokens; the one place the card buttons' look is set.
#[allow(clippy::too_many_arguments)]
unsafe fn card_button(c: &Compositor, gfx: &Gfx, theme: &Theme, tch: &windows::UI::Composition::VisualCollection, b: CardBtn, br: Rect4, st: WinState, enabled: bool, s: f32) -> Result<Option<SpriteVisual>> {
    let radius = theme.radii().control;
    let hc = theme.hc.filter(|_| theme.high_contrast);
    // A glyph button is part of the surface: the row hover fill (its
    // card's own hover lies under it, so the two add up).
    let hv = theme.fill_row(crate::theme::State::Hover);
    let on = b == CardBtn::Top && st.topmost;
    let mut ink = d2d(if enabled { theme.text_primary() } else { theme.text_disabled() });
    if on {
        // "On": the accent (high contrast: Highlight).
        let fill = match hc {
            Some(hc) => {
                ink = d2d(hc.highlight_text);
                hc.highlight
            }
            None => {
                ink = d2d(theme.on_accent());
                theme.accent()
            }
        };
        tch.InsertAtTop(&rounded(c, br, radius, Some(fill), None)?)?;
    } else if b == CardBtn::Top && hc.is_none() && enabled {
        // The design kit's pin: off on the tile color, on in the "on"
        // colors; the other buttons are bare until hovered.
        let face = rounded(c, br, radius, Some(theme.fill_tile(crate::theme::State::Rest)), None)?;
        tch.InsertAtTop(&face)?;
        // A raised thing: Flat v2's card light round it (wash::CardLight).
        let mut card = crate::wash::CardLight::new(c, tch, &face, &face.Brush()?)?;
        card.place(crate::wash::Shape::new(br.0, br.1, br.2, br.3, radius));
        card.show(c, theme, crate::theme::State::Rest, 0, crate::motion::EASE_OUT);
    }
    let hot = if enabled {
        // The hover: the menus' hover fill (no red, also on ×).
        let hot = match hc {
            Some(hc) => rounded(c, br, radius, None, Some((hc.highlight, 2.0)))?,
            // Over the card's own hover: one step stronger.
            None => rounded(c, br, radius, Some(hv), None)?,
        };
        hot.SetOpacity(0.0)?;
        tch.InsertAtTop(&hot)?;
        Some(hot)
    } else {
        None
    };
    if let Ok((surf, gw, gh)) = gfx.glyph(b.glyph(st), BTN_GLYPH, ink, s) {
        tch.InsertAtTop(&surface_visual(c, &surf, br.0 + (br.2 - gw) / 2.0, br.1 + (br.3 - gh) / 2.0, gw, gh)?)?;
    }
    Ok(hot)
}

/// A card's right-click menu: Always on top (checked while it is), Hide to
/// tray, Close. Shown outside the cards' borrow (the popup pumps
/// messages); the pick runs on the dock, checked against the window's
/// process again.
unsafe fn card_menu(w: HWND, pid: u32) {
    const TOP: u32 = 1;
    const TRAY: u32 = 2;
    const CLOSE: u32 = 3;
    let Some((gfx, theme, scale)) = with_preview(|p| (p.gfx.clone(), p.theme.clone(), p.scale)) else { return };
    let st = state_of(w);
    let tray = crate::traywin::tuck_refusal(&crate::traywin::facts(w), crate::traywin::contains(w), crate::traywin::count()).is_none();
    let items = vec![
        crate::popup::Item::new(TOP, crate::i18n::t("preview.topmost")).checked(st.topmost).enabled(can(CardBtn::Top, st, 1)),
        crate::popup::Item::new(TRAY, crate::i18n::t("dock.menu.send_to_tray")).enabled(tray),
        crate::popup::Item::sep(),
        crate::popup::Item::new(CLOSE, crate::i18n::t("dock.menu.close_window")),
    ];
    let mut at = POINT::default();
    let _ = GetCursorPos(&mut at);
    let out = crate::popup::track(crate::popup::Request {
        gfx: &gfx,
        theme: &theme,
        items,
        at,
        align: crate::popup::Align::Above,
        scale,
        titles: Default::default(),
        on_slide: None,
        drag_ids: 0..0,
        on_space: None,
        on_pick: None,
        hangs: false,
    });
    let crate::popup::Outcome::Picked(id) = out else { return };
    if !same(w, pid) {
        return;
    }
    match id {
        TOP => act(w, pid, CardBtn::Top, state_of(w), 1),
        TRAY => {
            super::with_dock(|d| d.send_to_tray(w));
        }
        CLOSE => act(w, pid, CardBtn::Close, state_of(w), 1),
        _ => {}
    }
    soon_refresh();
}

/// Look at the windows' states again shortly (an action's result).
fn soon_refresh() {
    with_preview(|p| unsafe {
        SetTimer(Some(p.hwnd), STATE_TIMER, STATE_SOON_MS, None);
    });
}

fn col(c: Rgba) -> Color {
    Color { A: c.a, R: c.r, G: c.g, B: c.b }
}

/// A rounded rectangle at `r`: filled and / or outlined.
unsafe fn rounded(c: &Compositor, r: Rect4, radius: f32, fill: Option<Rgba>, stroke: Option<(Rgba, f32)>) -> Result<SpriteVisual> {
    let v = c.CreateSpriteVisual()?;
    v.SetOffset(Vector3 { X: r.0, Y: r.1, Z: 0.0 })?;
    v.SetSize(Vector2 { X: r.2, Y: r.3 })?;
    if let Some(f) = fill {
        v.SetBrush(&c.CreateColorBrushWithColor(col(f))?)?;
        let g = c.CreateRoundedRectangleGeometry()?;
        g.SetSize(Vector2 { X: r.2, Y: r.3 })?;
        g.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
        v.SetClip(&c.CreateGeometricClipWithGeometry(&g)?)?;
    }
    if let Some((sc, t)) = stroke {
        let g = c.CreateRoundedRectangleGeometry()?;
        g.SetSize(Vector2 { X: r.2 - t, Y: r.3 - t })?;
        g.SetOffset(Vector2 { X: t / 2.0, Y: t / 2.0 })?;
        g.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
        let shape = c.CreateSpriteShapeWithGeometry(&g)?;
        shape.SetStrokeBrush(&c.CreateColorBrushWithColor(col(sc))?)?;
        shape.SetStrokeThickness(t)?;
        let sv = c.CreateShapeVisual()?;
        sv.Shapes()?.Append(&shape)?;
        sv.SetSize(Vector2 { X: r.2, Y: r.3 })?;
        v.Children()?.InsertAtTop(&sv)?;
    }
    Ok(v)
}

/// A drawn surface as a visual at (x, y), `w` x `h` DIP.
unsafe fn surface_visual(c: &Compositor, surf: &CompositionDrawingSurface, x: f32, y: f32, w: f32, h: f32) -> Result<SpriteVisual> {
    let v = c.CreateSpriteVisual()?;
    let b = c.CreateSurfaceBrushWithSurface(surf)?;
    b.SetStretch(CompositionStretch::Fill)?;
    v.SetBrush(&b)?;
    v.SetSize(Vector2 { X: w, Y: h })?;
    v.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 })?;
    Ok(v)
}

/// A text surface (gfx::text_fit) whose text's box is at (`x`, `y`), `w` x
/// `h`.
unsafe fn text_visual(c: &Compositor, surf: &CompositionDrawingSurface, x: f32, y: f32, w: f32, h: f32) -> Result<SpriteVisual> {
    surface_visual(c, surf, x, y, w, h)
}

fn d2d(c: Rgba) -> windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
    windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: c.a as f32 / 255.0 }
}

impl Preview {
    #[allow(clippy::too_many_arguments)]
    unsafe fn new(gfx: &Rc<Gfx>, theme: &Theme, scale: f32, app: &str, windows: &[HWND], anchor: POINT, pos: Position, work: RECT, opts: Opts, anim: Option<Opening>) -> Result<Self> {
        let hinst = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("NopBarPreview"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("NopBarPreview"),
            w!("Preview"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        let enable = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &enable as *const _ as *const _, 4);
        let c = gfx.compositor.clone();
        let setup = (|| -> Result<(DesktopWindowTarget, ContainerVisual)> {
            let interop: ICompositorDesktopInterop = c.cast()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, true)?;
            let root = c.CreateContainerVisual()?;
            root.SetScale(Vector3 { X: scale, Y: scale, Z: 1.0 })?;
            target.SetRoot(&root)?;
            Ok((target, root))
        })();
        let (target, root) = match setup {
            Ok(x) => x,
            Err(e) => {
                let _ = DestroyWindow(hwnd);
                return Err(e);
            }
        };
        let mut p = Preview {
            hwnd,
            _target: target,
            root,
            gfx: gfx.clone(),
            theme: theme.clone(),
            scale,
            app: app.to_string(),
            icon: opts.icon,
            cards: Vec::new(),
            file_card: None,
            hovered: None,
            hot: None,
            kb_btn: None,
            wheel_acc: 0,
            tracking: false,
            tips: Vec::new(),
            tip_on: None,
            tip_up: false,
            keyboard: None,
            anchor,
            pos,
            work,
            first: 0,
            shown: 0,
            footer: None,
            monitors: 1,
            peek_on: opts.peek,
            peek: None,
            shade: None,
            size: (0.0, 0.0),
            origin: (0.0, 0.0),
            anim,
            applied: match anim {
                Some(a) => {
                    let (k, o, _) = open_frame(a.plan, 0.0);
                    (k, o)
                }
                None => (1.0, 1.0),
            },
        };
        // Its shadow (without it, the cards still show).
        match make_shade(gfx, scale) {
            Ok(sh) => p.shade = Some(sh),
            Err(e) => crate::log!("preview: no shadow ({e})"),
        }
        // A failure part way (Composition out of resources, a lost device)
        // gives up the preview only: close() unregisters the thumbnails
        // made so far and destroys the window (audit A11: was a panic).
        for &w in windows {
            let hover = match c.CreateSpriteVisual() {
                Ok(v) => v,
                Err(e) => {
                    p.close();
                    return Err(e);
                }
            };
            // "Live window previews" off: the card shows the name only (and
            // there is no peek).
            let thumb = if crate::effects::items().live_previews { DwmRegisterThumbnail(hwnd, w).ok() } else { None };
            let src = thumb.and_then(|id| DwmQueryThumbnailSourceSize(id).ok()).map(|s| (s.cx, s.cy)).unwrap_or((0, 0));
            p.cards.push(Card { window: w, pid: pid_of(w), thumb, src, state: state_of(w), rect: (0.0, 0.0, 0.0, 0.0), hover, tools: None, hot: Vec::new(), dest: RECT::default() });
        }
        if let Err(e) = p.build() {
            p.close();
            return Err(e);
        }
        Ok(p)
    }

    /// The cards on the page (indexes).
    fn on_page(&self) -> std::ops::Range<usize> {
        self.first..(self.first + self.shown).min(self.cards.len())
    }

    /// The thumbnails on the page at scale `k` about the point the panel
    /// grows from, and opacity `o` (what the plate shows now).
    unsafe fn apply_thumbs(&mut self, k: f32, o: f32) {
        self.applied = (k, o);
        let at = (self.origin.0 * self.scale, self.origin.1 * self.scale);
        for i in self.on_page() {
            let card = &self.cards[i];
            if let Some(id) = card.thumb {
                let props = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_OPACITY,
                    rcDestination: scaled(card.dest, at, k),
                    opacity: (255.0 * o.clamp(0.0, 1.0)) as u8,
                    ..Default::default()
                };
                let _ = DwmUpdateThumbnailProperties(id, &props);
            }
        }
    }

    /// (Re)lays out the panel: background, cards, thumbnails, window rect.
    unsafe fn build(&mut self) -> Result<()> {
        let c = self.gfx.compositor.clone();
        let s = self.scale;
        let rtl = crate::i18n::rtl();
        self.monitors = crate::monitors::all().len().max(1);
        // Sized to the room past the anchor: kept inside the work area, it
        // never has to move over the gap toward the dock.
        let (max_w, max_h) = room_dip(self.pos, self.anchor, self.work, s);
        for card in &mut self.cards {
            card.state = state_of(card.window);
        }
        let widths: Vec<f32> = self.cards.iter().map(|k| card_width(k.src.0, k.src.1)).collect();
        let pg = page(&widths, self.first, max_w, max_h, rtl);
        self.first = pg.first;
        self.shown = pg.rects.len();
        self.footer = pg.footer;
        let (w, h) = (pg.w, pg.h);
        // Cards off the page: no thumbnail, no visuals.
        let range = self.on_page();
        for (k, card) in self.cards.iter_mut().enumerate() {
            if !range.contains(&k) {
                card.rect = (0.0, 0.0, 0.0, 0.0);
                card.dest = RECT::default();
                card.tools = None;
                card.hot.clear();
                if let Some(id) = card.thumb {
                    let props = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_VISIBLE, fVisible: BOOL(0), ..Default::default() };
                    let _ = DwmUpdateThumbnailProperties(id, &props);
                }
            }
        }
        let (w_px, h_px) = ((w * s).ceil() as i32, (h * s).ceil() as i32);
        let (x, y) = panel_origin(self.pos, self.anchor, w_px, h_px, self.work);
        let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w_px, h_px, SWP_NOACTIVATE);
        self.size = (w, h);
        // It grows from the point of its edge nearest the icon (7.4; kept
        // while it opens).
        if self.anim.is_none() || self.origin == (0.0, 0.0) {
            self.origin = crate::motion::origin((0.0, 0.0, w, h), Some(((self.anchor.x - x) as f32 / s, (self.anchor.y - y) as f32 / s, 0.0, 0.0)));
        }

        let ch = self.root.Children()?;
        ch.RemoveAll()?;
        let size = Vector2 { X: w, Y: h };
        let radii = self.theme.radii();
        // The menus' corners (a small transient panel beside the dock).
        let r = radii.menu;
        let clip = c.CreateRoundedRectangleGeometry()?;
        clip.SetSize(size)?;
        clip.SetCornerRadius(Vector2 { X: r, Y: r })?;
        self.root.SetClip(&c.CreateGeometricClipWithGeometry(&clip)?)?;
        let solid = self.theme.popup.material == "solid" || !crate::effects::items().blur;
        let menu = crate::theme::Material::Menu;
        let mut glass = None;
        if !solid {
            let backdrop = c.CreateSpriteVisual()?;
            // The blur through the glass's graph (crate::glass).
            let b = crate::glass::backdrop(&self.gfx, &self.theme, menu)?;
            backdrop.SetBrush(&b)?;
            backdrop.SetSize(size)?;
            ch.InsertAtTop(&backdrop)?;
            glass = Some(b);
        }
        // The menus' material (M3: a step lighter than a panel), for the
        // backdrop it has.
        let t = crate::glass::tint_for(&self.theme, menu, glass.as_ref());
        let tint = c.CreateSpriteVisual()?;
        tint.SetBrush(&crate::glass::tint_brush(&c, &self.theme, menu, crate::theme::Rgba { a: if solid { 255 } else { t.a }, ..t })?)?;
        tint.SetSize(size)?;
        ch.InsertAtTop(&tint)?;
        // The glass's top light and grain, under the rim (Flat v2: its
        // grain).
        if !solid || self.theme.flat_grain(menu) > 0.0 {
            if let Ok(Some(l)) = crate::glass::top_light(&self.gfx, &self.theme, menu, Some(size)) {
                ch.InsertAtTop(&l)?;
            }
        }
        // A theme's own top light (as popup::chrome; none with Glass).
        if let Some(hl) = self.theme.panel_highlight() {
            ch.InsertAtTop(&rounded(&c, (r, 1.0, (w - r * 2.0).max(0.0), 1.0), 0.0, Some(hl), None)?)?;
        }
        // The menus' rim (high contrast: the outline). Its E2 shadows and
        // contact ring are the shadow window's, right under it.
        let border = crate::popup::rim_visual(&self.gfx, &self.theme, menu, w, h, s, r)?;
        if let Some(sh) = &self.shade {
            let m = (crate::theme::metrics::SHADOW_M * s).ceil() as i32;
            if let Err(e) = fill_shade(&self.gfx, &self.theme, &sh.outer, w, h, s) {
                crate::log!("preview: shadow not drawn ({e})");
            }
            let _ = SetWindowPos(sh.hwnd, Some(self.hwnd), x - m, y - m, w_px + 2 * m, h_px + 2 * m, SWP_NOACTIVATE);
        }

        // Translucent text levels (they read over Glass on any wallpaper).
        let fg = d2d(self.theme.text_primary());
        let dim = d2d(self.theme.text_secondary());
        let hv = self.theme.fill_row(crate::theme::State::Hover);
        let hc = self.theme.hc.filter(|_| self.theme.high_contrast);
        let normal = windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
        let family = self.theme.font.family.clone();
        let mut tips: Vec<(usize, Rect4, String)> = Vec::new();
        for (j, rect) in pg.rects.iter().copied().enumerate() {
            let k = pg.first + j;
            let card = &mut self.cards[k];
            card.rect = rect;
            let st = card.state;
            let (cx, cy, cw, _) = rect;
            // Hover background (high contrast: the selection color as a
            // solid outline, like the menus, not a faint wash).
            card.hover = match hc {
                Some(hc) => rounded(&c, rect, radii.card, None, Some((hc.highlight, 2.0)))?,
                None => rounded(&c, rect, crate::theme::concentric(r, OUTER).min(radii.card), Some(hv), None)?,
            };
            card.hover.SetOpacity(0.0)?;
            fade(&c, &card.hover, HOVER_MS)?;
            ch.InsertAtTop(&card.hover)?;
            // High contrast: every card outlined, so they read as cards.
            if let Some(hc) = hc {
                ch.InsertAtTop(&rounded(&c, rect, radii.card, None, Some((hc.text, 1.0)))?)?;
            }
            // Title row: the app's icon, the title cut by its real width
            // with "…" (never squeezed; a cut one shows in full in a
            // tooltip), × at the end. Mirrored right to left.
            let mut tx = cx + CARD_PAD;
            if let Some(b) = &self.icon {
                let v = c.CreateSpriteVisual()?;
                v.SetBrush(b)?;
                v.SetSize(Vector2 { X: ICON, Y: ICON })?;
                v.SetOffset(Vector3 { X: crate::i18n::mirror::span(rtl, tx, ICON, cx, cx + cw), Y: cy + (TITLE_H - ICON) / 2.0, Z: 0.0 })?;
                ch.InsertAtTop(&v)?;
                tx += ICON + 6.0;
            }
            let title = title_of(card.window);
            let room = cx + cw - CARD_PAD / 2.0 - CLOSE - 2.0 - tx;
            if let Ok((surf, tw, th, cut)) = self.gfx.text_fit(&title, &family, crate::theme::metrics::SMALL, normal, fg, s, room.max(8.0)) {
                let vx = crate::i18n::mirror::span(rtl, tx, tw, cx, cx + cw);
                let (vx, vy) = super::label::snap_px(vx, cy + (TITLE_H - th) / 2.0, s);
                ch.InsertAtTop(&text_visual(&c, &surf, vx, vy, tw, th)?)?;
                if cut {
                    let tip_x = crate::i18n::mirror::span(rtl, cx, cw - CLOSE, cx, cx + cw);
                    tips.push((k, (tip_x, cy, cw - CLOSE, TITLE_H), title.clone()));
                }
            }
            // State row: "Not responding", "Minimized" or "Always on top".
            let row_y = cy + TITLE_H + THUMB_H;
            let tag = state_text(st);
            if !tag.is_empty() {
                let room = cw - CARD_PAD - CARD_PAD / 2.0 - 3.0 * BTN - 2.0 * BTN_GAP - 4.0;
                // "Not responding" in full ink (no red): the words carry it.
                let color = if st.hung && hc.is_none() { fg } else { dim };
                if let Ok((surf, tw, th, _)) = self.gfx.text_fit(tag, &family, crate::theme::metrics::CAPTION, normal, color, s, room.max(8.0)) {
                    let vx = crate::i18n::mirror::span(rtl, cx + CARD_PAD, tw, cx, cx + cw);
                    let (vx, vy) = super::label::snap_px(vx, row_y + (ACT_H - th) / 2.0, s);
                    ch.InsertAtTop(&text_visual(&c, &surf, vx, vy, tw, th)?)?;
                }
            }
            // × and the strip, shown with hover.
            let tools = c.CreateContainerVisual()?;
            tools.SetOpacity(0.0)?;
            fade(&c, &tools, HOVER_MS)?;
            let tch = tools.Children()?;
            card.hot.clear();
            for (b, br) in buttons(rect, rtl) {
                let enabled = can(b, st, self.monitors);
                if let Some(hot) = card_button(&c, &self.gfx, &self.theme, &tch, b, br, st, enabled, s)? {
                    card.hot.push((b, hot));
                }
                tips.push((10_000 + k * 4 + b.index(), br, b.tip(st).to_string()));
            }
            ch.InsertAtTop(&tools)?;
            card.tools = Some(tools);
            // Live thumbnail, drawn by DWM into our window.
            if let Some(id) = card.thumb {
                let src = DwmQueryThumbnailSourceSize(id).unwrap_or_default();
                let (bx, by, bw, bh) = (cx + CARD_PAD, cy + TITLE_H, cw - CARD_PAD * 2.0, THUMB_H);
                let (fx, fy, fw, fh) = fit(src.cx, src.cy, bw, bh);
                card.dest = RECT { left: ((bx + fx) * s) as i32, top: ((by + fy) * s) as i32, right: ((bx + fx + fw) * s) as i32, bottom: ((by + fy + fh) * s) as i32 };
                let (k, o) = self.applied;
                let at = (self.origin.0 * s, self.origin.1 * s);
                let props = DWM_THUMBNAIL_PROPERTIES {
                    dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                    rcDestination: scaled(card.dest, at, k),
                    opacity: (255.0 * o.clamp(0.0, 1.0)) as u8,
                    fVisible: BOOL(1),
                    fSourceClientAreaOnly: BOOL(0),
                    ..Default::default()
                };
                let _ = DwmUpdateThumbnailProperties(id, &props);
            }
        }
        // "+N more" under the cards.
        if let Some(f) = pg.footer {
            let n = pg.more.to_string();
            let label = crate::i18n::tf("preview.more", &[("n", &n)]);
            if let Ok((surf, tw, th, _)) = self.gfx.text_fit(&label, &family, crate::theme::metrics::CAPTION, normal, dim, s, f.2.max(8.0)) {
                let (vx, vy) = super::label::snap_px(f.0 + (f.2 - tw) / 2.0, f.1 + (f.3 - th) / 2.0, s);
                ch.InsertAtTop(&text_visual(&c, &surf, vx, vy, tw, th)?)?;
            }
        }
        ch.InsertAtTop(&border)?;
        self.set_tips(&tips);
        // The hover survives a rebuild (a state change, a scroll).
        self.paint_hover(self.hovered);
        self.paint_hot();
        Ok(())
    }

    unsafe fn close(mut self) {
        let _ = windows::Win32::System::Ole::RevokeDragDrop(self.hwnd);
        self.drop_tip();
        self.unpeek();
        if let Some(sh) = self.shade.take() {
            let _ = DestroyWindow(sh.hwnd);
        }
        for c in &self.cards {
            if let Some(id) = c.thumb {
                let _ = DwmUnregisterThumbnail(id);
            }
        }
        let _ = DestroyWindow(self.hwnd);
    }

    /// Closes fading out (crate::motion::retire: click-through for every
    /// app meanwhile, then destroyed). Its thumbnails and peek are DWM's
    /// and layered windows, not visuals: their opacity follows the fade
    /// step by step (motion::steps), and they go with the window.
    unsafe fn retire(mut self) {
        let _ = windows::Win32::System::Ole::RevokeDragDrop(self.hwnd);
        let _ = KillTimer(Some(self.hwnd), super::drag::FILE_HOVER_TIMER);
        self.drop_tip();
        for t in [HIDE_TIMER, PEEK_TIMER, STATE_TIMER, ANIM_TIMER, PEEK_ANIM_TIMER, TIP_TIMER] {
            let _ = KillTimer(Some(self.hwnd), t);
        }
        let thumbs: Vec<isize> = self.cards.iter().filter_map(|c| c.thumb).collect();
        // The ones on the page, where they rest.
        let shown_thumbs: Vec<(isize, RECT)> = self.on_page().filter_map(|k| self.cards[k].thumb.map(|id| (id, self.cards[k].dest))).collect();
        let peek = self.peek.take();
        // The thumbnails from what they show (a close while still opening
        // turns back from there); the plate from where Composition has it.
        let (k0, o0) = self.applied;
        let at = (self.origin.0 * self.scale, self.origin.1 * self.scale);
        let shown = (shown_thumbs, peek.as_ref().map(|p| (p.dim, p.thumb)));
        let (hwnd, c, root) = (self.hwnd, self.gfx.compositor.clone(), self.root.clone());
        // Its shadow goes with it, the same way (click-through already).
        if let Some(sh) = self.shade.take() {
            let (sh_hwnd, outer) = (sh.hwnd, sh.outer.clone());
            crate::motion::retire(sh_hwnd, &c, &outer, Box::new(sh));
        }
        crate::motion::retire_from(hwnd, &c, &root, crate::motion::Start::Current, Box::new(Fading { thumbs, peek, _preview: Some(self) }));
        if !crate::motion::retired(hwnd) {
            // Gone at once (its thumbnails and peek with it).
            return;
        }
        // As motion::close plays it (60 ms straight with reduced motion,
        // where nothing changes size).
        let mode = crate::motion::now();
        let (ms, _) = crate::motion::fade_plan(mode, crate::motion::close_ms(mode), crate::motion::EASE_IN, crate::effects::current().speed);
        let full = mode == crate::motion::Mode::Full;
        let (thumbs, peek) = shown;
        crate::motion::steps(
            ms,
            Box::new(move |l| {
                // (The plate steps back to CLOSE_SCALE from its size now.)
                let k = if full { crate::motion::CLOSE_SCALE + (k0 - crate::motion::CLOSE_SCALE) * l } else { k0 };
                let o = l * o0;
                for &(id, dest) in &thumbs {
                    let props = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_OPACITY, rcDestination: scaled(dest, at, k), opacity: (255.0 * o) as u8, ..Default::default() };
                    let _ = DwmUpdateThumbnailProperties(id, &props);
                }
                if let Some((dim, thumb)) = peek {
                    let _ = SetLayeredWindowAttributes(dim, COLORREF(0), (DIM_ALPHA as f32 * l) as u8, LWA_ALPHA);
                    let props = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_OPACITY, opacity: (255.0 * l) as u8, ..Default::default() };
                    let _ = DwmUpdateThumbnailProperties(thumb, &props);
                }
            }),
            Box::new(|| {}),
        );
    }

    fn hit(&self, x: f32, y: f32) -> CardHit {
        let cards: Vec<(usize, Rect4)> = self.on_page().map(|k| (k, self.cards[k].rect)).collect();
        hit_in(&cards, self.footer, x, y, crate::i18n::rtl())
    }

    /// The tooltips now: cut titles and the buttons (the old ones go). A
    /// tip showing stays through a rebuild while its own is unchanged;
    /// changed it shows the new text, gone it goes.
    fn set_tips(&mut self, list: &[(usize, Rect4, String)]) {
        let was = self.tip_on.and_then(|id| self.tips.iter().find(|t| t.0 == id).cloned());
        self.tips = list.to_vec();
        let Some(id) = self.tip_on else { return };
        match self.tips.iter().find(|t| t.0 == id) {
            None => self.drop_tip(),
            Some(now) if self.tip_up && Some(now) != was.as_ref() => self.show_tip(),
            Some(_) => {}
        }
    }

    /// The pointer at (x, y), window DIPs: the tooltip under it waits,
    /// follows or goes (tip_step).
    fn track_tip(&mut self, x: f32, y: f32) {
        match tip_step(self.tip_on, self.tip_up, tip_under(&self.tips, x, y)) {
            TipStep::Keep => {}
            TipStep::Hide => self.drop_tip(),
            TipStep::Show(id) => {
                self.tip_on = Some(id);
                self.show_tip();
            }
            TipStep::Wait(id) => {
                self.tip_on = Some(id);
                unsafe {
                    SetTimer(Some(self.hwnd), TIP_TIMER, crate::topbar::TIP_DELAY_MS, None);
                }
            }
        }
    }

    /// The wait is over: the tip under the pointer shows.
    fn tip_due(&mut self) {
        if self.tip_on.is_some() {
            self.tip_up = true;
            self.show_tip();
        }
    }

    /// No tip (and the next one waits again).
    fn drop_tip(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIP_TIMER);
        }
        if self.tip_up {
            crate::topbar::hide_tip();
        }
        self.tip_on = None;
        self.tip_up = false;
    }

    /// Tip `tip_on` centered under its title or button (screen px), kept
    /// in the work area; a long one wraps (tip_max_px).
    fn show_tip(&self) {
        let Some((_, (x, y, w, h), text)) = self.tip_on.and_then(|id| self.tips.iter().find(|t| t.0 == id)) else { return };
        let s = self.scale;
        let mut o = POINT::default();
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut o);
        }
        let gap = (crate::theme::metrics::space::S1 * s).round() as i32;
        let at = POINT { x: o.x + ((x + w / 2.0) * s).round() as i32, y: o.y + ((y + h) * s).round() as i32 + gap };
        crate::topbar::show_tip_wrapped(&self.gfx, &self.theme, s, text, at, false, self.work, tip_max_px(self.work));
    }

    /// The chosen card's full title (the window's name; the card may show
    /// it shortened) or its focused button's name, for a screen reader.
    fn announce(&self, i: usize) {
        if let Some(c) = self.cards.get(i) {
            let text = match self.kb_btn {
                Some(b) => b.name(c.state).to_string(),
                None => title_of(c.window),
            };
            unsafe {
                let _ = SetWindowTextW(self.hwnd, &windows::core::HSTRING::from(text.as_str()));
            }
        }
    }

    /// The keyboard chooser's focus to card `k` (its button `b`, or the
    /// card itself): in view, lit, its button marked, announced.
    fn choose(&mut self, k: usize, b: Option<CardBtn>) {
        if k >= self.cards.len() {
            return;
        }
        let b = b.filter(|b| can(*b, self.cards[k].state, self.monitors));
        self.keyboard = Some(k);
        self.kb_btn = b;
        self.ensure_visible(k);
        self.set_hover(Some(k));
        self.paint_hot();
        self.announce(k);
    }

    /// Card `i` lit (with its tools), the others not.
    fn paint_hover(&self, i: Option<usize>) {
        for (k, c) in self.cards.iter().enumerate() {
            let on = Some(k) == i;
            let _ = c.hover.SetOpacity(if on { 1.0 } else { 0.0 });
            if let Some(t) = &c.tools {
                let _ = t.SetOpacity(if on { 1.0 } else { 0.0 });
            }
        }
    }

    /// Button highlights: the one under the pointer and the one with the
    /// keyboard focus (in high contrast an outline: the focus ring).
    fn paint_hot(&self) {
        let kb = self.keyboard.zip(self.kb_btn);
        for (k, c) in self.cards.iter().enumerate() {
            for (b, v) in &c.hot {
                let on = self.hot == Some((k, *b)) || kb == Some((k, *b));
                let _ = v.SetOpacity(if on { 1.0 } else { 0.0 });
            }
        }
    }

    fn set_hover(&mut self, i: Option<usize>) {
        // The keyboard's card stays lit while the pointer wanders.
        let i = i.or(self.keyboard);
        if self.hovered == i {
            return;
        }
        self.paint_hover(i);
        self.hovered = i;
        // Resting on a card peeks at its window.
        self.unpeek();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), PEEK_TIMER);
            if self.peek_on && i.is_some() {
                SetTimer(Some(self.hwnd), PEEK_TIMER, PEEK_MS, None);
            }
        }
    }

    fn set_hot(&mut self, h: Option<(usize, CardBtn)>) {
        if self.hot != h {
            self.hot = h;
            self.paint_hot();
        }
    }

    /// Scrolls so card `i` is on the page.
    fn ensure_visible(&mut self, i: usize) {
        for _ in 0..=self.cards.len() {
            let nf = scroll_to(self.first, self.shown, i);
            if nf == self.first {
                return;
            }
            self.first = nf;
            self.unpeek();
            let _ = unsafe { self.build() };
        }
    }

    /// Shows the page from `first` (scroll, "+N more"). In the keyboard
    /// chooser a chosen card that left the page gives the choice to the
    /// nearest card on it (keys never act on a card out of view) and the
    /// dock tells screen readers.
    fn go_to(&mut self, first: usize) {
        if first == self.first {
            return;
        }
        self.first = first;
        self.hovered = None;
        self.hot = None;
        self.unpeek();
        let _ = unsafe { self.build() };
        if let Some(k) = self.keyboard {
            let page = self.on_page();
            let to = keep_on_page(k, page.start, page.end);
            if to != k {
                self.choose(to, None);
            } else {
                self.set_hover(Some(k));
            }
            let dock = super::views::ui_hwnd();
            unsafe {
                let _ = PostMessageW(Some(dock), WM_PREVIEW_A11Y, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// The windows' states changed (minimized elsewhere, not responding):
    /// the cards are drawn again.
    fn refresh_states(&mut self) {
        let changed = self.on_page().any(|k| state_of(self.cards[k].window) != self.cards[k].state);
        if changed {
            // A peeked window that was just minimized has nothing to show.
            self.unpeek();
            let _ = unsafe { self.build() };
        }
    }

    /// One frame of the thumbnails' open (the plate's is Composition's,
    /// on the same curves).
    unsafe fn anim_step(&mut self) {
        let Some(a) = self.anim else {
            let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
            return;
        };
        let (k, o, over) = a.frame(Instant::now());
        self.apply_thumbs(k, o);
        if over {
            self.anim = None;
            let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
        }
    }

    /// Shows the hovered card's window large (see the module notes).
    unsafe fn peek(&mut self) {
        self.unpeek();
        let Some(k) = self.hovered else { return };
        let Some(card) = self.cards.get(k) else { return };
        // A minimized window has nothing to show.
        // "Live window previews" off: static cards, no peek either.
        if !self.peek_on || card.thumb.is_none() || !crate::effects::items().live_previews || card.state.minimized || !same(card.window, card.pid) {
            return;
        }
        let src = card.thumb.and_then(|id| DwmQueryThumbnailSourceSize(id).ok()).map(|s| (s.cx, s.cy)).unwrap_or(card.src);
        let Some(r) = peek_rect(src.0, src.1, self.work) else { return };
        // Fades in with "Preview pop-in" (crate::effects).
        let ms = open_ms();
        match make_peek(self.hwnd, card.window, self.work, r, ms) {
            Ok(p) => {
                if ms > 0 {
                    SetTimer(Some(self.hwnd), PEEK_ANIM_TIMER, FRAME_MS, None);
                }
                self.peek = Some(p);
                if let Some(sh) = &self.shade {
                    seat_shade(sh.hwnd, self.hwnd);
                }
            }
            Err(e) => crate::log!("preview: no peek ({e})"),
        }
    }

    fn unpeek(&mut self) {
        if let Some(p) = self.peek.take() {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), PEEK_ANIM_TIMER);
                p.close();
            }
        }
    }
}

/// The peek's two click-through windows: the dimmed work area and, above
/// it, `target`'s thumbnail at `at`. Both go just under the dock; the cards
/// stay on top of everything.
unsafe fn make_peek(preview: HWND, target: HWND, work: RECT, at: RECT, ms: u32) -> Result<Peek> {
    let hinst = GetModuleHandleW(None)?;
    let black = windows::Win32::Graphics::Gdi::GetStockObject(windows::Win32::Graphics::Gdi::BLACK_BRUSH);
    let dim_class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(peek_proc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarPeekDim"),
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(black.0),
        ..Default::default()
    };
    RegisterClassExW(&dim_class);
    let frame_class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(peek_proc),
        hInstance: hinst.into(),
        lpszClassName: w!("NopBarPeek"),
        ..Default::default()
    };
    RegisterClassExW(&frame_class);
    let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED;
    let dim = CreateWindowExW(ex, w!("NopBarPeekDim"), PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None)?;
    let frame = match CreateWindowExW(ex | WS_EX_NOREDIRECTIONBITMAP, w!("NopBarPeek"), PCWSTR::null(), WS_POPUP, 0, 0, 1, 1, None, None, Some(hinst.into()), None) {
        Ok(f) => f,
        Err(e) => {
            let _ = DestroyWindow(dim);
            return Err(e);
        }
    };
    let fail = |e: windows::core::Error| {
        let _ = DestroyWindow(frame);
        let _ = DestroyWindow(dim);
        e
    };
    let first = if ms == 0 { DIM_ALPHA } else { 0 };
    SetLayeredWindowAttributes(dim, COLORREF(0), first, LWA_ALPHA).map_err(fail)?;
    SetLayeredWindowAttributes(frame, COLORREF(0), 255, LWA_ALPHA).map_err(fail)?;
    let thumb = DwmRegisterThumbnail(frame, target).map_err(fail)?;
    let (w, h) = (at.right - at.left, at.bottom - at.top);
    let props = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
        rcDestination: RECT { left: 0, top: 0, right: w, bottom: h },
        opacity: if ms == 0 { 255 } else { 0 },
        fVisible: BOOL(1),
        fSourceClientAreaOnly: BOOL(0),
        ..Default::default()
    };
    let _ = DwmUpdateThumbnailProperties(thumb, &props);
    // Under the dock (so it is not dimmed) and the cards above all.
    let dock = super::views::ui_hwnd();
    let after = if dock.is_invalid() { HWND_TOPMOST } else { dock };
    let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
    let _ = SetWindowPos(frame, Some(after), at.left, at.top, w, h, flags);
    let _ = SetWindowPos(dim, Some(frame), work.left, work.top, work.right - work.left, work.bottom - work.top, flags);
    let _ = SetWindowPos(preview, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    Ok(Peek { dim, frame, thumb, start: Instant::now(), ms })
}

unsafe extern "system" fn peek_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Placed in physical px by us; never resized for a DPI change.
        WM_DPICHANGED => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn local(lp: LPARAM, scale: f32) -> (f32, f32) {
    ((lp.0 & 0xFFFF) as i16 as f32 / scale, ((lp.0 >> 16) & 0xFFFF) as i16 as f32 / scale)
}

/// A click on the cards, decided inside the borrow and done outside it.
enum Click {
    Switch(HWND, u32),
    Act(HWND, u32, CardBtn, WinState, usize),
    Nothing,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Fading out (Preview::retire): not the open preview; nothing reaches it.
    if crate::motion::retired(hwnd) {
        return if msg == WM_NCHITTEST { LRESULT(HTTRANSPARENT as isize) } else { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    match msg {
        WM_MOUSEMOVE => {
            with_preview(|p| {
                let _ = KillTimer(Some(hwnd), HIDE_TIMER);
                if !p.tracking {
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                    p.tracking = true;
                }
                let (x, y) = local(lp, p.scale);
                p.track_tip(x, y);
                match p.hit(x, y) {
                    CardHit::Card(k, b) => {
                        p.set_hover(Some(k));
                        let st = p.cards[k].state;
                        let m = p.monitors;
                        p.set_hot(b.filter(|b| can(*b, st, m)).map(|b| (k, b)));
                    }
                    _ => {
                        p.set_hover(None);
                        p.set_hot(None);
                    }
                }
            });
            LRESULT(0)
        }
        WM_MOUSELEAVE_MSG => {
            with_preview(|p| {
                p.tracking = false;
                p.drop_tip();
                p.set_hover(None);
                p.set_hot(None);
                p.unpeek();
                SetTimer(Some(hwnd), HIDE_TIMER, HIDE_MS, None);
            });
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_MBUTTONUP => {
            let click = with_preview(|p| {
                let (x, y) = local(lp, p.scale);
                match p.hit(x, y) {
                    CardHit::Footer if msg == WM_LBUTTONUP => {
                        let n = p.cards.len();
                        let f = next_page(p.first, p.shown, n);
                        p.go_to(f);
                        Click::Nothing
                    }
                    // Middle click anywhere on a card: close it (taskbar).
                    CardHit::Card(k, _) if msg == WM_MBUTTONUP => {
                        let c = &p.cards[k];
                        Click::Act(c.window, c.pid, CardBtn::Close, c.state, p.monitors)
                    }
                    CardHit::Card(k, Some(b)) => {
                        let c = &p.cards[k];
                        Click::Act(c.window, c.pid, b, c.state, p.monitors)
                    }
                    CardHit::Card(k, None) => Click::Switch(p.cards[k].window, p.cards[k].pid),
                    _ => Click::Nothing,
                }
            })
            .unwrap_or(Click::Nothing);
            match click {
                // Ask it to close etc.; a closed window's card goes when it
                // really does (the dock's next window list, see sync()).
                Click::Act(w, pid, b, st, mons) => {
                    with_preview(|p| p.unpeek());
                    act(w, pid, b, st, mons);
                    soon_refresh();
                }
                Click::Switch(w, pid) if same(w, pid) => {
                    hide();
                    input::focus(w);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let card = with_preview(|p| {
                let (x, y) = local(lp, p.scale);
                match p.hit(x, y) {
                    CardHit::Card(k, _) => Some((p.cards[k].window, p.cards[k].pid)),
                    _ => None,
                }
            })
            .flatten();
            if let Some((w, pid)) = card {
                with_preview(|p| p.unpeek());
                card_menu(w, pid);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            // More cards than shown: one card on or back per whole notch
            // (fine-grained wheels and touchpads add up; the rest waits).
            let delta = (wp.0 >> 16) as u16 as i16;
            with_preview(|p| {
                if p.shown >= p.cards.len() {
                    p.wheel_acc = 0;
                    return;
                }
                let (steps, rest) = super::sound::notches(p.wheel_acc, delta as i32);
                p.wheel_acc = rest;
                let n = p.cards.len();
                let mut f = p.first;
                for _ in 0..steps.unsigned_abs().min(n as u32) {
                    f = scroll(f, p.shown, n, steps < 0);
                }
                p.go_to(f);
            });
            LRESULT(0)
        }
        WM_TIMER if wp.0 == super::drag::FILE_HOVER_TIMER => {
            let _ = KillTimer(Some(hwnd), wp.0);
            let mut pt = POINT::default();
            let card = with_preview(|p| p.file_card.take()).flatten();
            if crate::taskbar::hidden_by_us() && GetCursorPos(&mut pt).is_ok() && WindowFromPoint(pt) == hwnd && file_card(hwnd, pt) == card {
                if let Some((w, _)) = card.filter(|(w, pid)| same(*w, *pid)) {
                    hide();
                    input::focus(w);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == HIDE_TIMER => {
            let _ = KillTimer(Some(hwnd), HIDE_TIMER);
            // Still over the cards or the dock: stay.
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let under = GetAncestor(WindowFromPoint(pt), GA_ROOT);
            if under != hwnd && !super::views::is_dock(under) {
                hide();
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIP_TIMER => {
            let _ = KillTimer(Some(hwnd), TIP_TIMER);
            with_preview(|p| p.tip_due());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == PEEK_TIMER => {
            let _ = KillTimer(Some(hwnd), PEEK_TIMER);
            with_preview(|p| p.peek());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == PEEK_ANIM_TIMER => {
            let done = with_preview(|p| p.peek.as_ref().is_none_or(|k| k.step())).unwrap_or(true);
            if done {
                let _ = KillTimer(Some(hwnd), PEEK_ANIM_TIMER);
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == STATE_TIMER => {
            // Back to the slow pace after a quick look.
            SetTimer(Some(hwnd), STATE_TIMER, STATE_MS, None);
            with_preview(|p| p.refresh_states());
            LRESULT(0)
        }
        WM_TIMER if wp.0 == ANIM_TIMER => {
            if with_preview(|p| p.anim_step()).is_none() {
                let _ = KillTimer(Some(hwnd), ANIM_TIMER);
            }
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    /// The keyboard chooser opens from the dock's keyboard mode (Enter on
    /// an app with several windows) and moves only on its keys: a media,
    /// volume or letter key does nothing (crate::input).
    #[test]
    fn the_chooser_moves_only_on_its_keys() {
        let keys = [0x25, 0x26, 0x27, 0x28, 0x24, 0x23, 0x0D, 0x20, 0x1B, 0x09, 0x2E];
        let all = |_: usize, _: CardBtn| true;
        for vk in crate::input::sample::QUIET_OPEN {
            assert_eq!(kb_step_with(vk, false, false, 1, None, 3, &all, keys), KbStep::Nothing, "{vk:#x}");
        }
    }

    use super::*;

    /// Signed space (px) between the panel at `(x, y)` (`w` x `h`) and
    /// the dock's outer edge `edge` (bottom: a y; sides: an x).
    fn space(pos: Position, edge: i32, x: i32, y: i32, w: i32, h: i32) -> i32 {
        match pos {
            Position::Bottom => edge - (y + h),
            Position::Left => x - edge,
            Position::Right => edge - (x + w),
        }
    }

    /// A dock on `pos` just outside `work` (bar `d` px deep) with an icon
    /// `out` px past the bar: (bar, peak, the outer edge).
    fn dock_at(pos: Position, work: RECT, d: i32, out: i32) -> (RECT, RECT, i32) {
        match pos {
            Position::Bottom => {
                let bar = RECT { left: work.left + 100, top: work.bottom, right: work.right - 100, bottom: work.bottom + d };
                let pk = RECT { left: bar.left + 50, top: bar.top - out, right: bar.left + 50 + d, bottom: bar.bottom };
                (bar, pk, pk.top.min(bar.top))
            }
            Position::Left => {
                let bar = RECT { left: work.left - d, top: work.top + 100, right: work.left, bottom: work.bottom - 100 };
                let pk = RECT { left: bar.left, top: bar.top + 50, right: bar.right + out, bottom: bar.top + 50 + d };
                (bar, pk, pk.right.max(bar.right))
            }
            Position::Right => {
                let bar = RECT { left: work.right, top: work.top + 100, right: work.right + d, bottom: work.bottom - 100 };
                let pk = RECT { left: bar.left - out, top: bar.top + 50, right: bar.right, bottom: bar.top + 50 + d };
                (bar, pk, pk.left.min(bar.left))
            }
        }
    }

    #[test]
    fn the_entrance_never_comes_into_the_gap() {
        // Review P2 #1: the panel drifts in from the side away from the
        // dock, so on every frame it is at least the gap off it, and it
        // lands exactly the gap off it.
        let work = RECT { left: 100, top: 0, right: 2460, bottom: 1300 };
        for pos in [Position::Bottom, Position::Left, Position::Right] {
            for s in [1.0f32, 1.25, 1.5, 2.0] {
                let gap = crate::placement::popup_gap(s);
                for out in [-(8.0 * s) as i32, (40.0 * s) as i32] {
                    let (bar, pk, edge) = dock_at(pos, work, (64.0 * s) as i32, out);
                    let at = super::super::menu::menu_point(pos, bar, Some(pk), POINT::default(), gap);
                    let (w, h) = (500, 300);
                    let (x, y) = panel_origin(pos, at, w, h, work);
                    // It grows from the point of its near edge over the
                    // icon (motion::origin): scaled about it, that edge
                    // never moves, so no frame comes into the gap.
                    let o = crate::motion::origin((0.0, 0.0, w as f32, h as f32), Some(((at.x - x) as f32, (at.y - y) as f32, 0.0, 0.0)));
                    let near = |r: RECT| match pos {
                        Position::Bottom => r.bottom,
                        Position::Left => r.left,
                        Position::Right => r.right,
                    };
                    let rest = RECT { left: 0, top: 0, right: w, bottom: h };
                    for k in [crate::motion::OPEN_SCALE, 0.98, 1.0] {
                        let f = scaled(rest, o, k);
                        assert_eq!(near(f), near(rest), "{pos:?} {s} {out} at {k}");
                        let g = space(pos, edge, x + f.left, y + f.top, f.right - f.left, f.bottom - f.top);
                        assert!(g >= gap, "{pos:?} {s} {out} at {k}: {g} < {gap}");
                    }
                    assert_eq!(space(pos, edge, x, y, w, h), gap, "{pos:?} {s} lands on the gap");
                }
            }
        }
    }

    #[test]
    fn it_opens_as_menus_do() {
        use crate::motion::{Mode, EASE_OUT, LINEAR, MENU_FADE_MS, OPEN_MS, OPEN_SCALE, REDUCED_MS};
        // Full motion: GLIDE from 0.96 over OPEN_MS, the menu fade on the
        // ease-out, both stretched by the speed.
        let p = open_plan(Mode::Full, 1.0);
        assert_eq!((p.grow, p.fade, p.curve), (OPEN_MS, MENU_FADE_MS, EASE_OUT));
        assert_eq!(open_plan(Mode::Full, 0.5).grow, crate::effects::ms_at(OPEN_MS, 0.5));
        let (k, o, over) = open_frame(p, 0.0);
        assert_eq!((k, o, over), (OPEN_SCALE, 0.0, false));
        let (k, o, _) = open_frame(p, 60.0);
        assert!(k > OPEN_SCALE && k < 1.0 && o > 0.0 && o < 1.0);
        let (k1, _, _) = open_frame(p, 120.0);
        assert!(k1 > k, "it keeps growing");
        assert_eq!(open_frame(p, OPEN_MS as f32), (1.0, 1.0, true));
        assert_eq!(open_frame(p, f32::NAN), (1.0, 1.0, true));
        // Reduced motion: no growing, 60 ms straight, whatever the speed.
        let r = open_plan(Mode::Reduced, 0.25);
        assert_eq!((r.grow, r.fade, r.curve), (0, REDUCED_MS, LINEAR));
        let (k, o, over) = open_frame(r, 30.0);
        assert_eq!(k, 1.0);
        assert!((o - 0.5).abs() < 0.01 && !over);
        assert!(open_frame(r, REDUCED_MS as f32).2);
        // A thumbnail scales about the same point as the plate.
        let d = RECT { left: 100, top: 20, right: 300, bottom: 140 };
        assert_eq!(scaled(d, (200.0, 160.0), 1.0), d);
        assert_eq!(scaled(d, (200.0, 160.0), 0.5), RECT { left: 150, top: 90, right: 250, bottom: 150 });
    }

    /// The panels' review (2026-11-11, P2): the thumbnails' clock started
    /// before the preview was made, the plate's when it was: what making it
    /// took (windows, targets, shadow, thumbnails) was counted as open
    /// already, and with reduced motion they could be whole before the
    /// plate began to fade. A slow make is put in between.
    #[test]
    fn the_thumbnails_open_on_the_plates_clock() {
        use std::time::Duration;
        let ms = |n: u64| Duration::from_millis(n);
        for (mode, speed) in [(crate::motion::Mode::Full, 1.0), (crate::motion::Mode::Reduced, 1.0), (crate::motion::Mode::Full, 0.5)] {
            let plan = open_plan(mode, speed);
            let made = Instant::now();
            let mut a = Opening::new(plan);
            // Being made (150 ms, longer than the reduced fade): the open's
            // first frame, not over.
            assert_eq!(a.frame(made + ms(150)), (open_frame(plan, 0.0).0, open_frame(plan, 0.0).1, false), "{mode:?}: before the plate");
            // The plate's open begins: the thumbnails' with it.
            let start = made + ms(150);
            a.begin(start);
            assert_eq!(a.frame(start), open_frame(plan, 0.0), "{mode:?}: the same first frame");
            for t in [10u64, 30, 59, 90, 200] {
                assert_eq!(a.frame(start + ms(t)), open_frame(plan, t as f32), "{mode:?} at {t} ms");
            }
            let end = plan.grow.max(plan.fade) as u64;
            assert!(!a.frame(start + ms(end.saturating_sub(5))).2, "{mode:?}: still opening just before its end");
            assert!(a.frame(start + ms(end)).2, "{mode:?}: over with the plate's");
            // A frame from before it began (a timer already queued) is its
            // first.
            assert_eq!(a.frame(made), open_frame(plan, 0.0));
        }
        // The function the frame timer runs reads only this clock.
        let src = include_str!("preview.rs");
        let step = &src[src.find("unsafe fn anim_step").unwrap()..];
        let step = &step[..step.find("unsafe fn peek").unwrap()];
        assert!(step.contains("a.frame(Instant::now())") && !step.contains("elapsed"), "{step}");
        // And the clock begins right where the plate's open does.
        let show = &src[src.find("pub(super) unsafe fn show(").unwrap()..];
        let (begin, open) = (show.find("a.begin(Instant::now())").unwrap(), show.find("crate::motion::open(&c, &p.root").unwrap());
        assert!(begin < open && show[begin..open].matches(';').count() == 1, "nothing between the two");
    }

    #[test]
    fn its_shadow_is_a_window_of_its_own() {
        // The E2 shadows and contact ring: popup::shade_at in a
        // click-through window right under the cards; the panel's own
        // window stays the plate (no margin taking the dock's clicks).
        let src = include_str!("preview.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(code.contains("crate::popup::shade_at(gfx, theme, w, h, s, theme.radii().menu, crate::theme::Elevation::E2)"));
        assert!(code.contains(r#"w!("NopBarPreviewShade")"#) && code.contains("WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED"));
        // Opened, crossed and closed as every surface: motion's.
        for used in ["crate::motion::open(&c, &p.root", "crate::motion::open(&c, &sh.outer", "crate::motion::swap(", "crate::motion::retire(sh_hwnd", "crate::motion::fade_plan("] {
            assert!(code.contains(used), "{used}");
        }
        // (Spelled in pieces: dock::implicit_tests scans this file.)
        let gone = ["slide_off".to_string(), ["effects", "::ms(crate::motion::close_ms"].concat(), ["fa", "de(&c, &p.root, POP_MS)"].concat()];
        for g in gone {
            assert!(!code.contains(&g), "{g}");
        }
    }

    #[test]
    fn a_big_preview_keeps_to_the_room_past_the_anchor() {
        // Review P2 #2: many or wide windows on a small, high-DPI screen.
        // Sized to the room past the anchor, the panel is never pushed back
        // over the gap and stays inside the work area.
        let wide = card_width(3840, 1080);
        let widths = vec![wide; 8];
        for s in [1.0f32, 1.5, 2.0] {
            let gap = crate::placement::popup_gap(s);
            // 1024 x 768 under a top bar 64 px deep.
            let screen = RECT { left: 0, top: 64, right: 1024, bottom: 768 };
            for pos in [Position::Bottom, Position::Left, Position::Right] {
                let d = (64.0 * s) as i32;
                let work = match pos {
                    Position::Bottom => RECT { bottom: screen.bottom - d, ..screen },
                    Position::Left => RECT { left: d, ..screen },
                    Position::Right => RECT { right: screen.right - d, ..screen },
                };
                for out in [-(8.0 * s) as i32, (40.0 * s) as i32] {
                    let (bar, pk, edge) = dock_at(pos, work, d, out);
                    let at = super::super::menu::menu_point(pos, bar, Some(pk), POINT::default(), gap);
                    let (max_w, max_h) = room_dip(pos, at, work, s);
                    let pg = page(&widths, 0, max_w, max_h, false);
                    let (w, h) = ((pg.w * s).ceil() as i32, (pg.h * s).ceil() as i32);
                    let (x, y) = panel_origin(pos, at, w, h, work);
                    assert_eq!(space(pos, edge, x, y, w, h), gap, "{pos:?} {s} {out}: {w}x{h}");
                    assert!(x >= work.left && y >= work.top && x + w <= work.right && y + h <= work.bottom, "{pos:?} {s} {out}: inside");
                }
            }
        }
        // The review's case: a left dock, the anchor at x=180 on a 1024 px
        // wide work area, a panel that wanted 900 px.
        let work = RECT { left: 0, top: 0, right: 1024, bottom: 768 };
        let at = POINT { x: 180, y: 400 };
        let (max_w, max_h) = room_dip(Position::Left, at, work, 1.0);
        assert_eq!(max_w, 844.0);
        let pg = page(&[440.0, 440.0], 0, max_w, max_h, false);
        let w = pg.w.ceil() as i32;
        assert!(w <= 844, "{w}");
        assert_eq!(panel_origin(Position::Left, at, w, pg.h.ceil() as i32, work).0, 180, "not moved back over the gap");
    }

    #[test]
    fn previews_sit_the_popup_gap_off_the_dock() {
        // The panel (its window, no shadow margin) exactly popup_gap px off
        // the dock: past the bar, or past the tallest a magnified icon gets
        // (peak) when that stands out further.
        let work = RECT { left: 0, top: 0, right: 2560, bottom: 1440 };
        for (s, want) in [(1.0f32, 6), (1.25, 8), (1.5, 9), (2.0, 12)] {
            let gap = crate::placement::popup_gap(s);
            assert_eq!(gap, want, "{s}");
            let d = (64.0 * s) as i32;
            let (w, h) = (500, 200);
            for out in [-(8.0 * s) as i32, (40.0 * s) as i32] {
                let bar = RECT { left: 800, top: 1440 - d, right: 1800, bottom: 1440 };
                let peak = RECT { left: 1000, top: bar.top - out, right: 1000 + d, bottom: 1440 - (8.0 * s) as i32 };
                let at = super::super::menu::menu_point(Position::Bottom, bar, Some(peak), POINT::default(), gap);
                let (x, y) = panel_origin(Position::Bottom, at, w, h, work);
                assert_eq!(peak.top.min(bar.top) - (y + h), gap, "bottom {s} {out}");
                assert_eq!(x + w / 2, (peak.left + peak.right) / 2, "centered on the icon");
                let side = RECT { left: 0, top: 300, right: d, bottom: 1100 };
                let pk = RECT { left: (8.0 * s) as i32, top: 500, right: d + out, bottom: 500 + d };
                let at = super::super::menu::menu_point(Position::Left, side, Some(pk), POINT::default(), gap);
                let (x, _) = panel_origin(Position::Left, at, w, h, work);
                assert_eq!(x - pk.right.max(side.right), gap, "left {s} {out}");
                let side_r = RECT { left: 2560 - d, top: 300, right: 2560, bottom: 1100 };
                let pk_r = RECT { left: side_r.left - out, top: 500, right: 2560 - (8.0 * s) as i32, bottom: 500 + d };
                let at = super::super::menu::menu_point(Position::Right, side_r, Some(pk_r), POINT::default(), gap);
                let (x, _) = panel_origin(Position::Right, at, w, h, work);
                assert_eq!(pk_r.left.min(side_r.left) - (x + w), gap, "right {s} {out}");
            }
        }
    }

    fn r(l: i32, t: i32, rr: i32, b: i32) -> RECT {
        RECT { left: l, top: t, right: rr, bottom: b }
    }

    #[test]
    fn keyboard_chooser_moves_picks_closes_and_cancels() {
        // left, up, right, down, home, end, enter, space, esc, tab, delete
        let k = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let all = |_: usize, _: CardBtn| true;
        let step = |vk, at, btn| kb_step_with(vk, false, false, at, btn, 3, &all, k);
        assert_eq!(step(3, 0, None), KbStep::To(1, None));
        assert_eq!(step(3, 2, None), KbStep::To(0, None), "wraps like Alt+Tab");
        assert_eq!(step(1, 0, None), KbStep::To(2, None));
        assert_eq!(step(4, 1, None), KbStep::To(2, None));
        assert_eq!(step(2, 1, None), KbStep::To(0, None));
        assert_eq!(step(6, 0, None), KbStep::To(2, None));
        assert_eq!(step(5, 2, None), KbStep::To(0, None));
        assert_eq!(step(7, 1, None), KbStep::Pick);
        assert_eq!(step(8, 1, None), KbStep::Pick);
        assert_eq!(step(11, 1, None), KbStep::Close, "Delete closes the chosen window");
        assert_eq!(step(9, 1, None), KbStep::Cancel);
        assert_eq!(step(99, 1, None), KbStep::Nothing);
        assert_eq!(step(3, 9, None), KbStep::To(0, None), "a card past the end comes back inside");
        // Arrows from a button go to the next card itself.
        assert_eq!(step(3, 0, Some(CardBtn::Move)), KbStep::To(1, None));
    }

    #[test]
    fn tab_reaches_every_usable_button_and_enter_runs_it() {
        let k = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        // Card 1 cannot move (one monitor, say).
        let usable = |c: usize, b: CardBtn| !(c == 1 && b == CardBtn::Move);
        let tab = |at, btn, shift| kb_step_with(10, shift, false, at, btn, 3, &usable, k);
        // Forward: card, ×, always on top, next monitor, minimize, next card.
        assert_eq!(tab(0, None, false), KbStep::To(0, Some(CardBtn::Close)));
        assert_eq!(tab(0, Some(CardBtn::Close), false), KbStep::To(0, Some(CardBtn::Top)));
        assert_eq!(tab(0, Some(CardBtn::Top), false), KbStep::To(0, Some(CardBtn::Move)));
        assert_eq!(tab(0, Some(CardBtn::Move), false), KbStep::To(0, Some(CardBtn::MinRestore)));
        assert_eq!(tab(0, Some(CardBtn::MinRestore), false), KbStep::To(1, None));
        // A button that cannot act is skipped.
        assert_eq!(tab(1, Some(CardBtn::Top), false), KbStep::To(1, Some(CardBtn::MinRestore)));
        // Backward: to the card, then into the previous card's last button.
        assert_eq!(tab(1, Some(CardBtn::Close), true), KbStep::To(1, None));
        assert_eq!(tab(1, None, true), KbStep::To(0, Some(CardBtn::MinRestore)));
        assert_eq!(tab(0, None, true), KbStep::To(2, Some(CardBtn::MinRestore)), "wraps");
        // A card with no usable button: Shift+Tab lands on the card.
        let none = |_: usize, _: CardBtn| false;
        assert_eq!(kb_step_with(10, true, false, 1, None, 3, &none, k), KbStep::To(0, None));
        assert_eq!(kb_step_with(10, false, false, 1, None, 3, &none, k), KbStep::To(2, None));
        // Enter / Space run the focused button; Esc goes back to the card.
        let all = |_: usize, _: CardBtn| true;
        assert_eq!(kb_step_with(7, false, false, 0, Some(CardBtn::Top), 3, &all, k), KbStep::Press(CardBtn::Top));
        assert_eq!(kb_step_with(8, false, false, 0, Some(CardBtn::MinRestore), 3, &all, k), KbStep::Press(CardBtn::MinRestore));
        assert_eq!(kb_step_with(9, false, false, 0, Some(CardBtn::Top), 3, &all, k), KbStep::To(0, None));
    }

    #[test]
    fn right_to_left_arrows_follow_the_mirrored_cards() {
        let k = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let all = |_: usize, _: CardBtn| true;
        let rtl = |vk, at| kb_step_with(vk, false, true, at, None, 3, &all, k);
        // Card 0 is on the right: Left goes to card 1 (visually left of it).
        assert_eq!(rtl(1, 0), KbStep::To(1, None));
        assert_eq!(rtl(3, 1), KbStep::To(0, None));
        // Check against the layout: the card Left reaches is left of it.
        let wd = card_width(1920, 1080);
        let p = page(&[wd; 3], 0, 2000.0, 2000.0, true);
        let KbStep::To(to, _) = rtl(1, 0) else { panic!() };
        assert!(p.rects[to].0 < p.rects[0].0);
        // Up / Down and Tab keep the logical order.
        assert_eq!(rtl(2, 1), KbStep::To(0, None));
        assert_eq!(rtl(4, 1), KbStep::To(2, None));
        assert_eq!(kb_step_with(10, false, true, 0, Some(CardBtn::MinRestore), 3, &all, k), KbStep::To(1, None));
    }

    #[test]
    fn a_page_change_keeps_the_choice_in_view() {
        assert_eq!(keep_on_page(0, 8, 12), 8, "first card scrolled away: the first on the page");
        assert_eq!(keep_on_page(11, 0, 8), 7, "last card scrolled away: the last on the page");
        assert_eq!(keep_on_page(9, 8, 12), 9, "still on the page");
        assert_eq!(keep_on_page(3, 0, 0), 3);
    }

    #[test]
    fn card_width_follows_the_window_shape_within_bounds() {
        let pad = CARD_PAD * 2.0;
        assert!((card_width(1920, 1080) - (THUMB_H * 16.0 / 9.0 + pad)).abs() < 0.01);
        // Tall windows: the narrowest card; very wide ones: the widest.
        assert_eq!(card_width(1080, 1920), THUMB_MIN_W + pad);
        assert_eq!(card_width(5120, 1080), THUMB_MAX_W + pad);
        // Unknown size: 16:9.
        assert_eq!(card_width(0, 0), card_width(1600, 900));
        assert_eq!(card_width(-5, 100), card_width(1600, 900));
    }

    #[test]
    fn cards_wrap_and_rows_center() {
        let wd = card_width(1920, 1080);
        let p = page(&[wd; 3], 0, 2000.0, 2000.0, false);
        assert_eq!(p.rects.len(), 3);
        assert_eq!(p.w, OUTER * 2.0 + 3.0 * wd + 2.0 * GAP);
        assert_eq!(p.h, OUTER * 2.0 + CARD_H);
        assert_eq!((p.more, p.footer), (0, None));
        // Only two fit per row: the third wraps, centered under them.
        let p2 = page(&[wd; 3], 0, OUTER * 2.0 + 2.0 * wd + GAP + 1.0, 2000.0, false);
        assert_eq!(p2.rects[2].1, p2.rects[0].1 + CARD_H + GAP);
        assert!((p2.rects[2].0 + wd / 2.0 - p2.w / 2.0).abs() < 0.01, "the short row is centered");
        assert!(p2.h > p.h);
        // Mixed widths keep their own widths.
        let narrow = card_width(1080, 1920);
        let p3 = page(&[wd, narrow], 0, 2000.0, 2000.0, false);
        assert_eq!(p3.rects[1].2, narrow);
        assert_eq!(p3.rects[1].0, p3.rects[0].0 + wd + GAP);
    }

    #[test]
    fn more_than_eight_windows_scroll_with_a_count() {
        let wd = card_width(1920, 1080);
        let p = page(&[wd; 12], 0, 4000.0, 4000.0, false);
        assert_eq!(p.rects.len(), MAX_CARDS);
        assert_eq!(p.more, 4);
        let f = p.footer.expect("a +N strip");
        assert_eq!(f.3, FOOTER_H);
        assert!(f.1 + f.3 <= p.h - OUTER + 0.01, "the strip is inside the panel");
        assert!(p.rects.iter().all(|r| r.1 + r.3 <= f.1 + 0.01), "the strip is under the cards");
        // Scrolled to the end: the last four, the eight before are "more".
        let p = page(&[wd; 12], 8, 4000.0, 4000.0, false);
        assert_eq!((p.first, p.rects.len(), p.more), (8, 4, 8));
        // A start past the end comes back inside.
        assert_eq!(page(&[wd; 3], 9, 4000.0, 4000.0, false).first, 2);
        // Too low for two rows: one row, the rest counted.
        let p = page(&[wd; 6], 0, OUTER * 2.0 + 3.0 * wd + 2.0 * GAP, OUTER * 2.0 + CARD_H + FOOTER_H + 10.0, false);
        assert_eq!((p.rects.len(), p.more), (3, 3));
        assert!(p.h <= OUTER * 2.0 + CARD_H + FOOTER_H + 10.0);
        // No windows: nothing, and no panic.
        let p = page(&[], 0, 1000.0, 1000.0, false);
        assert!(p.rects.is_empty() && p.more == 0);
    }

    #[test]
    fn scrolling_stays_inside_and_follows_the_keyboard() {
        assert_eq!(scroll(0, 8, 12, true), 1);
        assert_eq!(scroll(4, 8, 12, true), 4, "the last card is in view: no further");
        assert_eq!(scroll(0, 8, 12, false), 0);
        assert_eq!(scroll(3, 8, 12, false), 2);
        assert_eq!(scroll_to(0, 8, 9), 2, "card 9 becomes the last one shown");
        assert_eq!(scroll_to(4, 8, 1), 1);
        assert_eq!(scroll_to(2, 8, 5), 2, "already in view");
        assert_eq!(next_page(0, 8, 12), 8);
        assert_eq!(next_page(8, 4, 12), 0, "after the last page, the first");
    }

    #[test]
    fn right_to_left_mirrors_cards_and_buttons() {
        let wd = card_width(1920, 1080);
        let l = page(&[wd; 3], 0, 2000.0, 2000.0, false);
        let rr = page(&[wd; 3], 0, 2000.0, 2000.0, true);
        // The first card on the right, the last on the left.
        assert_eq!(rr.rects[0].0 + rr.rects[0].2, l.w - OUTER);
        assert_eq!(rr.rects[2].0, OUTER);
        for (a, b) in l.rects.iter().zip(&rr.rects) {
            assert!((a.0 + b.0 + a.2 - l.w).abs() < 0.01);
        }
        // × at the right end in LTR, the left end in RTL; the strip too.
        let card = (OUTER, OUTER, wd, CARD_H);
        let ltr = buttons(card, false);
        let rtl = buttons(card, true);
        assert!(ltr[0].1 .0 > card.0 + card.2 / 2.0);
        assert!(rtl[0].1 .0 < card.0 + card.2 / 2.0);
        for ((b1, a), (b2, b)) in ltr.iter().zip(&rtl) {
            assert_eq!(b1, b2);
            assert!((a.0 + a.2 / 2.0 + b.0 + b.2 / 2.0 - (2.0 * card.0 + card.2)).abs() < 0.01, "{b1:?} mirrored");
            assert_eq!(a.1, b.1);
        }
        // Strip order: always on top, next monitor, minimize, left to right.
        assert!(ltr[1].1 .0 < ltr[2].1 .0 && ltr[2].1 .0 < ltr[3].1 .0);
        assert!(rtl[1].1 .0 > rtl[2].1 .0 && rtl[2].1 .0 > rtl[3].1 .0);
        // Buttons stay inside the card.
        for (_, b) in ltr.iter().chain(&rtl) {
            assert!(b.0 >= card.0 && b.0 + b.2 <= card.0 + card.2 && b.1 >= card.1 && b.1 + b.3 <= card.1 + card.3);
        }
    }

    #[test]
    fn hits_find_cards_buttons_and_the_strip() {
        let wd = card_width(1920, 1080);
        let card = (OUTER, OUTER, wd, CARD_H);
        let cards = [(5usize, card)];
        let footer = Some((OUTER, OUTER + CARD_H, wd, FOOTER_H));
        let center = |r: Rect4| (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        for rtl in [false, true] {
            for (b, br) in buttons(card, rtl) {
                let (x, y) = center(br);
                assert_eq!(hit_in(&cards, footer, x, y, rtl), CardHit::Card(5, Some(b)), "{b:?} rtl={rtl}");
            }
        }
        // The thumbnail: the card itself.
        assert_eq!(hit_in(&cards, footer, OUTER + wd / 2.0, OUTER + TITLE_H + THUMB_H / 2.0, false), CardHit::Card(5, None));
        // × in RTL is on the left: its LTR place is plain card.
        let (x, y) = center(buttons(card, false)[0].1);
        assert_eq!(hit_in(&cards, footer, x, y, true), CardHit::Card(5, None));
        assert_eq!(hit_in(&cards, footer, OUTER + 3.0, OUTER + CARD_H + 5.0, false), CardHit::Footer);
        assert_eq!(hit_in(&cards, footer, 0.0, 0.0, false), CardHit::Nothing);
    }

    #[test]
    fn actions_by_window_state() {
        let normal = WinState::default();
        let hung = WinState { hung: true, ..normal };
        let min = WinState { minimized: true, ..normal };
        let top = WinState { topmost: true, ..normal };
        for b in BTNS {
            assert!(can(b, normal, 2), "{b:?}");
            // Not responding: only the posted close request.
            assert_eq!(can(b, hung, 2), b == CardBtn::Close, "{b:?} hung");
            assert_eq!(can(b, WinState { minimized: true, ..hung }, 2), b == CardBtn::Close);
        }
        // A window we may not change keeps its pin, unavailable.
        let locked = WinState { locked: true, ..normal };
        assert!(!can(CardBtn::Top, locked, 2) && can(CardBtn::Close, locked, 2) && can(CardBtn::MinRestore, locked, 2));
        // One monitor: nowhere to move to.
        assert!(!can(CardBtn::Move, normal, 1));
        assert!(can(CardBtn::Top, normal, 1) && can(CardBtn::MinRestore, normal, 1));
        // Minimized: restore (and the rest) still work.
        assert!(can(CardBtn::MinRestore, min, 1) && can(CardBtn::Top, min, 1) && can(CardBtn::Move, min, 3));
        // Glyphs follow the state.
        assert_ne!(CardBtn::MinRestore.glyph(min), CardBtn::MinRestore.glyph(normal));
        assert_ne!(CardBtn::Top.glyph(top), CardBtn::Top.glyph(normal));
        // Keys are distinct (UIA).
        for (i, a) in BTNS.iter().enumerate() {
            assert!(BTNS[i + 1..].iter().all(|b| b.key() != a.key()));
            assert_eq!(a.index(), i);
        }
    }

    #[test]
    fn thumbnails_keep_aspect() {
        let (x, y, w, h) = fit(1920, 1080, 200.0, 100.0);
        assert!((x - 11.111).abs() < 0.01 && y == 0.0 && (w - 177.777).abs() < 0.01 && h == 100.0);
        let (x, y, w, h) = fit(1000, 2000, 200.0, 100.0);
        assert_eq!((w, h), (50.0, 100.0));
        assert_eq!((x, y), (75.0, 0.0));
        assert_eq!(fit(0, 0, 10.0, 10.0), (0.0, 0.0, 10.0, 10.0));
    }

    #[test]
    fn peek_is_centered_and_never_enlarged() {
        let work = r(0, 0, 2000, 1000);
        // Large: three quarters of the work area, aspect kept, centered.
        let p = peek_rect(4000, 2000, work).unwrap();
        assert_eq!((p.right - p.left, p.bottom - p.top), (1500, 750));
        assert_eq!(((p.left + p.right) / 2, (p.top + p.bottom) / 2), (1000, 500));
        // Small: its real size.
        let p = peek_rect(400, 300, r(100, 50, 2100, 1050)).unwrap();
        assert_eq!((p.left, p.top, p.right - p.left, p.bottom - p.top), (900, 400, 400, 300));
        assert_eq!(peek_rect(0, 10, work), None);
        assert_eq!(peek_rect(10, 10, r(0, 0, 0, 0)), None);
    }

    #[test]
    fn open_animation_follows_the_settings() {
        assert_eq!(open_len(true, true, 120), 120);
        assert_eq!(open_len(true, true, 60), 60, "the effects speed is applied by the caller");
        assert_eq!(open_len(false, true, 120), 0, "Preview pop-in off: none");
        assert_eq!(open_len(true, false, 120), 0, "animations off: none");
        assert_eq!(open_len(true, true, 999_999), 2000, "bounded");
        // What open_ms() passes on: High pops, Medium opens at once (as
        // before the small animations merged), Low-spec mode too.
        use crate::effects::{pick_preset, resolve, Env};
        let mut c = crate::config::Config::default();
        let on = Env { animations: true, high_contrast: false };
        assert_eq!(open_len_of(&resolve(&c, on), true), POP_MS as u32);
        pick_preset(&mut c, "medium");
        assert_eq!(open_len_of(&resolve(&c, on), true), 0);
        pick_preset(&mut c, "high");
        c.set_low_spec(true);
        assert_eq!(open_len_of(&resolve(&c, on), true), 0);
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert_eq!(ease(7.0), 1.0);
        assert!(ease(0.5) > 0.5, "eases out");
    }
    #[test]
    fn preview_chrome_is_on_the_tokens() {
        for v in [TITLE_H, ACT_H, CARD_PAD, GAP, OUTER, CLOSE, BTN, THUMB_H, FOOTER_H, ICON] {
            assert_eq!(v % 4.0, 0.0, "{v} off the 4 DIP grid");
        }
        let t = crate::theme::Theme::graphite();
        let r = t.radii();
        assert_eq!(r.menu, 12.0, "the panel's corners");
        assert_eq!(crate::theme::concentric(r.menu, OUTER), r.control, "what sits OUTER inside it is concentric: 12 - 4 = 8");
        let src = include_str!("preview.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(!code.contains("a.max(18)") && !code.contains("text_fit(&label, &family, 11.0"), "no literal fills or sizes");
    }

    #[test]
    fn tooltips_are_the_shared_drawn_ones() {
        // Design B3 review P3 #4: no system tooltip; the top bar's drawn
        // one (radius tip, E1 with the Shadows effect, Footnote), wrapping
        // a long title instead of cutting it.
        let src = include_str!("preview.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        for gone in ["tooltips_class32", "TTM_ADDTOOLW", "TTTOOLINFOW"] {
            assert!(!code.contains(gone), "{gone}");
        }
        assert!(code.contains("crate::topbar::show_tip_wrapped(") && code.contains("crate::topbar::hide_tip()"));
        // A third of the work area, at least 200 px.
        assert_eq!(tip_max_px(RECT { left: 0, top: 0, right: 1920, bottom: 1040 }), 640.0);
        assert_eq!(tip_max_px(RECT { left: 0, top: 0, right: 480, bottom: 800 }), 200.0);
    }

    #[test]
    fn the_tooltip_under_the_pointer_waits_then_follows() {
        let tips = vec![(1, (10.0, 10.0, 100.0, 32.0), "A long title".to_string()), (10_004, (120.0, 160.0, 24.0, 24.0), "Close".to_string())];
        assert_eq!(tip_under(&tips, 50.0, 20.0), Some(1));
        assert_eq!(tip_under(&tips, 130.0, 170.0), Some(10_004));
        assert_eq!(tip_under(&tips, 5.0, 5.0), None);
        assert_eq!(tip_under(&tips, 110.0, 20.0), None, "the right edge is outside");
        // A first one waits; from one to the next while up: at once.
        assert_eq!(tip_step(None, false, Some(1)), TipStep::Wait(1));
        assert_eq!(tip_step(Some(1), false, Some(1)), TipStep::Keep, "the wait goes on");
        assert_eq!(tip_step(Some(1), true, Some(10_004)), TipStep::Show(10_004));
        assert_eq!(tip_step(Some(1), false, Some(10_004)), TipStep::Wait(10_004), "not up yet: waits for the new one");
        // Off them: hidden (the next waits again); nothing to nothing: kept.
        assert_eq!(tip_step(Some(1), true, None), TipStep::Hide);
        assert_eq!(tip_step(None, false, None), TipStep::Keep);
    }
}

/// The cards' plate and its shadow window read back for the design audit
/// (tests only).
#[cfg(test)]
#[path = "preview_harmony.rs"]
mod harmony;
