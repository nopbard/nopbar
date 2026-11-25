//! The pieces every page's own cards are made of, so they look and space
//! like the rows' (rows.rs): a heading over a card (small, dim, an "i"
//! after it), a card's row (its part of the rounded card, the hairline
//! over it), a picture card, and a button at a row's end. Each one takes
//! the space before it from the one rule (layout::gap_before) and logs
//! itself (layout.rs).

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;

use super::layout::{Block, CtlKind, Kind};
use super::rows::{tok, Mirror};
use super::{clip_to, App, Hit};

pub(super) fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// A row drawn control first (its button measures where the label stops),
/// read label first: the "i" pushed last (row_text's) goes before the hits
/// pushed since `from`, so Tab reaches it before the row's control, as on
/// every settings row (App::focusables takes the hits' order). Pure:
/// tested.
pub(super) fn info_first(hits: &mut Vec<(D2D_RECT_F, Hit)>, from: usize) {
    if let Some(k) = (from..hits.len()).rev().find(|&k| matches!(hits[k].1, Hit::Info(_))) {
        let h = hits.remove(k);
        hits.insert(from.min(k), h);
    }
}

impl App {
    /// A heading over the card that follows (the rows' headings' look:
    /// footnote semibold, dim, in line with the rows' labels), an "i"
    /// after it when `tip` (a tooltip id). Returns where its card starts.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn heading(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, key: &str, label: &str, tip: Option<usize>, clip: D2D_RECT_F) -> Result<f32> {
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left, right };
        let y = self.block_top(y, Block::Header);
        self.note(Kind::Header, key, m.r(rect(left, y, right, y + tok::HEADER_H)));
        let lx = left + tok::ROW_PAD;
        let room = right - lx - tok::ROW_PAD - if tip.is_some() { tok::INFO_W } else { 0.0 };
        let size = tok::HEADER_SIZE;
        let hy = y + tok::HEADER_H - size - 10.0;
        self.text_box(dc, label, m.span(lx, room), hy, size, true, room, self.dim(), rtl)?;
        if let Some(id) = tip {
            let cx = lx + self.text_w(label, size, true).min(room) + tok::INFO_GAP + tok::INFO_D / 2.0;
            self.paint_info(dc, hits, id, m.x(cx), hy + size * 0.67, clip)?;
        }
        Ok(y + tok::HEADER_H)
    }

    /// One row of a card, `h` high, from `y` (`first`: the card's first
    /// row - the space before the card is added - `last`: its last): its
    /// part of the card and the hairline over it. Returns its rectangle
    /// (unmirrored).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn card_row(&self, dc: &ID2D1DeviceContext, key: &str, y: f32, h: f32, left: f32, right: f32, first: bool, last: bool) -> Result<D2D_RECT_F> {
        let m = Mirror { on: crate::i18n::rtl(), left, right };
        let y = if first { self.block_top(y, Block::Card) } else { y };
        let r = rect(left, y, right, y + h);
        self.note(Kind::Seg { first, last }, key, m.r(r));
        self.card_segment(dc, m.r(r), self.card_r(), first, last)?;
        if !first {
            self.hairline(dc, m, left + tok::ROW_PAD, right - tok::ROW_PAD, y)?;
        }
        Ok(r)
    }

    /// A card of one row (card_row, first and last).
    pub(super) unsafe fn single_row(&self, dc: &ID2D1DeviceContext, key: &str, y: f32, h: f32, left: f32, right: f32) -> Result<D2D_RECT_F> {
        self.card_row(dc, key, y, h, left, right, true, true)
    }

    /// The label of a row in `r`: one line from the text inset, centered
    /// on the row's line `cy`, cut before `stop` (unmirrored x); with an
    /// "i" after it when `tip`.
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn row_text(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, key: &str, r: D2D_RECT_F, cy: f32, stop: f32, label: &str, tip: Option<usize>, clip: D2D_RECT_F) -> Result<()> {
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left: r.left, right: r.right };
        let lx = r.left + tok::ROW_PAD;
        let room = (stop - tok::ROW_PAD - lx - if tip.is_some() { tok::INFO_W } else { 0.0 }).max(20.0);
        let used = self.text_w(label, tok::LABEL_SIZE, false).min(room);
        self.note(Kind::Label, key, m.r(rect(lx, cy - 9.0, lx + used + if tip.is_some() { tok::INFO_W } else { 0.0 }, cy + 9.0)));
        self.text_box(dc, label, m.span(lx, room), self.text_top(cy, tok::LABEL_SIZE), tok::LABEL_SIZE, false, room, self.ink(), rtl)?;
        if let Some(id) = tip {
            self.paint_info(dc, hits, id, m.x(lx + used + tok::INFO_GAP + tok::INFO_D / 2.0), cy, clip)?;
        }
        Ok(())
    }

    /// A button at the end of a row (its right edge the row's inner edge
    /// `cr`, centered on `cy`), hit `hit`; returns its rectangle
    /// (unmirrored).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn row_button(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, m: Mirror, key: &str, cr: f32, cy: f32, label: &str, hit: Hit, clip: D2D_RECT_F) -> Result<D2D_RECT_F> {
        let w = self.text_w(label, tok::BODY, false) + 32.0;
        let r = rect(cr - w, cy - tok::CONTROL_H / 2.0, cr, cy + tok::CONTROL_H / 2.0);
        self.note(Kind::Ctl { kind: CtlKind::Button, cy }, key, m.r(r));
        self.button(dc, m.r(r), label, self.hover == Some(hit))?;
        if let Some(c) = clip_to(m.r(r), clip) {
            hits.push((c, hit));
        }
        Ok(r)
    }

    /// A picture's card (the top bar's, the screen corners'), `h` high
    /// from `y` (the space before it added); returns its rectangle.
    pub(super) unsafe fn picture_card(&self, dc: &ID2D1DeviceContext, key: &str, y: f32, h: f32, left: f32, right: f32) -> Result<D2D_RECT_F> {
        let y = self.block_top(y, Block::Card);
        let r = rect(left, y, right, y + h);
        self.note(Kind::Picture, key, r);
        self.card(dc, r)?;
        Ok(r)
    }
}
