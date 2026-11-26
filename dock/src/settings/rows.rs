//! Rows: a label (and a grey description) on one side, the row's control
//! on the other. Used by the settings pages and the first-run flow, which
//! mirrors them right to left (ar, fa, he).
//!
//! The pages read like macOS System Settings: rows sit in rounded groups
//! ("inset cards", one tone step off the window), each group under an
//! optional heading; rows inside a group are told apart by a hairline
//! inset from the text, never by outlines. "More options" folds open
//! inside their group (their rows indented); "Advanced" is a group of its
//! own. Every page uses the same anatomy and spacing (tok), and the same
//! achromatic controls: a switch whose "on" is the high-contrast fill
//! (near-black on light, near-white on dark), a segmented choice whose
//! pick is a raised tile, a slider filled with the same "on" tone.
//!
//! How a page's rows fall into groups is pure (slots): tested.

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, ID2D1Layer, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ELLIPSE, D2D1_LAYER_OPTIONS1_NONE, D2D1_LAYER_PARAMETERS1, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD};
use windows_numerics::Vector2;

use super::layout::{Block, CtlKind, Kind};
use super::model::{self, Ctl, Row};
use super::{clip_to, col, App, Hit, FONT};
use crate::glyphs::Glyph;
use crate::i18n::t;

/// How long a row found by the search stays lit (it fades over the end).
pub(super) const FLASH: std::time::Duration = std::time::Duration::from_millis(1600);
/// Timer redrawing the lit row while it fades.
pub(super) const FLASH_TIMER: usize = 0x6B71;
/// A key field's Reset (inside its end; tok::FIELD_W is the field's).
pub(super) const RESET_W: f32 = 30.0;

/// Layout tokens shared by every page (DIP): the shared theme tokens
/// (design B3: theme::ramp for type, metrics::space for every gap) under
/// the names the pages use, on the 4 DIP grid. Type ramp (body 13): caption
/// 11, footnote 12, body 13, headline 15 semibold, title 20 semibold;
/// weights 400 and 600 only, never bold, never bolder for "picked" (the
/// fill says it). Corners come from the theme at run time (App::ctl_r,
/// card_r, menu_r, the tooltip's radii.tip): panel 16, menu 12, card / tile
/// 8, control 4, a switch a capsule (h / 2), an inner shape concentric
/// (outer - its inset).
pub(super) mod tok {
    use crate::theme::{metrics::space, ramp};
    /// The type ramp's sizes.
    pub const CAPTION: f32 = ramp::CAPTION.size;
    pub const FOOTNOTE: f32 = ramp::FOOTNOTE.size;
    pub const BODY: f32 = ramp::BODY.size;
    /// Card titles and the first-run flow's headings (headline 15).
    pub const HEADLINE: f32 = ramp::HEADLINE.size;
    /// The page's title over everything (title: 20/26 semibold). A page has
    /// no intro sentence on screen (a screen reader still hears it).
    pub const TITLE_SIZE: f32 = ramp::TITLE.size;
    /// The first-run flow's big title (large title 26).
    pub const LARGE_TITLE: f32 = ramp::LARGE_TITLE.size;
    /// A row: label (body) and secondary text (footnote) sizes, the padding
    /// inside its group (sideways, and above and below its text), its
    /// least height (labels only: a description is behind an "i").
    pub const LABEL_SIZE: f32 = BODY;
    pub const HINT_SIZE: f32 = FOOTNOTE;
    pub const ROW_PAD: f32 = space::S5;
    pub const ROW_VPAD: f32 = space::S3;
    pub const ROW_MIN: f32 = 40.0;
    /// Space under a card that something else follows (the display map,
    /// a question card).
    pub const ROW_GAP: f32 = space::S3;
    /// Between two groups, and a group label over a card (its height and
    /// text size: footnote semibold, dim).
    pub const GROUP_GAP: f32 = space::S6;
    pub const HEADER_H: f32 = space::S6;
    pub const HEADER_SIZE: f32 = FOOTNOTE;
    /// "Advanced" / "More options": the line's height, and how far the
    /// rows inside "More options" are indented once it is open.
    pub const FOLD_H: f32 = 40.0;
    pub const FOLD_INDENT: f32 = 20.0;
    /// The controls: their height (buttons, lists, the segmented choice),
    /// a segment's inset inside one (its corners concentric: the control's
    /// minus it), the switch's size (a capsule).
    pub const CONTROL_H: f32 = 28.0;
    /// A list button's and a key field's width (one token: the two read
    /// as the same kind of control).
    pub const FIELD_W: f32 = 180.0;
    pub const SEGMENT_INSET: f32 = space::S1;
    pub use crate::gfx::parts::{SWITCH_H, SWITCH_W};
    /// The "i" after a label whose description is worth reading (its
    /// circle's diameter, the gap before it, the room it takes).
    pub const INFO_D: f32 = 12.0;
    /// (Room for its focus ring between the label and the glyph.)
    pub const INFO_GAP: f32 = space::S3;
    pub const INFO_W: f32 = INFO_D + INFO_GAP;
    /// Its hit target (the glyph stays small; the target does not).
    pub const INFO_HIT: f32 = 24.0;
    /// The accent's colors: a dot's diameter and the gap between two.
    pub const SWATCH_D: f32 = 16.0;
    pub const SWATCH_GAP: f32 = space::S3;
    /// Light / dark as pictures: a tile's least and widest width, its
    /// height to width, the gap between two, the space over them and under
    /// their names.
    pub const TILE_MIN_W: f32 = 88.0;
    pub const TILE_MAX_W: f32 = 120.0;
    pub const TILE_RATIO: f32 = 0.625;
    pub const TILE_GAP: f32 = space::S5;
    pub const TILE_TOP: f32 = space::S5;
    pub const TILE_NAME: f32 = space::S5;
    pub const TILE_BOTTOM: f32 = space::S4;
    /// The keyboard's focus ring: how far outside its control, and how
    /// thick (one style everywhere; its corners the control's plus the gap).
    pub const RING_GAP: f32 = space::S1;
    pub const RING_W: f32 = 2.0;
    /// An "i"'s ring goes round its glyph (not its larger target), the
    /// same gap out as every control's.
    pub const INFO_RING_GAP: f32 = RING_GAP;
}

/// Appearance's light / dark row (drawn as pictures) and accent row (a row
/// of colors).
pub(super) const MODE_ROW: &str = "theme.mode";
pub(super) const ACCENT_ROW: &str = "theme.accent";

/// The width `n` accent dots take.
pub(super) fn swatches_w(n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    n as f32 * tok::SWATCH_D + (n - 1) as f32 * tok::SWATCH_GAP
}

/// The dots' centers, the last one's right edge at `cr` (unmirrored).
/// Pure: tested.
pub(super) fn swatch_centers(cr: f32, n: usize) -> Vec<f32> {
    let x0 = cr - swatches_w(n) + tok::SWATCH_D / 2.0;
    (0..n).map(|i| x0 + i as f32 * (tok::SWATCH_D + tok::SWATCH_GAP)).collect()
}

/// The keyboard stops at the dot picked (`cur`) and at "Custom…" (its
/// own action). Pure: tested.
pub(super) fn swatch_focusable(oi: usize, cur: usize) -> bool {
    oi == cur || oi == crate::config::ACCENT_CUSTOM
}

/// The dot an arrow (`fwd`: right / End) or Home / End (`end`) goes to
/// from `from`, among the colors of a row of `n` (the last, "Custom…",
/// left out: it opens a dialog). None: nowhere else. Pure: tested.
pub(super) fn swatch_step(from: usize, n: usize, fwd: bool, end: bool) -> Option<usize> {
    let last = n.checked_sub(2)?;
    let to = match (end, fwd) {
        (true, true) => last,
        (true, false) => 0,
        // Past the last preset ("Custom…" itself): nowhere further.
        (false, true) if from >= last => return None,
        (false, true) => from + 1,
        // From "Custom…" (a color of one's own): the last preset.
        (false, false) if from > last => last,
        (false, false) => from.saturating_sub(1),
    };
    (to != from).then_some(to)
}

/// An "i"'s hit around its center: larger than its circle.
pub(super) fn info_hit(cx: f32, cy: f32) -> D2D_RECT_F {
    let r = tok::INFO_HIT / 2.0;
    rect(cx - r, cy - r, cx + r, cy + r)
}

/// How far a note's first line starts over the bottom of its row's top
/// part (just under the control: the control's own space under it is
/// the gap).
pub(super) const NOTE_RISE: f32 = 4.0;
/// The room a note's fix link takes under its text.
pub(super) const NOTE_LINK_H: f32 = 24.0;

/// A note under a row whose top part (label and control) is `top_h` high
/// with its label's text `text_h` high: where the note starts (from the
/// row's top) and how much it adds to the row, for a note `note_h` high
/// (its text, and its link if any). Its last line ends as far from the
/// row's bottom as the label's first starts from the row's top, so the
/// row's padding is even. Every row's notes. Pure: tested.
pub(super) fn note_box(top_h: f32, text_h: f32, note_h: f32) -> (f32, f32) {
    let start = top_h - NOTE_RISE;
    let inset = ((top_h - text_h) / 2.0).max(tok::ROW_VPAD);
    (start, (start + note_h + inset - top_h).max(0.0))
}

/// How far a field's value keeps off its plain end (its end with no
/// chevron, Reset or ×).
pub(super) const FIELD_TEXT_PAD: f32 = 12.0;

/// Where a field's value `w` wide starts in `field` (as drawn: mirrored
/// already): centered on the field, so every key field and list button in
/// a column shares one center line whatever sits at its end, and only
/// moved off center to stay clear of the `end` DIP its chevron, Reset or
/// × takes at its trailing edge (`end_on_left`: right to left) and
/// FIELD_TEXT_PAD off the other. Returns (x, the room it has). Pure: tested.
pub(super) fn field_value_x(field: D2D_RECT_F, w: f32, end: f32, end_on_left: bool) -> (f32, f32) {
    let (lo, hi) = if end_on_left { (field.left + end, field.right - FIELD_TEXT_PAD) } else { (field.left + FIELD_TEXT_PAD, field.right - end) };
    let x = ((field.left + field.right) / 2.0 - w / 2.0).min(hi - w).max(lo);
    (x, hi - lo)
}

/// The glyph's box of the "i" whose hit is `hit` (same center). Pure.
pub(super) fn info_glyph(hit: D2D_RECT_F) -> D2D_RECT_F {
    let (cx, cy, r) = ((hit.left + hit.right) / 2.0, (hit.top + hit.bottom) / 2.0, tok::INFO_D / 2.0);
    rect(cx - r, cy - r, cx + r, cy + r)
}

/// The icon font with Windows' Info glyph (U+E946): Segoe Fluent Icons
/// (Windows 11), else Segoe MDL2 Assets (Windows 10); None: neither.
fn info_font(g: &crate::gfx::Gfx) -> Option<&'static str> {
    static FONT: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *FONT.get_or_init(|| ["Segoe Fluent Icons", "Segoe MDL2 Assets"].into_iter().find(|f| g.has_font(f)))
}

/// Whether a row from `top` to `bottom` shows in `clip` at all.
fn visible_row(top: f32, bottom: f32, clip: D2D_RECT_F) -> bool {
    bottom > clip.top && top < clip.bottom
}

/// Light / dark as pictures in a card from `left` to `right`: each of the
/// `n` tiles (from the row's top, unmirrored: the caller mirrors them),
/// as wide as the longest name `name_w` asks (within TILE_MIN_W..TILE_MAX_W,
/// narrower when the row is), and the row's height. Pure: tested.
pub(super) fn mode_tiles(left: f32, right: f32, n: usize, name_w: f32) -> (Vec<D2D_RECT_F>, f32) {
    if n == 0 {
        return (Vec::new(), tok::ROW_MIN);
    }
    let room = right - left - tok::ROW_PAD * 2.0;
    let fit = (room - tok::TILE_GAP * (n - 1) as f32) / n as f32;
    let w = (name_w + 8.0).clamp(tok::TILE_MIN_W, tok::TILE_MAX_W).min(fit).max(40.0).floor();
    let h = (w * tok::TILE_RATIO).round();
    let tiles = (0..n)
        .map(|i| {
            let x = left + tok::ROW_PAD + i as f32 * (w + tok::TILE_GAP);
            rect(x, tok::TILE_TOP, x + w, tok::TILE_TOP + h)
        })
        .collect();
    (tiles, tok::TILE_TOP + h + 6.0 + tok::TILE_NAME + tok::TILE_BOTTOM)
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// A slider's track width in a row `w` wide: the label and its line keep
/// SLIDER_TEXT_MIN at least (the track gives way, down to 140).
pub(super) fn slider_track(w: f32) -> f32 {
    (w - tok::ROW_PAD * 2.0 - SLIDER_VALUE_W - 8.0 - 12.0 - SLIDER_TEXT_MIN).clamp(140.0, 220.0)
}
const SLIDER_VALUE_W: f32 = 52.0;
/// Between a label and the grey line under it (model::subtitled).
const SUB_GAP: f32 = 2.0;
const SLIDER_TEXT_MIN: f32 = 220.0;

/// Whether a choice of options this wide (all of them side by side) is
/// drawn as a list button instead, in a row `w` wide: past 60 % of it
/// the label would be squeezed.
pub(super) fn choice_compact(options_w: f32, w: f32) -> bool {
    options_w > w * 0.6
}

/// A segmented choice's part widths for its options' text widths: all as
/// wide as the widest one's text and its padding (the Mac's even parts:
/// uneven ones read as a ragged row of buttons). Pure: tested.
pub(super) fn segment_widths(text_ws: impl Iterator<Item = f32>) -> Vec<f32> {
    const PAD: f32 = 26.0;
    let ws: Vec<f32> = text_ws.collect();
    let widest = ws.iter().copied().fold(0.0f32, f32::max);
    vec![(widest + PAD).ceil(); ws.len()]
}

/// What a row is, for grouping a page.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum RowKind {
    /// A heading over the group that follows.
    Header,
    /// "Advanced" (its own group: `own_group`) or "More options" (inside
    /// the group it follows) over the `n` rows after it.
    Fold { n: usize, own_group: bool },
    /// A setting; `break_after`: the group ends after it (something is
    /// drawn under it outside the group).
    Setting { break_after: bool },
}

/// What row `r` is for grouping. Pure: tested.
pub(super) fn row_kind(r: &Row) -> RowKind {
    match r.ctl {
        Ctl::Header => RowKind::Header,
        Ctl::Fold { n } => RowKind::Fold { n, own_group: r.label == "settings.fold.advanced" },
        // Light / dark (its pictures) is a card of its own; so is "Same as
        // Dock 1" (its question card follows it).
        // General's first card ends at the tray icon (the taskbar's card
        // follows without a heading); the widgets' at the last widget
        // (screen sharing, every widget's, a card of its own under it).
        _ => RowKind::Setting { break_after: r.key == model::SAME_AS_MAIN || r.key == MODE_ROW || r.key == "dock.displays" || r.key == "tray_icon" || r.key == "widgets.pieces" },
    }
}

/// Where a row is drawn.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Slot {
    Hidden,
    Header,
    /// A row of a group: the first one has the group's top corners (and
    /// no hairline over it), the last its bottom corners; `indent`: inside
    /// an open "More options".
    Row { first: bool, last: bool, indent: bool },
}

/// The rows' slots from what they are and which are shown. Pure: tested.
pub(super) fn slots(kinds: &[RowKind], visible: &[bool]) -> Vec<Slot> {
    let n = kinds.len();
    let mut indent = vec![false; n];
    for (fi, k) in kinds.iter().enumerate() {
        if let RowKind::Fold { n: k, own_group: false } = *k {
            for v in indent.iter_mut().take((fi + k + 1).min(n)).skip(fi + 1) {
                *v = true;
            }
        }
    }
    let mut out = vec![Slot::Hidden; n];
    let mut prev: Option<usize> = None;
    for i in 0..n {
        if !visible.get(i).copied().unwrap_or(false) {
            continue;
        }
        if kinds[i] == RowKind::Header {
            out[i] = Slot::Header;
            prev = Some(i);
            continue;
        }
        let first = match prev {
            None => true,
            Some(p) => kinds[p] == RowKind::Header || matches!(kinds[p], RowKind::Setting { break_after: true }) || matches!(kinds[i], RowKind::Fold { own_group: true, .. }),
        };
        if !first {
            if let Some(Slot::Row { last, .. }) = prev.map(|p| &mut out[p]) {
                *last = false;
            }
        }
        out[i] = Slot::Row { first, last: true, indent: indent[i] };
        prev = Some(i);
    }
    out
}

/// A switch's knob between its ends: `p` of the way (0 off, 1 on; the
/// spring may overshoot a little, kept inside the track). Pure: tested.
pub(super) fn knob_x(left: f32, right: f32, radius: f32, inset: f32, p: f32) -> f32 {
    let (a, b) = (left + inset + radius, right - inset - radius);
    a + (b - a) * p.clamp(-0.06, 1.06).clamp(0.0, 1.0)
}

/// Mirrors x positions between `left` and `right` when `on`.
#[derive(Clone, Copy)]
pub(super) struct Mirror {
    pub on: bool,
    pub left: f32,
    pub right: f32,
}

impl Mirror {
    pub fn x(&self, x: f32) -> f32 {
        crate::i18n::mirror::x(self.on, x, self.left, self.right)
    }

    /// Left edge of the box `x..x + w`, mirrored.
    pub fn span(&self, x: f32, w: f32) -> f32 {
        crate::i18n::mirror::span(self.on, x, w, self.left, self.right)
    }

    pub fn r(&self, r: D2D_RECT_F) -> D2D_RECT_F {
        let (l, rt) = crate::i18n::mirror::edges(self.on, r.left, r.right, self.left, self.right);
        rect(l, r.top, rt, r.bottom)
    }
}

/// `a` toward `b` by `p` (0..1), channel by channel.
pub(super) fn mix(a: D2D1_COLOR_F, b: D2D1_COLOR_F, p: f32) -> D2D1_COLOR_F {
    let p = p.clamp(0.0, 1.0);
    D2D1_COLOR_F { r: a.r + (b.r - a.r) * p, g: a.g + (b.g - a.g) * p, b: a.b + (b.b - a.b) * p, a: a.a + (b.a - a.a) * p }
}

impl App {
    /// Shared settings menu face, including the reserved chevron column.
    pub(super) unsafe fn menu_value(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, value: &str, state: crate::theme::State, rtl: bool) -> Result<()> {
        let cy = (r.top + r.bottom) / 2.;
        self.fill_round(dc, self.ctl_fill(state), r, self.ctl_r())?;
        let (_, room) = field_value_x(r, 0., RESET_W, rtl);
        let shown = self.ellipsized(value, tok::BODY, false, room);
        let tw = self.text_w(&shown, tok::BODY, false);
        let (tx, _) = field_value_x(r, tw, RESET_W, rtl);
        self.text_box(dc, &shown, if rtl { tx - 1. } else { tx }, self.text_top(cy, tok::BODY), tok::BODY, false, tw + 1., self.ink(), rtl)?;
        self.chevron(dc, if rtl { r.left + 16. } else { r.right - 16. }, cy, self.dim())?;
        Ok(())
    }
    pub(super) fn text_w(&self, s: &str, size: f32, bold: bool) -> f32 {
        let w = if bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        self.gfx.text_layout(s, FONT, size, w).map(|l| l.1).unwrap_or(0.0)
    }

    /// One line of text in the box `x`..`x + maxw` from `y`, cut to it:
    /// from the left, or (right to left) ending at its right edge.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn text_box(&self, dc: &ID2D1DeviceContext, s: &str, x: f32, y: f32, size: f32, bold: bool, maxw: f32, c: D2D1_COLOR_F, rtl: bool) -> Result<()> {
        if maxw <= 4.0 {
            return Ok(());
        }
        let tw = self.text_w(s, size, bold);
        let tx = if rtl { x + maxw - tw } else { x };
        self.note_text(s, rect(tx.max(x), y, (tx + tw).min(x + maxw), y + size * 1.25));
        // Wider than its box: cut at the edge (layout::check flags it).
        if tw > maxw + 0.5 {
            self.note(super::layout::Kind::Cut, s, rect(x, y, x + maxw, y + size * 1.25));
        }
        dc.PushAxisAlignedClip(&rect(x, y - 4.0, x + maxw, y + size * 2.0), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        #[cfg(test)]
        self.quiet_log.set(true);
        let r = self.label(dc, s, tx, y, size, bold, c);
        #[cfg(test)]
        self.quiet_log.set(false);
        dc.PopAxisAlignedClip();
        r.map(|_| ())
    }

    /// The height of a row's label wrapped to `maxw`.
    fn row_label_h(&self, row: &Row, maxw: f32) -> f32 {
        self.para_h(&self.row_label(row), tok::LABEL_SIZE, maxw.max(40.0))
    }

    /// Whether fold row `fi` of `rows` shows its rows: opened, or one of
    /// them has a note (a setting that does not work as chosen is never
    /// hidden), or the search shows one of them.
    pub(super) fn fold_open(&self, rows: &[Row], fi: usize) -> bool {
        let Some(r) = rows.get(fi) else { return false };
        let Ctl::Fold { n } = r.ctl else { return true };
        if self.folds.contains(&r.key) || rows.iter().skip(fi + 1).take(n).any(|x| self.row_note(x.key).is_some()) {
            return true;
        }
        self.filter_now().is_some_and(|f| (fi + 1..=fi + n).any(|i| f.get(i).copied().unwrap_or(false)))
    }

    /// Opens the fold holding row `ri` of the rows shown (a search result,
    /// a link to it).
    pub(super) fn open_fold_of(&mut self, ri: usize) {
        if let Some(key) = model::fold_of(self.rows(), ri).and_then(|fi| self.rows().get(fi)).map(|r| r.key) {
            if !self.folds.contains(&key) {
                self.folds.push(key);
            }
        }
    }

    /// The page's rows as drawn now: each row's slot (hidden, a heading,
    /// a row of a group), from what is shown (the search's filter, closed
    /// folds, rows that do not apply now).
    pub(super) fn row_slots(&self, rows: &[Row]) -> Vec<Slot> {
        let filter = self.filter_now();
        let kinds: Vec<RowKind> = rows.iter().map(row_kind).collect();
        let mut visible = vec![true; rows.len()];
        let mut hidden_to = 0;
        for (ri, r) in rows.iter().enumerate() {
            let shown = ri >= hidden_to && !self.row_hidden(r.key) && filter.as_ref().is_none_or(|f| f.get(ri).copied().unwrap_or(true));
            visible[ri] = shown;
            if let Ctl::Fold { n } = r.ctl {
                if !shown || !self.fold_open(rows, ri) {
                    hidden_to = hidden_to.max(ri + 1 + n);
                }
            }
        }
        let mut out = slots(&kinds, &visible);
        // A row under the switch above it: indented like a fold's.
        for (s, r) in out.iter_mut().zip(rows.iter()) {
            if let Slot::Row { indent, .. } = s {
                *indent |= model::is_sub_row(r.key);
            }
        }
        out
    }

    /// A rounded group's part: the whole card, or a row of it with the
    /// group's top and / or bottom corners.
    pub(super) unsafe fn fill_segment(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32, top: bool, bottom: bool) -> Result<()> {
        if c.a <= 0.0 {
            return Ok(());
        }
        let h = r.bottom - r.top;
        let rad = rad.min(h / 2.0).max(0.0);
        match (top, bottom) {
            (true, true) => self.fill_round(dc, c, r, rad),
            (false, false) => self.fill_round(dc, c, r, 0.0),
            // One rounded end: a rounded rectangle under a square one
            // covering the other end (same color, drawn once: a layer
            // would cost more than the two fills overlap in alpha).
            (top, _) => {
                let b = dc.CreateSolidColorBrush(&c, None)?;
                let geo = rounded_one_end(dc, r, rad, top)?;
                dc.FillGeometry(&geo, &b, None);
                Ok(())
            }
        }
    }

    /// A switch whose right end is `cr`, centered on `cy` (mirrored by
    /// `m`): achromatic, "on" told by contrast (the theme's active tone,
    /// near-black on light, near-white on dark), the knob springing across
    /// when it changes (`anim`: the key its motion is kept under).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn toggle_switch(&self, dc: &ID2D1DeviceContext, m: Mirror, cr: f32, cy: f32, on: bool, hot: bool, anim: &str) -> Result<()> {
        self.toggle_switch_tone(dc,m,cr,cy,on,hot,anim,self.on_tone())
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn toggle_switch_neutral(&self, dc: &ID2D1DeviceContext, m: Mirror, cr: f32, cy: f32, on: bool, hot: bool, anim: &str) -> Result<()> {
        self.toggle_switch_tone(dc,m,cr,cy,on,hot,anim,super::rgba(self.look.active()))
    }
    #[allow(clippy::too_many_arguments)]
    unsafe fn toggle_switch_tone(&self, dc: &ID2D1DeviceContext, m: Mirror, cr: f32, cy: f32, on: bool, hot: bool, anim: &str, tone:D2D1_COLOR_F) -> Result<()> {
        let (w, h) = (tok::SWITCH_W, tok::SWITCH_H);
        let r = rect(cr - w, cy - h / 2.0, cr, cy + h / 2.0);
        let mr = m.r(r);
        // How far toward `on` the knob is (1: there).
        let p = self.switch_progress(anim);
        let on_p = if on { p } else { 1.0 - p };
        let rad = h / 2.0 - 3.0;
        // (Glass's knob is a capsule: kept inside by its half width.)
        let hw = if self.solid || self.hc.is_some() { rad } else { rad * crate::gfx::parts::KNOB_GLASS_W };
        // Right to left the knob's "on" end is the left one.
        let at = knob_x(mr.left, mr.right, hw, 3.0, if m.on { 1.0 - on_p } else { on_p });
        if let Some(hc) = self.hc {
            // The system's colors: the track outlined in text, filled with
            // Highlight when on; the knob in their opposite.
            let (track, knob) = if on { (super::rgba(hc.highlight), super::rgba(hc.highlight_text)) } else { (super::rgba(hc.window), super::rgba(hc.text)) };
            self.fill_round(dc, track, mr, h / 2.0)?;
            let b = dc.CreateSolidColorBrush(&super::rgba(hc.text), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(mr.left + 0.5, mr.top + 0.5, mr.right - 0.5, mr.bottom - 0.5), radiusX: h / 2.0 - 0.5, radiusY: h / 2.0 - 0.5 }, &b, 1.0, None);
            let kb = dc.CreateSolidColorBrush(&knob, None)?;
            dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: at, Y: cy }, radiusX: rad, radiusY: rad }, &kb);
            return Ok(());
        }
        // Off: the track fill (a step more under the pointer).
        let off_track = self.track_fill_at(hot);
        let track = mix(off_track, tone, on_p);
        self.fill_round(dc, track, mr, h / 2.0)?;
        // The knob: white on light (on the near-black track too), and on
        // dark a light grey that turns to the dark ink on the near-white
        // track; a soft shadow under it instead of an outline.
        let off_knob = super::rgba(self.look.knob());
        let on_knob = if self.light { off_knob } else { self.on_ink() };
        let knob = mix(off_knob, on_knob, on_p);
        self.knob(dc, at, cy, rad, knob)
    }

    /// A knob (switches, sliders), `rad` half its height: Glass's a
    /// capsule at elevation E1 (a real soft shadow, gfx::soft_shadows);
    /// Flat's a flat disc with no shadow, its contact ring round it (a
    /// white one on a white card keeps its edge); in high contrast the
    /// disc alone.
    pub(super) unsafe fn knob(&self, dc: &ID2D1DeviceContext, x: f32, cy: f32, rad: f32, c: D2D1_COLOR_F) -> Result<()> {
        let glass = !self.solid && self.hc.is_none();
        let hw = if glass { rad * crate::gfx::parts::KNOB_GLASS_W } else { rad };
        let body = rect(x - hw, cy - rad, x + hw, cy + rad);
        if glass {
            crate::gfx::soft_shadows(dc, body, rad, &self.look.knob_shadow())?;
        } else if self.hc.is_none() {
            let ring = self.look.contact();
            if ring.a > 0 {
                let w = crate::theme::CONTACT_W;
                let b = dc.CreateSolidColorBrush(&super::rgba(ring), None)?;
                dc.DrawEllipse(&D2D1_ELLIPSE { point: Vector2 { X: x, Y: cy }, radiusX: rad + w / 2.0, radiusY: rad + w / 2.0 }, &b, w, None);
            }
        }
        if glass {
            return self.fill_round(dc, c, body, rad);
        }
        let b = dc.CreateSolidColorBrush(&c, None)?;
        dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: x, Y: cy }, radiusX: rad, radiusY: rad }, &b);
        Ok(())
    }

    /// One of NopBar's line glyphs (`crate::glyphs`), `size` an em,
    /// centered on (`cx`, `cy`).
    pub(super) unsafe fn glyph(&self, dc: &ID2D1DeviceContext, g: Glyph, size: f32, cx: f32, cy: f32, c: D2D1_COLOR_F) -> Result<()> {
        crate::glyphs::draw(dc, g, cx, cy, size, c)
    }

    /// The "i" after a label, centered on (`cx`, `cy`) (mirrored
    /// already): Windows' own Info glyph (Segoe Fluent Icons, Segoe MDL2
    /// Assets on Windows 10; NopBar's drawn one without either), small and
    /// in the tertiary text color, the secondary while its tip is wanted.
    /// Its hit (INFO_HIT, cut to `clip`) shows the tip (`id`: a row, or a
    /// card's CARD_TIP + i).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn paint_info(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, id: usize, cx: f32, cy: f32, clip: D2D_RECT_F) -> Result<()> {
        let lit = self.hover == Some(Hit::Info(id)) || (self.keyboard && self.focus == Some(Hit::Info(id))) || self.tip.is_some_and(|t| t.ri == id && t.hide.is_none());
        let c = if lit { self.dim() } else { self.faint() };
        match info_font(&self.gfx) {
            Some(family) => self.icon_glyph(dc, family, "\u{E946}", tok::INFO_D, cx, cy, c)?,
            None => self.glyph(dc, Glyph::Info, tok::INFO_D, cx, cy, c)?,
        }
        let hit = info_hit(cx, cy);
        // Keyed by its hit: its focus ring (keyed the same) is checked
        // against it (layout::check).
        self.note(Kind::Info, &format!("{:?}", Hit::Info(id)), hit);
        if let Some(h) = clip_to(hit, clip) {
            hits.push((h, Hit::Info(id)));
        }
        Ok(())
    }

    /// One glyph `g` of icon font `family` at `size`, its ink centered on
    /// (`cx`, `cy`).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn icon_glyph(&self, dc: &ID2D1DeviceContext, family: &str, g: &str, size: f32, cx: f32, cy: f32, c: D2D1_COLOR_F) -> Result<()> {
        let (layout, _, _) = self.gfx.text_layout(g, family, size, DWRITE_FONT_WEIGHT_NORMAL)?;
        let o = layout.GetOverhangMetrics()?;
        // The ink, in the layout's box (its max width and height): from
        // (-left, -top) to (max_w + right, max_h + bottom).
        let (mw, mh) = (layout.GetMaxWidth(), layout.GetMaxHeight());
        let ink_cx = (-o.left + mw + o.right) / 2.0;
        let ink_cy = (-o.top + mh + o.bottom) / 2.0;
        let b = dc.CreateSolidColorBrush(&c, None)?;
        // Not snapped to the pixel grid: its ink stays on (cx, cy), where
        // its focus ring (unsnapped) is centered too.
        dc.DrawTextLayout(Vector2 { X: cx - ink_cx, Y: cy - ink_cy }, &layout, &b, windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NO_SNAP);
        Ok(())
    }

    /// Logs what was drawn where (layout.rs; only under a test).
    pub(super) fn note(&self, kind: super::layout::Kind, key: &str, r: D2D_RECT_F) {
        #[cfg(not(test))]
        let _ = (kind, key, r);
        #[cfg(test)]
        if let Some(v) = self.log.borrow_mut().as_mut() {
            v.push(super::layout::Item { kind, key: key.to_string(), r });
        }
    }

    /// Logs a text drawn (without logging it twice: a label inside a box).
    pub(super) fn note_text(&self, s: &str, r: D2D_RECT_F) {
        #[cfg(not(test))]
        let _ = (s, r);
        #[cfg(test)]
        if self.log.borrow().is_some() && !self.quiet_log.get() {
            self.note(super::layout::Kind::Text, s, r);
        }
    }

    /// Where the next block starts after `y` (`b`: a heading or a card),
    /// the space before it the one rule's (layout::gap_before).
    pub(super) fn block_top(&self, y: f32, b: super::layout::Block) -> f32 {
        let g = super::layout::gap_before(self.flow.get(), b);
        self.flow.set(b);
        y + g
    }

    /// A note's color: the text color (no hue: a note is told by being
    /// there, under its setting).
    pub(super) fn warn(&self) -> D2D1_COLOR_F {
        if let Some(h) = self.hc {
            return super::rgba(h.text);
        }
        self.ink()
    }

    /// How lit row `ri` of the page shown is (search result), 0..1.
    fn flash_of(&self, ri: usize) -> f32 {
        let Some((s, r, t0)) = self.flash else { return 0.0 };
        if s != self.section || r != ri || self.welcome.is_some() {
            return 0.0;
        }
        let e = t0.elapsed().as_secs_f32() / FLASH.as_secs_f32();
        if e >= 1.0 {
            0.0
        } else if !self.motion || e < 0.55 {
            1.0
        } else {
            1.0 - (e - 0.55) / 0.45
        }
    }

    /// The rows from `y` down, in their groups; returns where they end.
    /// Records each row's top and bottom (for scrolling one into view) and
    /// where the open list's button is (`anchor`).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn paint_rows(
        &self,
        dc: &ID2D1DeviceContext,
        hits: &mut Vec<(D2D_RECT_F, Hit)>,
        rows: &[Row],
        mut y: f32,
        left: f32,
        right: f32,
        clip: D2D_RECT_F,
        rtl: bool,
        anchor: &mut Option<D2D_RECT_F>,
    ) -> Result<f32> {
        let measure = |s: &str, size: f32| -> f32 { self.text_w(s, size, false) };
        let slots = self.row_slots(rows);
        let filtering = self.filter_now().is_some();
        let mut tops = self.row_tops.borrow_mut();
        tops.clear();
        self.howto_rows.borrow_mut().clear();
        self.compact.borrow_mut().clear();
        let dock_page = self.welcome.is_none() && self.sections.get(self.section).is_some_and(|s| s.kind == model::Kind::Dock);
        let windows_page = self.welcome.is_none() && self.sections.get(self.section).is_some_and(|s| s.kind == model::Kind::Windows);
        let m = Mirror { on: rtl, left, right };
        let card_r = self.card_r();
        // Labels only, on the pages and in the first-run flow alike: a
        // description is behind the "i" after its label.
        let pages = true;
        // One line of a label (where an "i" after it is centered).
        let line_h = self.para_h("Ag", tok::LABEL_SIZE, 400.0);
        for (ri, row) in rows.iter().enumerate() {
            // The Dock page's map of the displays: after the displays
            // choice, over the picked dock's own rows (not while searching).
            if dock_page && row.key == model::THIS_DOCK && !filtering {
                y = self.paint_display_map(dc, hits, y, left, right, clip)?;
            }
            // Window management's drawing of the screen: under the hot
            // corners' heading, over their rows (not while searching).
            if windows_page && row.key == "hot_corners.top_left" && !filtering {
                y = self.paint_corners(dc, y, left, right)?;
            }
            let fill = |c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32| self.fill_round(dc, c, m.r(r), rad);
            match slots[ri] {
                Slot::Hidden => {
                    tops.push((y, y));
                    continue;
                }
                // A heading over the group that follows: text only.
                Slot::Header => {
                    y = self.block_top(y, Block::Header);
                    self.note(Kind::Header, row.key, m.r(rect(left, y, right, y + tok::HEADER_H)));
                    // In line with the rows' labels; its description
                    // behind an "i". The light / dark pictures have no
                    // label of their own: theirs is on the heading over
                    // them (in reading order before the pictures).
                    let tip = if model::info_hint(row) { Some(ri) } else { rows.get(ri + 1).filter(|n| n.key == MODE_ROW && model::info_hint(n) && !matches!(slots.get(ri + 1), Some(Slot::Hidden))).map(|_| ri + 1) };
                    let info = tip.is_some();
                    let lx = left + tok::ROW_PAD;
                    // The widgets' load: a word at the heading's end, in line
                    // with the rows' controls (settings::widgets).
                    let status = if row.key == model::WIDGETS_SUMMARY { self.widgets_status() } else { None };
                    let status_w = status.map(|s| self.text_w(s, tok::HEADER_SIZE, false) + 12.0).unwrap_or(0.0);
                    let room = right - lx - tok::ROW_PAD - if info { tok::INFO_W } else { 0.0 } - status_w;
                    // The picked dock's heading stands out (its name, in the
                    // text color): what follows is that dock's alone.
                    let own = row.key == model::THIS_DOCK;
                    let size = if own { tok::LABEL_SIZE } else { tok::HEADER_SIZE };
                    let hy = y + tok::HEADER_H - size - 10.0;
                    let label = self.row_label(row);
                    self.text_box(dc, &label, m.span(lx, room), hy, size, true, room, if own { self.ink() } else { self.dim() }, rtl)?;
                    let h = tok::HEADER_H;
                    if let Some(id) = tip {
                        let cx = lx + self.text_w(&label, size, true).min(room) + tok::INFO_GAP + tok::INFO_D / 2.0;
                        self.paint_info(dc, hits, id, m.x(cx), hy + size * 0.67, clip)?;
                    }
                    if let Some(s) = status {
                        let sw = status_w - 12.0;
                        self.text_box(dc, s, m.span(right - tok::ROW_PAD - sw, sw), hy, tok::HEADER_SIZE, false, sw + 0.5, self.dim(), rtl)?;
                    }
                    tops.push((y, y + h));
                    y += h;
                    continue;
                }
                Slot::Row { first, .. } if first => y = self.block_top(y, Block::Card),
                Slot::Row { .. } => {}
            }
            let Slot::Row { first, last, indent } = slots[ri] else { continue };
            let pad = tok::ROW_PAD + if indent { tok::FOLD_INDENT } else { 0.0 };
            // Light / dark as pictures (Appearance): a card of its own.
            if pages && row.key == MODE_ROW {
                if let Ctl::Choice { opts, get, .. } = &row.ctl {
                    let h = self.paint_mode_tiles(dc, hits, ri, opts, get(&self.cfg), y, m, clip, (first, last))?;
                    self.note(Kind::Seg { first, last }, row.key, m.r(rect(left, y, right, y + h)));
                    tops.push((y, y + h));
                    y += h;
                    continue;
                }
            }
            // "Advanced" / "More options": a row with a chevron; its rows
            // follow (in the group) once it is open.
            if let Ctl::Fold { .. } = row.ctl {
                let open = self.fold_open(rows, ri);
                let r = rect(left, y, right, y + tok::FOLD_H);
                self.card_segment(dc, r, card_r, first, last)?;
                if !first {
                    self.hairline(dc, m, left + pad, right - tok::ROW_PAD, y)?;
                }
                if self.hover == Some(Hit::Fold(ri)) {
                    self.fill_segment(dc, self.row_fill(self.state_of(Hit::Fold(ri))), r, card_r, first, last)?;
                }
                // Closed: the chevron points along the reading direction.
                let g = if open { Glyph::ChevronDown } else if rtl { Glyph::ChevronLeft } else { Glyph::ChevronRight };
                self.glyph(dc, g, 10.0, m.x(left + pad + 5.0), y + tok::FOLD_H / 2.0, self.dim())?;
                let room = right - left - pad - 22.0 - tok::ROW_PAD;
                self.text_box(dc, t(row.label), m.span(left + pad + 18.0, room), self.text_top(y + tok::FOLD_H / 2.0, tok::LABEL_SIZE), tok::LABEL_SIZE, false, room, self.ink(), rtl)?;
                if r.bottom > clip.top && r.top < clip.bottom {
                    if let Some(c) = clip_to(m.r(r), clip) {
                        hits.push((c, Hit::Fold(ri)));
                    }
                }
                self.note(Kind::Seg { first, last }, row.key, m.r(r));
                tops.push((y, y + tok::FOLD_H));
                y += tok::FOLD_H;
                continue;
            }
            // Where the control starts: the label stops before it.
            let cr = right - tok::ROW_PAD;
            let track_w = slider_track(right - left);
            // A choice too wide for the row is a list button.
            let compact = match &row.ctl {
                Ctl::Choice { opts, .. } => choice_compact(segment_widths(opts.iter().map(|o| measure(model::opt_text(o), tok::BODY))).iter().sum::<f32>(), right - left),
                _ => false,
            };
            // The accent as a row of colors (Appearance), when they fit;
            // else its list button.
            let swatches = match &row.ctl {
                Ctl::Menu { opts, .. } if pages && row.key == ACCENT_ROW => !choice_compact(swatches_w(opts.len()), right - left),
                _ => false,
            };
            if compact {
                self.compact.borrow_mut().push(ri);
            }
            let ctl_left = match &row.ctl {
                Ctl::Toggle { .. } => cr - tok::SWITCH_W - 8.0,
                Ctl::Slider { .. } => cr - SLIDER_VALUE_W - 8.0 - track_w - 12.0,
                Ctl::Choice { .. } if compact => cr - tok::FIELD_W,
                Ctl::Choice { opts, .. } => cr - segment_widths(opts.iter().map(|o| measure(model::opt_text(o), tok::BODY))).iter().sum::<f32>() - 2.0 * tok::SEGMENT_INSET,
                Ctl::Menu { opts, .. } if swatches => cr - swatches_w(opts.len()) - 4.0,
                Ctl::Menu { .. } => cr - tok::FIELD_W,
                Ctl::Button { text: label } => cr - measure(t(label), tok::BODY) - 32.0,
                Ctl::Navigate { value, .. } => cr - measure(value(&self.cfg), tok::BODY) - 28.0,
                Ctl::Shortcut { .. } => cr - tok::FIELD_W,
                Ctl::Header | Ctl::Fold { .. } => cr,
            };
            let new_note_w = if row.key == "widgets.notes" && self.cfg.widgets.notes { measure(t("notes.new"), tok::BODY) + 32.0 } else { 0.0 };
            let ctl_left = ctl_left - if new_note_w > 0.0 { new_note_w + 8.0 } else { 0.0 };
            let maxw = ctl_left - tok::ROW_PAD - (left + pad);
            // The description: behind an "i" after the label when it says
            // more than the label (else only for a screen reader).
            let info = model::info_hint(row);
            let label_w = (maxw - if info { tok::INFO_W } else { 0.0 }).max(40.0);
            // Long labels wrap (translations): the row grows to hold them.
            let label_h = self.row_label_h(row, label_w);
            // What a label alone does not say: a grey line under it (the
            // row grows to hold it wrapped; the control stays centered on
            // the label and that line together).
            let sub = if !super::agent_notifications::installed(row.key) {
                Some(std::borrow::Cow::Borrowed(t("settings.agents.missing")))
            } else { model::subtitled(row).then(|| self.row_hint(row)).filter(|s| !s.is_empty()) };
            let sub_h = sub.as_ref().map(|s| SUB_GAP + self.para_h(s, tok::HINT_SIZE, label_w)).unwrap_or(0.0);
            let text_h = label_h + sub_h;
            let top_h = (text_h + tok::ROW_VPAD * 2.0).max(tok::ROW_MIN);
            // A note (the setting does not work as chosen): under the
            // label and control, the row's full width.
            let note = self.row_note(row.key);
            let note_w = right - left - pad - tok::ROW_PAD;
            let (note_at, note_h) = match &note {
                Some((s, fix)) => note_box(top_h, text_h, self.para_h(s, tok::FOOTNOTE, note_w) + if fix.is_some() { NOTE_LINK_H } else { 0.0 }),
                None => (top_h, 0.0),
            };
            let h = top_h + note_h;
            let card = rect(left, y, right, y + h);
            self.note(Kind::Seg { first, last }, row.key, m.r(card));
            // A widget's row: where its how-to card goes (howto.rs).
            if super::widgets::kind_of(row.key).is_some() {
                if let Some(c) = clip_to(m.r(card), clip) {
                    self.howto_rows.borrow_mut().push((ri, c));
                }
            }
            tops.push((card.top, card.bottom));
            self.card_segment(dc, card, card_r, first, last)?;
            if matches!(row.ctl, Ctl::Navigate { .. }) && self.hover == Some(Hit::Button(ri)) {
                self.fill_segment(dc, self.row_fill(self.state_of(Hit::Button(ri))), m.r(card), card_r, first, last)?;
            }
            if !first {
                self.hairline(dc, m, left + pad, right - tok::ROW_PAD, y)?;
            }
            let lit = self.flash_of(ri);
            if lit > 0.0 {
                // A search result: lit by a tone step that fades (an outline
                // in high contrast, where fills do not show).
                match self.hc {
                    Some(hc) => {
                        let b = dc.CreateSolidColorBrush(&D2D1_COLOR_F { a: lit, ..super::rgba(hc.highlight) }, None)?;
                        let mr = m.r(card);
                        dc.DrawRectangle(&rect(mr.left + 1.0, mr.top + 1.0, mr.right - 1.0, mr.bottom - 1.0), &b, 2.0, None);
                    }
                    None => self.fill_segment(dc, D2D1_COLOR_F { a: self.row_fill(crate::theme::State::Selected).a * lit, ..self.row_fill(crate::theme::State::Selected) }, card, card_r, first, last)?,
                }
            }
            let cy = y + top_h / 2.0;
            let brand=match row.key {"chat.claude_mcp"=>Some("Claude"),"chat.codex_mcp"=>Some("Codex"),"chat.antigravity_mcp"=>Some("Antigravity"),_=>None};
            let brand_w=if brand.is_some(){36.}else{0.};
            let tx = m.span(left + pad + brand_w, label_w-brand_w);
            if let Some(name) = brand {
                let locked = self.row_locked(row.key);
                if locked {
                    dc.PushLayer(&D2D1_LAYER_PARAMETERS1 {
                        contentBounds: m.r(card),
                        geometricMask: std::mem::ManuallyDrop::new(None),
                        maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                        maskTransform: windows_numerics::Matrix3x2::identity(),
                        opacity: 0.4,
                        opacityBrush: std::mem::ManuallyDrop::new(None),
                        layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                    }, None::<&windows::Win32::Graphics::Direct2D::ID2D1Layer>);
                }
                let result = self.brand(dc, name, m.span(left + pad, 24.), cy - 12., 24.);
                if locked { dc.PopLayer(); }
                result?;
            }
            let ty = cy - text_h / 2.0;
            // Shown but not settable now (a display that is "Same as Dock
            // 1"): dimmed, its control faint and out of reach.
            let locked = self.row_locked(row.key);
            let label = self.row_label(row);
            self.para_dir(dc, &label, tx, ty, tok::LABEL_SIZE, false, label_w-brand_w, if locked { self.dim() } else { self.ink() }, rtl)?;
            if let Some(s) = &sub {
                self.para_dir(dc, s, tx, ty + label_h + SUB_GAP, tok::HINT_SIZE, false, label_w, self.dim(), rtl)?;
            }
            // The "i" right after the label's first line (before the
            // control in reading and Tab order).
            let used = self.text_w(&label, tok::LABEL_SIZE, false).min(label_w-brand_w)+brand_w;
            self.note(Kind::Label, row.key, m.r(rect(left + pad, ty, left + pad + used + if info { tok::INFO_W } else { 0.0 }, ty + label_h)));
            if info && visible_row(y, y + top_h, clip) {
                self.paint_info(dc, hits, ri, m.x(left + pad + used + tok::INFO_GAP + tok::INFO_D / 2.0), ty + line_h / 2.0, clip)?;
            }
            let visible = card.bottom > clip.top && card.top < clip.bottom;
            if let Some((s, fix)) = &note {
                let ny = y + note_at;
                let nx = m.span(left + pad, note_w);
                let small = tok::FOOTNOTE;
                let nh = self.para_dir(dc, s, nx, ny, small, false, note_w, self.warn(), rtl)?;
                // The note's box holds its link too (as note_box counts it).
                self.note(Kind::Note, s, rect(nx, ny, nx + note_w, ny + nh + if fix.is_some() { NOTE_LINK_H } else { 0.0 }));
                if let Some(fix) = fix {
                    let label = fix.label();
                    let lw = measure(label, small) + 16.0;
                    let r = rect(left + pad - 8.0, ny + nh + 2.0, left + pad - 8.0 + lw, ny + nh + NOTE_LINK_H);
                    if self.hover == Some(Hit::RowFix(ri)) {
                        fill(self.row_fill(self.state_of(Hit::RowFix(ri))), r, self.ctl_r())?;
                    }
                    let link = self.ink();
                    let lx = m.r(r).left + 8.0;
                    let tw = self.label(dc, label, lx, self.text_top((r.top + r.bottom) / 2.0, small), small, true, link)?;
                    // Underlined: a link, also without color.
                    self.fill_round(dc, link, rect(lx, r.bottom - 4.0, lx + tw, r.bottom - 3.0), 0.0)?;
                    if visible {
                        if let Some(c) = clip_to(m.r(r), clip) {
                            hits.push((c, Hit::RowFix(ri)));
                        }
                    }
                }
            }
            let hit = |r: D2D_RECT_F, h: Hit, hits: &mut Vec<(D2D_RECT_F, Hit)>| {
                if visible && !locked {
                    if let Some(r) = clip_to(m.r(r), clip) {
                        hits.push((r, h));
                    }
                }
            };
            if locked {
                let lp = D2D1_LAYER_PARAMETERS1 {
                    contentBounds: card,
                    geometricMask: std::mem::ManuallyDrop::new(None),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: windows_numerics::Matrix3x2::identity(),
                    opacity: 0.4,
                    opacityBrush: std::mem::ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS1_NONE,
                };
                dc.PushLayer(&lp, None::<&ID2D1Layer>);
            }
            let ch = tok::CONTROL_H;
            if new_note_w > 0.0 {
                let r = rect(ctl_left, cy - ch / 2.0, ctl_left + new_note_w, cy + ch / 2.0);
                self.button(dc, m.r(r), t("notes.new"), self.hover == Some(Hit::NewNote(ri)))?;
                hit(r, Hit::NewNote(ri), hits);
            }
            match &row.ctl {
                Ctl::Header | Ctl::Fold { .. } => {}
                Ctl::Shortcut { id } => {
                    // One field to the row's edge; its Reset (when not the
                    // default) inside its end, like a search box's clear.
                    let field = rect(cr - tok::FIELD_W, cy - ch / 2.0, cr, cy + ch / 2.0);
                    let reset = rect(cr - RESET_W, cy - ch / 2.0, cr, cy + ch / 2.0);
                    self.note(Kind::Ctl { kind: CtlKind::Field, cy }, row.key, m.r(field));
                    self.paint_shortcut(dc, ri, id, m.r(field), m.r(reset))?;
                    let fixed = crate::shortcuts::find(id).is_some_and(crate::shortcuts::read_only);
                    if !fixed {
                        hit(field, Hit::Shortcut(ri), hits);
                        if self.cfg.shortcut(id) != crate::shortcuts::find(id).map(|s| s.default).unwrap_or("none") {
                            hit(reset, Hit::ShortcutReset(ri), hits);
                        }
                    }
                }
                Ctl::Toggle { get, .. } => {
                    self.note(Kind::Ctl { kind: CtlKind::Toggle, cy }, row.key, m.r(rect(cr - tok::SWITCH_W, cy - tok::SWITCH_H / 2.0, cr, cy + tok::SWITCH_H / 2.0)));
                    self.toggle_switch(dc, m, cr, cy, get(&self.cfg), self.hover == Some(Hit::Toggle(ri)), row.key)?;
                    hit(rect(cr - tok::SWITCH_W - 8.0, cy - 16.0, cr + 4.0, cy + 16.0), Hit::Toggle(ri), hits);
                }
                Ctl::Slider { get, min, max, step, unit, .. } => {
                    let v = get(&self.cfg);
                    let val = model::fmt_value(v, *step, model::unit_text(unit));
                    let vw = SLIDER_VALUE_W;
                    let sized = row.key == "companion.size";
                    let vx = if rtl { m.x(cr) } else { cr - measure(&val, tok::BODY) };
                    let (x0, tw) = (cr - vw - 8.0 - track_w, track_w);
                    if sized {
                        let small = t("companion.small");
                        let large = t("companion.large");
                        self.label(dc, small, m.span(x0 - 12. - measure(small, 12.), measure(small, 12.)), self.text_top(cy, tok::FOOTNOTE), tok::FOOTNOTE, false, self.dim())?;
                        self.label(dc, large, m.span(x0 + tw + 16., measure(large, 12.)), self.text_top(cy, tok::FOOTNOTE), tok::FOOTNOTE, false, self.dim())?;
                    } else {
                        self.label(dc, &val, vx, self.text_top(cy, tok::BODY), tok::BODY, false, self.dim())?;
                    }
                    self.note(Kind::Ctl { kind: CtlKind::Slider, cy }, row.key, m.r(rect(x0 - 9.0, cy - 9.0, cr, cy + 9.0)));
                    let f = ((v - min) / (max - min)).clamp(0.0, 1.0);
                    // No marks under the track (small / medium / large
                    // with names crowded the row; the Mac's has none): the
                    // value stands at its end.
                    let kx = m.x(x0 + tw * f);
                    match self.hc {
                        // High contrast: the system's own colors, opaque, with
                        // outlines (no washes, no fixed white knob).
                        Some(h) => {
                            let (bg, fg, hl) = (super::rgba(h.window), super::rgba(h.text), super::rgba(h.highlight));
                            let track = m.r(rect(x0, cy - 3.0, x0 + tw, cy + 3.0));
                            fill(bg, rect(x0, cy - 3.0, x0 + tw, cy + 3.0), 3.0)?;
                            let ob = dc.CreateSolidColorBrush(&fg, None)?;
                            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: track, radiusX: 3.0, radiusY: 3.0 }, &ob, 1.0, None);
                            fill(hl, rect(x0, cy - 3.0, x0 + tw * f, cy + 3.0), 3.0)?;
                            let e = D2D1_ELLIPSE { point: Vector2 { X: kx, Y: cy }, radiusX: 9.0, radiusY: 9.0 };
                            let kb = dc.CreateSolidColorBrush(&bg, None)?;
                            dc.FillEllipse(&e, &kb);
                            dc.DrawEllipse(&e, &ob, 2.0, None);
                        }
                        // The track a tone step, its filled part the "on"
                        // tone, a white knob floating on a soft shadow.
                        None => {
                            fill(self.track_fill(), rect(x0, cy - 2.0, x0 + tw, cy + 2.0), 2.0)?;
                            fill(self.on_tone(), rect(x0, cy - 2.0, x0 + tw * f, cy + 2.0), 2.0)?;
                            let held = self.dragging.is_some_and(|d| matches!(d, Hit::Slider(r, ..) if r == ri));
                            // The menus' knob: 16 (18 held), elevation E1.
                            let knob = super::rgba(self.look.knob());
                            self.knob(dc, kx, cy, if held { 9.0 } else { 8.0 }, knob)?;
                        }
                    }
                    // Right to left the track runs from its right end
                    // (a negative width: set_slider needs no other change).
                    let h = if rtl { Hit::Slider(ri, m.x(x0), -tw) } else { Hit::Slider(ri, x0, tw) };
                    hit(rect(x0 - 10.0, cy - 14.0, x0 + tw + 10.0, cy + 14.0), h, hits);
                }
                Ctl::Button { text: label } => {
                    let label = t(label);
                    let tw = measure(label, tok::BODY) + 32.0;
                    let r = rect(cr - tw, cy - ch / 2.0, cr, cy + ch / 2.0);
                    self.note(Kind::Ctl { kind: CtlKind::Button, cy }, row.key, m.r(r));
                    self.button(dc, m.r(r), label, self.hover == Some(Hit::Button(ri)))?;
                    hit(r, Hit::Button(ri), hits);
                }
                Ctl::Navigate { value, .. } => {
                    let label = value(&self.cfg);
                    let tw = measure(label, tok::BODY) + 1.0;
                    let x = cr - tw - 24.0;
                    self.text_box(dc, label, m.span(x, tw), self.text_top(cy, tok::BODY), tok::BODY, false, tw, self.dim(), rtl)?;
                    let arrow = if rtl { Glyph::ChevronLeft } else { Glyph::ChevronRight };
                    self.glyph(dc, arrow, 16.0, m.x(cr - 8.0), cy, self.dim())?;
                    self.note(Kind::Ctl { kind: CtlKind::Button, cy }, row.key, m.r(rect(x, cy - ch / 2.0, cr, cy + ch / 2.0)));
                    hit(card, Hit::Button(ri), hits);
                }
                Ctl::Choice { opts, get, .. } if !compact => {
                    let cur = get(&self.cfg);
                    let widths = segment_widths(opts.iter().map(|o| measure(model::opt_text(o), tok::BODY)));
                    let total: f32 = widths.iter().sum();
                    // The track (to the row's edge), the options inset in
                    // it, the picked one raised.
                    let inset = tok::SEGMENT_INSET;
                    let mut x = cr - inset - total;
                    let track = rect(x - inset, cy - ch / 2.0, cr, cy + ch / 2.0);
                    self.note(Kind::Ctl { kind: CtlKind::Choice, cy }, row.key, m.r(track));
                    fill(self.ctl_fill(crate::theme::State::Rest), track, self.ctl_r())?;
                    for (oi, o) in opts.iter().enumerate() {
                        let r = rect(x, cy - ch / 2.0 + inset, x + widths[oi], cy + ch / 2.0 - inset);
                        let picked = oi == cur;
                        // Concentric with the track.
                        let inner = crate::theme::concentric(self.ctl_r(), inset);
                        if picked {
                            self.pick(dc, m.r(r), inner)?;
                        } else if self.hover == Some(Hit::Choice(ri, oi)) {
                            fill(self.row_fill(self.state_of(Hit::Choice(ri, oi))), r, inner)?;
                        }
                        let fg = if picked { self.pick_ink() } else { self.dim() };
                        // Centered in its part; the pick keeps its weight
                        // (raised, not bolder: no width jump).
                        let w = self.text_w(model::opt_text(o), tok::BODY, false);
                        let mr = m.r(r);
                        self.label(dc, model::opt_text(o), (mr.left + mr.right - w) / 2.0, self.text_top(cy, tok::BODY), tok::BODY, false, fg)?;
                        // The parts are told apart by a short divider where
                        // neither side is the picked one (a hairline).
                        if oi + 1 < opts.len() && oi != cur && oi + 1 != cur && self.hc.is_none() {
                            let (lx, lw) = crate::popup::hairline(x + widths[oi], self.scale);
                            fill(super::rgba(self.look.hairline()), rect(lx, cy - 7.0, lx + lw, cy + 7.0), 0.0)?;
                        }
                        hit(r, Hit::Choice(ri, oi), hits);
                        x += widths[oi];
                    }
                }
                Ctl::Menu { opts, get, .. } if swatches => {
                    let cur = get(&self.cfg).min(opts.len() - 1);
                    self.note(Kind::Ctl { kind: CtlKind::Swatches, cy }, row.key, m.r(rect(cr - swatches_w(opts.len()), cy - tok::SWATCH_D / 2.0, cr, cy + tok::SWATCH_D / 2.0)));
                    self.paint_swatches(dc, ri, opts.len(), cur, cr, cy, m)?;
                    for (oi, x) in swatch_centers(cr, opts.len()).into_iter().enumerate() {
                        let d = tok::SWATCH_D / 2.0 + 4.0;
                        hit(rect(x - d, cy - d, x + d, cy + d), Hit::Swatch(ri, oi), hits);
                    }
                }
                Ctl::Menu { opts, get, .. } | Ctl::Choice { opts, get, .. } => {
                    let cur = get(&self.cfg).min(opts.len() - 1);
                    let open = self.menu.map(|(r, _)| r) == Some(ri);
                    let r = rect(cr - tok::FIELD_W, cy - ch / 2.0, cr, cy + ch / 2.0);
                    self.note(Kind::Ctl { kind: CtlKind::Field, cy }, row.key, m.r(r));
                    let st = if self.pressed == Some(Hit::Menu(ri)) { crate::theme::State::Pressed } else if open || self.hover == Some(Hit::Menu(ri)) { crate::theme::State::Hover } else { crate::theme::State::Rest };
                    let mr = m.r(r);
                    if row.key != "theme.accent" || self.cfg.theme.custom_accent().is_none() {
                        self.menu_value(dc, m.r(r), model::opt_text(opts[cur]), st, rtl)?;
                    } else {
                    fill(self.ctl_fill(st), r, self.ctl_r())?;
                    // The accent's own color: its swatch before "Custom…"
                    // (the two centered as one).
                    let swatch = (row.key == "theme.accent").then(|| self.cfg.theme.custom_accent()).flatten();
                    let sw = if swatch.is_some() { 18.0 } else { 0.0 };
                    let (_, room) = field_value_x(mr, 0.0, RESET_W, rtl);
                    // A long choice ends in an ellipsis in its fixed-width
                    // button (the list shows it whole); centered like the
                    // key fields' values (field_value_x).
                    let shown = self.ellipsized(model::opt_text(opts[cur]), tok::BODY, false, room - sw);
                    let tw = self.text_w(&shown, tok::BODY, false);
                    let (gx, _) = field_value_x(mr, tw + sw, RESET_W, rtl);
                    let tx = if rtl { gx } else { gx + sw };
                    if let Some(c) = swatch {
                        let sx = if rtl { gx + tw + 12.0 } else { gx + 6.0 };
                        let e = D2D1_ELLIPSE { point: Vector2 { X: sx, Y: cy }, radiusX: 6.0, radiusY: 6.0 };
                        dc.FillEllipse(&e, &dc.CreateSolidColorBrush(&super::rgba(c), None)?);
                        let edge = match self.hc {
                            Some(h) => super::rgba(h.text),
                            None => self.wash(0.24),
                        };
                        dc.DrawEllipse(&e, &dc.CreateSolidColorBrush(&edge, None)?, 1.0, None);
                    }
                    // (Right to left text_box hugs its box's right end: the box starts
                    // one before, so the text is where field_value_x put it.)
                    self.text_box(dc, &shown, if rtl { tx - 1.0 } else { tx }, self.text_top(cy, tok::BODY), tok::BODY, false, tw + 1.0, self.ink(), rtl)?;
                    self.chevron(dc, if rtl { mr.left + 16.0 } else { mr.right - 16.0 }, cy, self.dim())?;
                    }
                    if open {
                        *anchor = Some(mr);
                    }
                    hit(r, Hit::Menu(ri), hits);
                }
            }
            if locked {
                dc.PopLayer();
            }
            y += h;
            // "Same as Dock 1": its question, or "Undo" once done (a card
            // of its own after the row's).
            if dock_page && row.key == model::SAME_AS_MAIN && last {
                y = self.paint_same_card(dc, hits, y, left, right, clip)?;
            }
        }
        Ok(y)
    }

    /// Light / dark as pictures (`opts`, `cur` picked) for row `ri`, a card
    /// of its own from `y` (`ends`: its first / last part); returns its
    /// height. Each tile is a small window in that look ("Same as
    /// Windows": half light, half dark), its name under it; the one picked
    /// has the accent's ring (Highlight in high contrast). Hits: the
    /// choice's own (keys and screen readers as a segmented choice).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn paint_mode_tiles(
        &self,
        dc: &ID2D1DeviceContext,
        hits: &mut Vec<(D2D_RECT_F, Hit)>,
        ri: usize,
        opts: &[&'static str],
        cur: usize,
        y: f32,
        m: Mirror,
        clip: D2D_RECT_F,
        ends: (bool, bool),
    ) -> Result<f32> {
        let names: Vec<&str> = opts.iter().map(|o| model::opt_text(o)).collect();
        let name_w = names.iter().map(|n| self.text_w(n, tok::FOOTNOTE, false)).fold(0.0, f32::max);
        let (tiles, h) = mode_tiles(m.left, m.right, opts.len(), name_w);
        let card = rect(m.left, y, m.right, y + h);
        self.card_segment(dc, card, self.card_r(), ends.0, ends.1)?;
        if !ends.0 {
            self.hairline(dc, m, m.left + tok::ROW_PAD, m.right - tok::ROW_PAD, y)?;
        }
        let lit = self.flash_of(ri);
        if lit > 0.0 && self.hc.is_none() {
            self.fill_segment(dc, D2D1_COLOR_F { a: self.row_fill(crate::theme::State::Selected).a * lit, ..self.row_fill(crate::theme::State::Selected) }, card, self.card_r(), ends.0, ends.1)?;
        }
        let rad = self.ctl_r();
        for (oi, t0) in tiles.iter().enumerate() {
            let r = m.r(rect(t0.left, y + t0.top, t0.right, y + t0.bottom));
            let picked = oi == cur;
            let hot = self.hover == Some(Hit::Choice(ri, oi));
            // The scene: THEME_MODES' order is system, light, dark.
            match oi {
                0 => {
                    // Light on the reading side's first half, dark after it.
                    let mid = (r.left + r.right) / 2.0;
                    let (a, b) = if m.on { (false, true) } else { (true, false) };
                    dc.PushAxisAlignedClip(&rect(r.left, r.top, mid, r.bottom), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    let first = self.mode_scene(dc, r, a);
                    dc.PopAxisAlignedClip();
                    first?;
                    dc.PushAxisAlignedClip(&rect(mid, r.top, r.right, r.bottom), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                    let second = self.mode_scene(dc, r, b);
                    dc.PopAxisAlignedClip();
                    second?;
                }
                1 => self.mode_scene(dc, r, true)?,
                _ => self.mode_scene(dc, r, false)?,
            }
            // The pick's ring (2 DIP, 3 out, concentric); a faint one under
            // the pointer.
            let ring = |c: D2D1_COLOR_F, w: f32| -> Result<()> {
                let b = dc.CreateSolidColorBrush(&c, None)?;
                let o = 3.0;
                dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left - o, r.top - o, r.right + o, r.bottom + o), radiusX: rad + o, radiusY: rad + o }, &b, w, None);
                Ok(())
            };
            match self.hc {
                Some(h) if picked => ring(super::rgba(h.highlight), 2.0)?,
                Some(h) => ring(super::rgba(h.text), 1.0)?,
                None if picked => ring(self.on_tone(), 2.0)?,
                None if hot => ring(self.wash(0.20), 1.0)?,
                None => {}
            }
            // Its name, centered under it (cut to the tile and a little
            // more), in the text color once picked.
            let room = (r.right - r.left) + tok::TILE_GAP - 4.0;
            let name = self.ellipsized(names[oi], tok::FOOTNOTE, false, room);
            let w = self.text_w(&name, tok::FOOTNOTE, false);
            self.label(dc, &name, (r.left + r.right - w) / 2.0, r.bottom + 6.0, tok::FOOTNOTE, false, if picked { self.ink() } else { self.dim() })?;
            let hr = rect(r.left - 4.0, r.top - 4.0, r.right + 4.0, r.bottom + 6.0 + tok::TILE_NAME);
            if visible_row(hr.top, hr.bottom, clip) {
                if let Some(c) = clip_to(hr, clip) {
                    hits.push((c, Hit::Choice(ri, oi)));
                }
            }
        }
        Ok(h)
    }

    /// A tile's small window in the light or dark look: its background,
    /// a card on it and a bar along its bottom (the high contrast colors
    /// for both).
    unsafe fn mode_scene(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, light: bool) -> Result<()> {
        let rad = self.ctl_r();
        let (bg, panel, bar) = match (self.hc, light) {
            (Some(h), _) => (super::rgba(h.window), super::rgba(h.window), super::rgba(h.text)),
            (None, true) => (col(0.91, 0.91, 0.90, 1.0), col(1.0, 1.0, 1.0, 1.0), col(0.0, 0.0, 0.0, 0.18)),
            (None, false) => (col(0.13, 0.13, 0.14, 1.0), col(0.24, 0.24, 0.25, 1.0), col(1.0, 1.0, 1.0, 0.22)),
        };
        self.fill_round(dc, bg, r, rad)?;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let inset = (w * 0.12).round();
        self.fill_round(dc, panel, rect(r.left + inset, r.top + inset, r.right - inset, r.bottom - h * 0.36), 2.0)?;
        self.fill_round(dc, bar, rect(r.left + inset, r.bottom - h * 0.24, r.right - inset, r.bottom - h * 0.24 + 6.0), 3.0)?;
        if let Some(hc) = self.hc {
            let b = dc.CreateSolidColorBrush(&super::rgba(hc.text), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), radiusX: rad, radiusY: rad }, &b, 1.0, None);
        }
        Ok(())
    }

    /// The accent's colors (`n` options, `cur` picked) ending at `cr`,
    /// centered on `cy`: Automatic first (Windows' accent beside the
    /// achromatic "on"), the presets, then "Custom…" (a "+", or the color
    /// chosen once it is). The pick has a thin ring a little out (the
    /// text color: the dot itself is the color); none of them an outline
    /// (high contrast: the system's edge).
    #[allow(clippy::too_many_arguments)]
    unsafe fn paint_swatches(&self, dc: &ID2D1DeviceContext, ri: usize, n: usize, cur: usize, cr: f32, cy: f32, m: Mirror) -> Result<()> {
        use crate::config::{ACCENTS, ACCENT_CUSTOM};
        let rad = tok::SWATCH_D / 2.0;
        let dot = |x: f32, c: D2D1_COLOR_F| -> Result<()> {
            let b = dc.CreateSolidColorBrush(&c, None)?;
            dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: x, Y: cy }, radiusX: rad, radiusY: rad }, &b);
            Ok(())
        };
        for (oi, x) in swatch_centers(cr, n).into_iter().enumerate() {
            let x = m.x(x);
            let picked = oi == cur;
            if oi == ACCENT_CUSTOM {
                match self.cfg.theme.custom_accent() {
                    Some(c) => dot(x, super::rgba(c))?,
                    None => {
                        dot(x, self.wash(0.10))?;
                        self.glyph(dc, Glyph::Plus, 10.0, x, cy, self.dim())?;
                    }
                }
            } else if oi == 0 {
                // Automatic: Windows' accent (the light look's) and the
                // achromatic "on" (the dark look's), half and half.
                let win = super::rgba(self.look_for.1.accent);
                dc.PushAxisAlignedClip(&rect(x - rad - 1.0, cy - rad - 1.0, x, cy + rad + 1.0), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                let a = dot(x, win);
                dc.PopAxisAlignedClip();
                a?;
                dc.PushAxisAlignedClip(&rect(x, cy - rad - 1.0, x + rad + 1.0, cy + rad + 1.0), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                let b = dot(x, if self.light { col(0.10, 0.10, 0.10, 1.0) } else { col(0.94, 0.94, 0.94, 1.0) });
                dc.PopAxisAlignedClip();
                b?;
            } else if let Some(c) = ACCENTS.get(oi).and_then(|(hex, _)| crate::theme::parse_color(hex)) {
                dot(x, super::rgba(c))?;
            }
            let ring = |c: D2D1_COLOR_F, w: f32, out: f32| -> Result<()> {
                let b = dc.CreateSolidColorBrush(&c, None)?;
                dc.DrawEllipse(&D2D1_ELLIPSE { point: Vector2 { X: x, Y: cy }, radiusX: rad + out, radiusY: rad + out }, &b, w, None);
                Ok(())
            };
            match self.hc {
                Some(h) if picked => ring(super::rgba(h.highlight), 2.0, 3.0)?,
                Some(h) => ring(super::rgba(h.text), 1.0, 0.0)?,
                None if picked => ring(self.dim(), 1.5, 3.0)?,
                None if self.hover == Some(Hit::Swatch(ri, oi)) => ring(self.wash(0.25), 1.5, 3.0)?,
                None => {}
            }
        }
        Ok(())
    }

    /// A hairline between two rows of a group, from `x0` to `x1` (inset
    /// from the text, never edge to edge): a faint tone, the text color in
    /// high contrast.
    pub(super) unsafe fn hairline(&self, dc: &ID2D1DeviceContext, m: Mirror, x0: f32, x1: f32, y: f32) -> Result<()> {
        // One device pixel on a whole pixel (popup::hairline), the theme's
        // hairline tone (the text color in high contrast).
        let c = super::rgba(self.look.hairline());
        let c = match self.hc {
            Some(h) => super::rgba(h.text),
            None => c,
        };
        let (top, th) = crate::popup::hairline(y, self.scale);
        let r = m.r(rect(x0, top, x1, top + th));
        self.note(Kind::Hairline, "", r);
        self.fill_round(dc, c, r, 0.0)
    }

    /// What is picked (the segmented choice's pick, the sub-pages', the
    /// sidebar's page): Flat inverts it (the "on" tone, no shadow); Glass
    /// lays a see-through white pill on it (glass A, 2026-11-12: no line,
    /// no lit edge, no shadow); Highlight in high contrast. Its text:
    /// pick_ink.
    pub(super) unsafe fn pick(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, rad: f32) -> Result<()> {
        if let Some(h) = self.hc {
            return self.fill_round(dc, super::rgba(h.highlight), r, rad);
        }
        if self.solid {
            return self.fill_round(dc, super::rgba(self.look.active()), r, rad);
        }
        let (d, l) = crate::theme::fills::GLASS_PICK;
        self.fill_round(dc, D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: if self.light { l } else { d } }, r, rad)
    }

    /// The text on pick(): Flat's inverted ink, Glass's own; HighlightText
    /// in high contrast.
    pub(super) fn pick_ink(&self) -> D2D1_COLOR_F {
        match self.hc {
            Some(_) => self.on_ink(),
            None if self.solid => super::rgba(self.look.on_active()),
            None => self.ink(),
        }
    }

    /// A card's segment (a group's rows, `top` / `bottom`: its ends):
    /// card_fill alone (glass A: no lit edge).
    pub(super) unsafe fn card_segment(&self, dc: &ID2D1DeviceContext, r: D2D_RECT_F, rad: f32, top: bool, bottom: bool) -> Result<()> {
        self.fill_segment(dc, self.card_fill(), r, rad, top, bottom)
    }

    /// A shortcut row's key field (what is set, "Press keys…" while
    /// recording) and its Reset button (only when not the default).
    unsafe fn paint_shortcut(&self, dc: &ID2D1DeviceContext, ri: usize, id: &str, field: D2D_RECT_F, reset: D2D_RECT_F) -> Result<()> {
        use crate::shortcuts::{self, Kind};
        let Some(s) = shortcuts::find(id) else { return Ok(()) };
        let v = self.cfg.shortcut(id);
        let recording = self.recording.map(|(r, _)| r) == Some(ri);
        let fixed = shortcuts::read_only(s);
        let off = shortcuts::is_off(&v);
        let text = if recording {
            let mods = self.recording.map(|(_, m)| m).unwrap_or(0);
            if mods == 0 { t("shortcuts.press").to_string() } else { shortcuts::display_mods(mods) }
        } else if off {
            t("shortcuts.off").to_string()
        } else if let Kind::Switch(on) = s.kind {
            if on.eq_ignore_ascii_case("win") { "Win".to_string() } else { shortcuts::display_spec(on) }
        } else {
            shortcuts::display_spec(&v)
        };
        let hot = self.hover == Some(Hit::Shortcut(ri)) && !fixed;
        self.key_field(dc, field, recording, hot, fixed)?;
        let fg = if fixed || (off && !recording) { self.dim() } else if recording && self.hc.is_none() { self.on_ink() } else { self.ink() };
        // The text on the field's center line, clear of Reset's end when
        // shown (field_value_x: the list buttons' too).
        let resettable = !fixed && v != s.default;
        let bold = !off && !recording;
        let tw = self.text_w(&text, tok::BODY, bold);
        let end = if resettable { RESET_W } else { FIELD_TEXT_PAD };
        let (tx, room) = field_value_x(field, tw, end, reset.left <= field.left + 1.0);
        self.text_box(dc, &text, tx, self.text_top((field.top + field.bottom) / 2.0, tok::BODY), tok::BODY, bold, room.min(tw + 1.0), fg, false)?;
        if resettable {
            let hot = self.hover == Some(Hit::ShortcutReset(ri));
            if hot {
                self.fill_round(dc, self.ctl_fill(crate::theme::State::Hover), reset, self.ctl_r())?;
            }
            // The "undo" arrow, as tall as the old font glyph at 13 DIP
            // (its ink fills about 80% of the em box).
            self.glyph(dc, Glyph::Undo, 16.0, (reset.left + reset.right) / 2.0, (reset.top + reset.bottom) / 2.0, self.dim())?;
        }
        Ok(())
    }

    /// A key recorder's field: a tone step at rest, the "on" tone while
    /// it records (no outline); Highlight's outline in high contrast.
    pub(super) unsafe fn key_field(&self, dc: &ID2D1DeviceContext, field: D2D_RECT_F, recording: bool, hot: bool, fixed: bool) -> Result<()> {
        if fixed {
            return Ok(());
        }
        match (self.hc, recording) {
            (Some(h), true) => {
                self.fill_round(dc, self.wash(0.12), field, self.ctl_r())?;
                let b = dc.CreateSolidColorBrush(&super::rgba(h.highlight), None)?;
                dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(field.left + 1.0, field.top + 1.0, field.right - 1.0, field.bottom - 1.0), radiusX: self.ctl_r() - 1.0, radiusY: self.ctl_r() - 1.0 }, &b, 2.0, None);
                Ok(())
            }
            (None, true) => self.fill_round(dc, self.on_tone(), field, self.ctl_r()),
            _ => self.fill_round(dc, self.ctl_fill(if hot { crate::theme::State::Hover } else { crate::theme::State::Rest }), field, self.ctl_r()),
        }
    }

    /// The row asked to be shown (a search result): scrolled into view.
    /// True if the scroll changed (draw again).
    pub(super) fn reveal_row(&mut self, ri: usize) -> bool {
        let Some(&(top, bottom)) = self.row_tops.borrow().get(ri) else { return false };
        let view = super::win_h() - super::FOOTER - self.content_top();
        let origin = self.content_top() - self.scroll;
        let (top, bottom) = (top - origin, bottom - origin);
        let max = (self.content_h - view + 16.0).max(0.0);
        let s = if bottom > self.scroll + view - 16.0 {
            bottom - view + 24.0
        } else if top < self.scroll + 8.0 {
            top - 16.0
        } else {
            self.scroll
        }
        .clamp(0.0, max);
        if s == self.scroll {
            return false;
        }
        self.scroll = s;
        true
    }
}

/// A rectangle with its top (`top`) or bottom corners rounded by `rad`.
unsafe fn rounded_one_end(dc: &ID2D1DeviceContext, r: D2D_RECT_F, rad: f32, top: bool) -> Result<windows::Win32::Graphics::Direct2D::ID2D1PathGeometry> {
    use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED};
    use windows::Win32::Graphics::Direct2D::{D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_SWEEP_DIRECTION_CLOCKWISE};
    let factory = dc.GetFactory()?;
    let geo = factory.CreatePathGeometry()?;
    let sink = geo.Open()?;
    let p = |x: f32, y: f32| Vector2 { X: x, Y: y };
    let arc = |to: Vector2| D2D1_ARC_SEGMENT {
        point: to,
        size: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F { width: rad, height: rad },
        rotationAngle: 0.0,
        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
        arcSize: D2D1_ARC_SIZE_SMALL,
    };
    if top {
        sink.BeginFigure(p(r.left, r.bottom), D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLine(p(r.left, r.top + rad));
        sink.AddArc(&arc(p(r.left + rad, r.top)));
        sink.AddLine(p(r.right - rad, r.top));
        sink.AddArc(&arc(p(r.right, r.top + rad)));
        sink.AddLine(p(r.right, r.bottom));
    } else {
        sink.BeginFigure(p(r.left, r.top), D2D1_FIGURE_BEGIN_FILLED);
        sink.AddLine(p(r.right, r.top));
        sink.AddLine(p(r.right, r.bottom - rad));
        sink.AddArc(&arc(p(r.right - rad, r.bottom)));
        sink.AddLine(p(r.left + rad, r.bottom));
        sink.AddArc(&arc(p(r.left, r.bottom - rad)));
    }
    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
    sink.Close()?;
    Ok(geo)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: RowKind = RowKind::Header;

    /// Every settings source, its tests cut off.
    fn sources() -> Vec<(&'static str, String)> {
        [
            ("access.rs", include_str!("access.rs")),
            ("calendar.rs", include_str!("calendar.rs")),
            ("client.rs", include_str!("client.rs")),
            ("corners.rs", include_str!("corners.rs")),
            ("dialogs.rs", include_str!("dialogs.rs")),
            ("displays_view.rs", include_str!("displays_view.rs")),
            ("extra.rs", include_str!("extra.rs")),
            ("input.rs", include_str!("input.rs")),
            ("apply.rs", include_str!("apply.rs")),
            ("mod.rs", include_str!("mod.rs")),
            ("model.rs", include_str!("model.rs")),
            ("pins.rs", include_str!("pins.rs")),
            ("record.rs", include_str!("record.rs")),
            ("report.rs", include_str!("report.rs")),
            ("reset.rs", include_str!("reset.rs")),
            ("rows.rs", include_str!("rows.rs")),
            ("search_box.rs", include_str!("search_box.rs")),
            ("status.rs", include_str!("status.rs")),
            ("tooltip.rs", include_str!("tooltip.rs")),
            ("topbar_view.rs", include_str!("topbar_view.rs")),
            ("update_card.rs", include_str!("update_card.rs")),
            ("welcome.rs", include_str!("welcome.rs")),
        ]
        .into_iter()
        .map(|(n, s)| (n, without_tests(s)))
        .collect()
    }

    /// `src` without its `#[cfg(test)]` blocks (anywhere in the file).
    fn without_tests(src: &str) -> String {
        let mut out = String::new();
        let mut rest = src;
        while let Some(i) = rest.find("#[cfg(test)]") {
            out.push_str(&rest[..i]);
            let after = &rest[i..];
            let Some(open) = after.find('{') else { return out };
            let mut depth = 0;
            let mut end = after.len();
            for (k, ch) in after[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + k + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            rest = &after[end..];
        }
        out.push_str(rest);
        out
    }

    #[test]
    fn settings_text_is_drawn_with_the_shared_format_on_the_ramp() {
        // Design B3: the shared text format (locale, Hangul fallback), the
        // ramp's sizes, never Bold, no symbol for an icon.
        let off_ramp = [", 12.5,", ", 13.5,", ", 10.5,", ", 11.5,", ", 14.0, true", ", 14.0, false", ", 17.0,", ", 18.0, true", ", 10.0, true"];
        for (name, code) in sources() {
            assert!(!code.contains("CreateTextFormat"), "{name}: gfx::ui_format, not CreateTextFormat");
            assert!(!code.contains("FONT_WEIGHT_BOLD"), "{name}: never Bold");
            assert!(!code.contains("\"Segoe UI Variable"), "{name}: the user's UI font (fonts::THEME_DEFAULT)");
            assert!(!code.contains("u{229E}"), "{name}: no \"⊞\" symbol");
            for p in off_ramp {
                assert!(!code.contains(p), "{name}: an off-ramp text size ({p})");
            }
        }
    }

    /// The arguments of every `name` call (`name` ends in "(") in `code`,
    /// split at its own commas (not those in nested calls, closures'
    /// brackets or strings), trimmed.
    fn calls<'a>(code: &'a str, name: &str) -> Vec<Vec<&'a str>> {
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(i) = code[from..].find(name) {
            let start = from + i + name.len();
            from = start;
            let (mut depth, mut text, mut arg) = (0i32, false, start);
            let mut args = Vec::new();
            let mut prev = ' ';
            for (k, ch) in code[start..].char_indices() {
                match ch {
                    '"' if prev != '\\' => text = !text,
                    _ if text => {}
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' if depth == 0 => {
                        args.push(code[arg..start + k].trim());
                        break;
                    }
                    ')' | ']' | '}' => depth -= 1,
                    ',' if depth == 0 => {
                        args.push(code[arg..start + k].trim());
                        arg = start + k + 1;
                    }
                    _ => {}
                }
                prev = ch;
            }
            out.push(args);
        }
        out
    }

    #[test]
    fn settings_sizes_and_corners_are_tokens_not_numbers() {
        // Design B3 review P3 #5: every text size a call passes is a ramp
        // token (tok::*, ramp::*, or a size worked out from one), never a
        // bare number - the About title (22), the subtabs (13 - size) and the fields (13) were. Every
        // corner of a rounded rectangle is a radius token (ctl_r, card_r,
        // menu_r, ...), concentric with it for an inset outline, or a hairline
        // mark's (3 or less: rails, carets, a capsule of a thin bar).
        let sized = [
            (".text_box(", 4),
            (".para_dir(", 4),
            (".label(", 4),
            (".label_in(", 4),
            (".centered(", 3),
            (".text_w(", 1),
            (".text_width(", 1),
            (".para_h(", 1),
            (".text_top(", 1),
            (".ellipsized(", 1),
            (".text_layout(", 2),
            (".text_layout_line(", 2),
            (".text_layout_tabular(", 2),
            (".text_layout_wrapped(", 2),
            (".text_layout_wrapped_leading(", 2),
            (".text_fit(", 2),
        ];
        let number = |a: &str| a.parse::<f32>().ok();
        // Pictures of a screen in miniature (the effects' live demo, the
        // top bar's, the hot corners' map, the first-run flow's screens and
        // theme swatches) are drawn to their own small scale: sizes only.
        let miniature = ["topbar_view.rs", "corners.rs"];
        // Nothing left out: pins.rs takes the tokens too (fix/webapp2).
        let later: [&str; 0] = [];
        let welcome_cut = |code: &str| -> String {
            // welcome.rs: its scene() and theme_art() are such pictures.
            let mut out = code.to_string();
            for f in ["unsafe fn scene(", "unsafe fn theme_art("] {
                if let Some(i) = out.find(f) {
                    let end = out[i + 1..].find("\n    unsafe fn ").or_else(|| out[i + 1..].find("\n    pub(super) fn ")).map(|e| i + 1 + e).unwrap_or(out.len());
                    out.replace_range(i..end, "");
                }
            }
            out
        };
        let mut bad = Vec::new();
        for (name, code) in sources() {
            for (call, at) in sized {
                for args in calls(&code, call) {
                    if let Some(v) = args.get(at).and_then(|a| number(a)) {
                        bad.push(format!("{name}: {call} size {v}"));
                    }
                }
            }
            if miniature.contains(&name) || later.contains(&name) {
                continue;
            }
            let code = if name == "welcome.rs" { welcome_cut(&code) } else { code };
            let corner = |v: f32| v > 3.0;
            for args in calls(&code, ".fill_round(") {
                if let Some(v) = args.get(3).and_then(|a| number(a)).filter(|v| corner(*v)) {
                    bad.push(format!("{name}: fill_round radius {v}"));
                }
            }
            for args in calls(&code, "D2D1_ROUNDED_RECT {") {
                for a in args {
                    if let Some(v) = a.strip_prefix("radiusX:").and_then(|v| number(v.trim())).filter(|v| corner(*v)) {
                        bad.push(format!("{name}: rounded rect radius {v}"));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "off the ramp / radii scale:\n{}", bad.join("\n"));
        // The scanner itself: it finds a literal and leaves a token alone.
        let probe = "self.text_box(dc, t(\"a, (b\"), x, y, 22.0, true, w, c, rtl)?; self.fill_round(dc, c, r, self.ctl_r())?; self.fill_round(dc, c, r, 5.0)?;";
        assert_eq!(calls(probe, ".text_box(")[0][4], "22.0");
        assert_eq!(calls(probe, ".fill_round(").iter().map(|a| a[3]).collect::<Vec<_>>(), ["self.ctl_r()", "5.0"]);
    }

    #[test]
    fn settings_shadows_are_real_blurs() {
        // The stair-step shadows (stacked translucent shapes) are gone from
        // the list, the tooltip and the knob (glass A's pick has none).
        let corners = sources().into_iter().find(|(n, _)| *n == "corners.rs").unwrap().1;
        assert!(corners.contains("soft_shadows") && !corners.contains("for (grow, dy) in"));
        let tip = sources().into_iter().find(|(n, _)| *n == "tooltip.rs").unwrap().1;
        assert!(tip.contains("soft_shadows") && !tip.contains("for (grow, dy) in"));
        let rows = sources().into_iter().find(|(n, _)| *n == "rows.rs").unwrap().1;
        assert!(!rows.contains("for (grow, dy, k) in"), "the knob's rings");
        assert!(rows.contains("soft_shadows"), "the knob");
    }
    const S: RowKind = RowKind::Setting { break_after: false };

    fn row(first: bool, last: bool) -> Slot {
        Slot::Row { first, last, indent: false }
    }

    #[test]
    fn rows_fall_into_groups_under_their_headings() {
        // A row alone, then a heading over two rows: two groups.
        let kinds = [S, H, S, S];
        let got = slots(&kinds, &[true; 4]);
        assert_eq!(got, vec![row(true, true), Slot::Header, row(true, false), row(false, true)]);
        // A hidden row is skipped: its neighbours close the group around it.
        let got = slots(&kinds, &[true, true, true, false]);
        assert_eq!(got[2], row(true, true));
        assert_eq!(got[3], Slot::Hidden);
        // A heading whose rows are all hidden draws nothing under it.
        let got = slots(&[S, H, S], &[true, true, false]);
        assert_eq!(got, vec![row(true, true), Slot::Header, Slot::Hidden]);
        assert!(slots(&[], &[]).is_empty());
    }

    #[test]
    fn folds_open_inside_their_group_and_advanced_is_its_own() {
        let more = RowKind::Fold { n: 2, own_group: false };
        let adv = RowKind::Fold { n: 1, own_group: true };
        let kinds = [S, more, S, S, S, adv, S];
        let got = slots(&kinds, &[true; 7]);
        // The switch, "More options" and its two rows: one group, the two
        // indented; the next row goes on in it.
        assert_eq!(got[0], row(true, false));
        assert_eq!(got[1], row(false, false));
        assert_eq!(got[2], Slot::Row { first: false, last: false, indent: true });
        assert_eq!(got[3], Slot::Row { first: false, last: false, indent: true });
        assert_eq!(got[4], row(false, true));
        // "Advanced" starts a group of its own.
        assert_eq!(got[5], row(true, false));
        assert_eq!(got[6], row(false, true));
        // Closed: its rows hidden, it ends the group.
        let got = slots(&kinds, &[true, true, false, false, true, true, false]);
        assert_eq!(got[5], row(true, true));
    }

    #[test]
    fn a_row_with_something_under_it_ends_its_group() {
        let brk = RowKind::Setting { break_after: true };
        let got = slots(&[S, brk, S, S], &[true; 4]);
        assert_eq!(got, vec![row(true, false), row(false, true), row(true, false), row(false, true)]);
    }

    #[test]
    fn segments_are_even() {
        assert_eq!(segment_widths([20.0, 40.5, 30.0].into_iter()), vec![67.0; 3]);
        assert!(segment_widths(std::iter::empty()).is_empty());
    }

    #[test]
    fn the_knob_stays_inside_its_track() {
        let (l, r, rad, inset) = (100.0, 140.0, 9.0, 3.0);
        assert_eq!(knob_x(l, r, rad, inset, 0.0), 112.0);
        assert_eq!(knob_x(l, r, rad, inset, 1.0), 128.0);
        assert_eq!(knob_x(l, r, rad, inset, 0.5), 120.0);
        // A spring's overshoot never pushes it out.
        assert_eq!(knob_x(l, r, rad, inset, 1.2), 128.0);
        assert_eq!(knob_x(l, r, rad, inset, -0.3), 112.0);
    }

    #[test]
    fn colors_mix_toward_each_other() {
        let a = col(0.0, 0.0, 0.0, 1.0);
        let b = col(1.0, 1.0, 1.0, 0.0);
        let m = mix(a, b, 0.25);
        assert!((m.r - 0.25).abs() < 1e-6 && (m.a - 0.75).abs() < 1e-6);
        assert_eq!(mix(a, b, 2.0).r, 1.0, "clamped");
    }

    #[test]
    fn the_tokens_sit_on_a_4_dip_grid_with_one_set_of_corners() {
        for v in [tok::ROW_MIN, tok::ROW_PAD, tok::ROW_VPAD, tok::ROW_GAP, tok::GROUP_GAP, tok::HEADER_H, tok::FOLD_H, tok::FOLD_INDENT, tok::CONTROL_H, tok::SWITCH_W, tok::SWITCH_H, tok::SWATCH_D, tok::SWATCH_GAP, tok::TILE_GAP, tok::TILE_TOP, tok::TILE_NAME, tok::TILE_BOTTOM, tok::INFO_D, tok::INFO_GAP, tok::INFO_W, tok::INFO_HIT, tok::FIELD_W] {
            assert_eq!(v % 4.0, 0.0, "{v} off the grid");
        }
        assert_eq!(tok::ROW_MIN, 40.0, "a row is 40 DIP");
        // Type: body 13, footnote 12, caption 11 the least; the title 20.
        assert_eq!((tok::LABEL_SIZE, tok::HINT_SIZE, tok::HEADER_SIZE, tok::TITLE_SIZE), (13.0, 12.0, 12.0, 20.0));
        // Every size is a ramp size (theme::ramp), none in between.
        let ramp: Vec<f32> = crate::theme::ramp::RAMP.iter().map(|s| s.size).collect();
        for v in [tok::CAPTION, tok::FOOTNOTE, tok::BODY, tok::HEADLINE, tok::TITLE_SIZE, tok::LARGE_TITLE, tok::LABEL_SIZE, tok::HINT_SIZE, tok::HEADER_SIZE] {
            assert!(ramp.contains(&v), "{v} is off the ramp");
        }
        // Corners: the theme's (card 12, menu 12, control 8, tooltip 8); a
        // segment is concentric in its track (8 - 2 = 6); the switch a
        // capsule.
        let r = crate::theme::Theme::graphite().radii();
        assert_eq!((r.tile, r.menu, r.control, r.tip), (12.0, 12.0, 8.0, 8.0));
        assert_eq!(crate::theme::concentric(r.control, tok::SEGMENT_INSET), 6.0);
        const { assert!(tok::SWITCH_H <= tok::ROW_MIN - 2.0 * 8.0 && tok::CONTROL_H < tok::ROW_MIN) };
    }

    #[test]
    fn light_and_dark_is_a_card_of_its_own_on_the_appearance_page() {
        let rows = model::appearance_rows();
        let kinds: Vec<RowKind> = rows.iter().map(row_kind).collect();
        let got = slots(&kinds, &vec![true; rows.len()]);
        let at = |k: &str| rows.iter().position(|r| r.key == k).unwrap();
        // Its group label, the pictures alone, then the finish opens the
        // next card, the accent ends it (the theme file is under Advanced).
        assert_eq!(got[0], Slot::Header);
        assert_eq!(got[at(MODE_ROW)], row(true, true));
        assert_eq!(got[at("theme.finish")], row(true, false));
        assert_eq!(got[at(ACCENT_ROW)], row(false, true));
        assert!(at("theme.finish") < at(ACCENT_ROW), "the finish comes before the accent");
        // The text: under its own label, its last row ends it.
        assert_eq!(got[at("appearance.text_size")], row(false, true));
    }

    #[test]
    fn mode_tiles_fit_the_card_and_mirror() {
        // A card 452 DIP wide (the smallest window): three tiles as wide
        // as their longest name asks, 16 apart, from the row's padding.
        let (tiles, h) = mode_tiles(244.0, 696.0, 3, 100.0);
        assert_eq!(tiles.len(), 3);
        assert_eq!(tiles[0].left, 244.0 + tok::ROW_PAD);
        let w = tiles[0].right - tiles[0].left;
        assert_eq!(w, 108.0);
        assert_eq!(tiles[1].left - tiles[0].right, tok::TILE_GAP);
        assert_eq!(tiles[0].bottom - tiles[0].top, (w * tok::TILE_RATIO).round());
        assert_eq!(h, tok::TILE_TOP + (w * tok::TILE_RATIO).round() + 6.0 + tok::TILE_NAME + tok::TILE_BOTTOM);
        assert!(tiles[2].right <= 696.0 - tok::ROW_PAD);
        // Short names: the least width; long ones: the widest.
        assert_eq!(mode_tiles(244.0, 696.0, 3, 20.0).0[0].right - 244.0 - tok::ROW_PAD, tok::TILE_MIN_W);
        assert_eq!(mode_tiles(0.0, 2000.0, 3, 400.0).0[0].right - tok::ROW_PAD, tok::TILE_MAX_W);
        // A narrow card: narrower tiles, all inside it.
        let (narrow, _) = mode_tiles(0.0, 300.0, 3, 100.0);
        assert!(narrow[2].right <= 300.0 - tok::ROW_PAD, "{narrow:?}");
        // Right to left: the first (Same as Windows) on the right.
        let m = Mirror { on: true, left: 244.0, right: 696.0 };
        let r0 = m.r(tiles[0]);
        assert_eq!(r0.right, 696.0 - tok::ROW_PAD);
        assert!(m.r(tiles[2]).left < r0.left);
        assert!(mode_tiles(0.0, 400.0, 0, 0.0).0.is_empty());
    }

    #[test]
    fn accent_dots_end_at_the_control_edge_and_mirror() {
        let n = crate::config::ACCENT_CUSTOM + 1;
        let xs = swatch_centers(600.0, n);
        assert_eq!(xs.len(), n);
        assert_eq!(xs[n - 1] + tok::SWATCH_D / 2.0, 600.0, "the last ends at the control's edge");
        assert_eq!(xs[1] - xs[0], tok::SWATCH_D + tok::SWATCH_GAP);
        assert_eq!(swatches_w(n), xs[n - 1] - xs[0] + tok::SWATCH_D);
        assert_eq!(swatches_w(0), 0.0);
        // Right to left: Automatic on the right.
        let m = Mirror { on: true, left: 244.0, right: 696.0 };
        assert!(m.x(xs[0]) > m.x(xs[n - 1]));
        // They fit the smallest window's row as dots (not the list).
        assert!(!choice_compact(swatches_w(n), 696.0 - 244.0));
    }

    #[test]
    fn the_keyboard_walks_the_accent_colors_but_not_into_custom() {
        use crate::config::ACCENT_CUSTOM;
        let n = ACCENT_CUSTOM + 1;
        // Stops: the color picked and "Custom…".
        assert!(swatch_focusable(3, 3));
        assert!(!swatch_focusable(4, 3));
        assert!(swatch_focusable(ACCENT_CUSTOM, 3));
        // Arrows: one along, never on to "Custom…"; Home / End the ends.
        assert_eq!(swatch_step(3, n, true, false), Some(4));
        assert_eq!(swatch_step(3, n, false, false), Some(2));
        assert_eq!(swatch_step(0, n, false, false), None);
        assert_eq!(swatch_step(n - 2, n, true, false), None, "the last preset");
        assert_eq!(swatch_step(4, n, true, true), Some(n - 2));
        assert_eq!(swatch_step(4, n, false, true), Some(0));
        // From a color of one's own: back to the presets, not further on.
        assert_eq!(swatch_step(ACCENT_CUSTOM, n, false, false), Some(n - 2));
        assert_eq!(swatch_step(ACCENT_CUSTOM, n, true, false), None);
        assert_eq!(swatch_step(0, 1, true, false), None);
    }

    #[test]
    fn an_info_hit_is_larger_than_its_glyph() {
        let r = info_hit(100.0, 50.0);
        assert!(r.right - r.left >= tok::INFO_D + 4.0 && r.bottom - r.top >= tok::INFO_D + 4.0);
        assert_eq!(((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0), (100.0, 50.0));
        // It never reaches the switch at the row's end: the label leaves
        // its room (INFO_W) before the control's gap.
        const { assert!(tok::INFO_W >= tok::INFO_D + tok::INFO_GAP) };
    }
}
