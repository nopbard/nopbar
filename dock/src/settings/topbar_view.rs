//! The Top bar page's picture of the bar: what shows on it with the
//! switches as they are (an item turned off is a faint outline; an Auto
//! item counts as shown, so it is a layout, not a live view). Each item
//! is a link to its row below (Hit::BarCell); the line under the picture
//! names the item under the mouse or the keyboard. One layout (`layout`) places
//! both the drawing and the links, and its parts never overlap.

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ELLIPSE, D2D1_ROUNDED_RECT};
use windows_numerics::Vector2;

use super::rows::Mirror;
use super::{clip_to, col, App, Hit};
use crate::config::Config;
use crate::glyphs::Glyph;

/// The bar's height in the picture, and the status icons' cell.
pub(super) const BAR_H: f32 = 36.0;
const CELL: f32 = 26.0;
/// Space kept between the bar's three parts (left, island, right).
const GAP: f32 = 6.0;
/// Sizes tried, roomiest first: (item pitch, item width, menus width,
/// island width).
const SIZES: [(f32, f32, f32, f32); 3] = [(28.0, 26.0, 84.0, 104.0), (25.0, 23.0, 66.0, 80.0), (22.0, 20.0, 48.0, 56.0)];
/// The island's least width; under it the island is left out.
const ISLAND_MIN: f32 = 40.0;

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// How an optional item looks in the picture.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Look {
    /// Three dots (virtual desktops).
    Dots,
    /// crate::i18n::t("topbar.preview.input") (the input indicator).
    Input,
}

/// The optional items on the right of the bar, left to right, with the
/// row they are set by.
const RIGHT: [(&str, Look); 2] = [
    ("topbar.show_desktops", Look::Dots),
    ("input.indicator", Look::Input),
];

/// Whether the item set by row `key` shows (an "Auto" item counts as shown).
pub(super) fn shown(c: &Config, key: &str) -> bool {
    match key {
        "topbar.show_app_menus" => c.topbar.show_app_menus,
        "topbar.island" => c.topbar.island,
        "topbar.show_desktops" => c.topbar.desktops_mode() != 2,
        "input.indicator" => c.input.indicator_mode() != 2,
        _ => false,
    }
}

/// Every row the picture links to (checked by the tests).
#[cfg(test)]
pub(super) fn linked_keys() -> Vec<&'static str> {
    let mut v = vec!["topbar.show_app_menus", "topbar.island"];
    v.extend(RIGHT.iter().map(|(k, _)| *k));
    v
}

/// Where the picture's parts go, as x spans (left, right) in left to right
/// order (mirrored when drawn right to left). Items that do not fit are
/// left out (`cells` keeps the rightmost ones: the input indicator first).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BarLayout {
    pub name_x: f32,
    pub menus: Option<(f32, f32)>,
    pub island: Option<(f32, f32)>,
    /// The status icons' centers.
    pub status: [f32; 3],
    pub clock_x: f32,
    /// (index into RIGHT, span).
    pub cells: Vec<(usize, (f32, f32))>,
}

/// Lays the bar out between `left` and `right`: the clock and status icons
/// from the right edge, the optional items left of them, the logo, name
/// and menus from the left edge, and the island in the space between
/// (centered on the bar when it can be). Shrinks the items first, then
/// leaves out the leftmost optional items, then the menus, then the
/// island: no two parts ever overlap.
pub(super) fn layout(left: f32, right: f32, name_w: f32, clock_w: f32) -> BarLayout {
    let clock_x = right - 12.0 - clock_w;
    let status = [clock_x - CELL * 0.5, clock_x - CELL * 1.5, clock_x - CELL * 2.5];
    let right_start = clock_x - CELL * 3.0 - GAP;
    let name_x = left + 32.0;
    let menus_l = name_x + name_w + 6.0;
    let mid = (left + right) / 2.0;
    let place = |n: usize, pitch: f32, cell: f32, menus: bool, mw: f32, iw: f32| -> Option<BarLayout> {
        let gap_l = if menus { menus_l + mw + GAP } else { name_x + name_w + GAP };
        let gap_r = if n == 0 { right_start } else { right_start - (n - 1) as f32 * pitch - cell - GAP };
        let room = gap_r - gap_l;
        if room < ISLAND_MIN {
            return None;
        }
        let w = iw.min(room);
        let c = mid.clamp(gap_l + w / 2.0, gap_r - w / 2.0);
        let cells = (0..n)
            .map(|k| {
                let r = right_start - k as f32 * pitch;
                (RIGHT.len() - 1 - k, (r - cell, r))
            })
            .collect();
        Some(BarLayout { name_x, menus: menus.then_some((menus_l, menus_l + mw)), island: Some((c - w / 2.0, c + w / 2.0)), status, clock_x, cells })
    };
    for (pitch, cell, mw, iw) in SIZES {
        if let Some(l) = place(RIGHT.len(), pitch, cell, true, mw, iw) {
            return l;
        }
    }
    let (pitch, cell, mw, iw) = SIZES[SIZES.len() - 1];
    for n in (0..RIGHT.len()).rev() {
        if let Some(l) = place(n, pitch, cell, true, mw, iw) {
            return l;
        }
    }
    if let Some(l) = place(0, pitch, cell, false, mw, iw) {
        return l;
    }
    BarLayout { name_x, menus: None, island: None, status, clock_x, cells: Vec::new() }
}

impl App {
    /// The picture in a card of its own from `y` (the bar, then the name
    /// of the item under the pointer); returns where the card ends.
    pub(super) unsafe fn paint_bar_view(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        let rtl = crate::i18n::rtl();
        let pad = super::rows::tok::ROW_PAD;
        // The bar with the same space all round it (the name of the item
        // under the pointer floats under it as a tip: bar_tip).
        let card = self.picture_card(dc, "topbar.view", y, pad + BAR_H + pad, left, right)?;
        let (y, left, right) = (card.top + pad, left + pad, right - pad);
        let m = Mirror { on: rtl, left, right };
        let bar = rect(left, y, right, y + BAR_H);
        let enabled = self.cfg.topbar.enabled;
        self.fill_round(dc, self.wash(if enabled { 0.10 } else { 0.04 }), bar, 8.0)?;
        let cy = y + BAR_H / 2.0;
        // Off: the bar is gone, the picture only faint.
        let ink = if enabled { self.ink() } else { self.dim() };
        let row_of = |key: &str| self.rows().iter().position(|r| r.key == key);
        let visible = bar.bottom > clip.top && bar.top < clip.bottom;
        // The same layout places the drawing and the links.
        let clock = "9:41";
        let cw = self.text_w(clock, super::rows::tok::BODY, false);
        let name_w = self.text_w("NopBar", super::rows::tok::BODY, true);
        let lay = layout(left, right, name_w, cw);
        let span = |(l, r): (f32, f32), h: f32| rect(l, cy - h / 2.0, r, cy + h / 2.0);
        // One optional item: lit if shown, a faint tone behind it if not
        // (an outline only in high contrast); a link to its row either way.
        let item = |r: D2D_RECT_F, key: &str, hits: &mut Vec<(D2D_RECT_F, Hit)>| -> Result<bool> {
            let on = enabled && shown(&self.cfg, key);
            let Some(ri) = row_of(key) else { return Ok(on) };
            let mr = m.r(r);
            let hot = self.hover == Some(Hit::BarCell(ri));
            if hot {
                self.fill_round(dc, self.wash(0.10), mr, 6.0)?;
            }
            if !on {
                match self.hc {
                    Some(h) => {
                        let b = dc.CreateSolidColorBrush(&super::rgba(h.text), None)?;
                        dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(mr.left + 0.5, mr.top + 0.5, mr.right - 0.5, mr.bottom - 0.5), radiusX: 6.0, radiusY: 6.0 }, &b, 1.0, None);
                    }
                    None if !hot => self.fill_round(dc, self.wash(0.06), mr, 6.0)?,
                    None => {}
                }
            }
            if visible {
                if let Some(c) = clip_to(mr, clip) {
                    hits.push((c, Hit::BarCell(ri)));
                }
            }
            Ok(on)
        };
        let faint = |on: bool| -> D2D1_COLOR_F { if on { ink } else { self.wash(0.30) } };

        // Left: the logo, the app's name and its menus.
        self.fill_round(dc, ink, m.r(rect(left + 12.0, cy - 5.0, left + 22.0, cy + 5.0)), 5.0)?;
        self.label(dc, "NopBar", m.span(lay.name_x, name_w), self.text_top(cy, super::rows::tok::BODY), super::rows::tok::BODY, true, ink)?;
        if let Some(menus) = lay.menus {
            let on = item(span(menus, CELL), "topbar.show_app_menus", hits)?;
            let bw = ((menus.1 - menus.0 - 16.0) / 3.0 - 6.0).max(4.0);
            for k in 0..3 {
                let x = menus.0 + 8.0 + k as f32 * (bw + 6.0);
                self.fill_round(dc, faint(on), m.r(rect(x, cy - 2.0, x + bw, cy + 2.0)), 2.0)?;
            }
        }

        // Middle: the Live Island.
        if let Some(island) = lay.island {
            let on = item(span(island, CELL), "topbar.island", hits)?;
            if on {
                let c = if self.hc.is_some() { ink } else { col(0.05, 0.05, 0.05, 0.92) };
                self.fill_round(dc, c, m.r(span((island.0 + 4.0, island.1 - 4.0), 22.0)), 11.0)?;
            }
        }

        // Right: the clock, the status icons, then the optional items.
        self.label(dc, clock, m.span(lay.clock_x, cw), self.text_top(cy, super::rows::tok::BODY), super::rows::tok::BODY, false, ink)?;
        // As the bar has them: Control Center, search, Wi-Fi.
        for (g, x) in [Glyph::Sliders, Glyph::Search, Glyph::Wifi(4)].into_iter().zip(lay.status) {
            self.glyph(dc, g, 13.0, m.x(x), cy, ink)?;
        }
        for (k, cell) in &lay.cells {
            let (key, look) = RIGHT[*k];
            let on = item(span(*cell, CELL), key, hits)?;
            let c = faint(on);
            let gx = m.x((cell.0 + cell.1) / 2.0);
            match look {
                Look::Input => {
                    let w = self.text_w(crate::i18n::t("topbar.preview.input"), super::rows::tok::FOOTNOTE, true);
                    self.label(dc, crate::i18n::t("topbar.preview.input"), gx - w / 2.0, cy - 8.0, super::rows::tok::FOOTNOTE, true, c)?;
                }
                Look::Dots => {
                    let b = dc.CreateSolidColorBrush(&c, None)?;
                    for k in [-1.0f32, 0.0, 1.0] {
                        dc.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: gx + k * 6.0, Y: cy }, radiusX: 2.2, radiusY: 2.2 }, &b);
                    }
                }
            }
        }
        Ok(card.bottom)
    }

    /// The item of the bar's picture under the mouse or the keyboard: its
    /// row and what to say about it (its name and whether it shows).
    pub(super) fn bar_caption(&self) -> Option<(usize, String)> {
        let ri = match (self.hover, self.focus) {
            (Some(Hit::BarCell(ri)), _) => ri,
            (_, Some(Hit::BarCell(ri))) if self.keyboard => ri,
            _ => return None,
        };
        let r = self.rows().get(ri)?;
        let on = self.cfg.topbar.enabled && shown(&self.cfg, r.key);
        Some((ri, format!("{} \u{00B7} {}", self.row_label(r), crate::i18n::t(if on { "settings.topbar.view.on" } else { "settings.topbar.view.off" }))))
    }

    /// Over everything (with the tips): bar_caption as a tip chip
    /// centered under its item (as drawn now: `hits`), inside the window.
    pub(super) unsafe fn paint_bar_tip(&self, dc: &ID2D1DeviceContext, hits: &[(D2D_RECT_F, Hit)]) -> Result<()> {
        let Some((ri, text)) = self.bar_caption() else { return Ok(()) };
        let Some(cell) = hits.iter().find(|(_, h)| *h == Hit::BarCell(ri)).map(|(r, _)| *r) else { return Ok(()) };
        let (w, h) = self.chip_size(&text);
        let x = ((cell.left + cell.right - w) / 2.0).clamp(8.0, (super::win_w() - 8.0 - w).max(8.0));
        let r = rect(x, cell.bottom + 6.0, x + w, cell.bottom + 6.0 + h);
        self.chip(dc, r, &text, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every part's span, mirrored right to left when `rtl`.
    fn spans(l: &BarLayout, left: f32, right: f32, name_w: f32, clock_w: f32, rtl: bool) -> Vec<(&'static str, (f32, f32))> {
        let mut v = vec![("logo", (left + 12.0, left + 22.0)), ("name", (l.name_x, l.name_x + name_w)), ("clock", (l.clock_x, l.clock_x + clock_w))];
        v.extend(l.status.iter().map(|c| ("status", (c - CELL / 2.0, c + CELL / 2.0))));
        v.extend(l.menus.map(|s| ("menus", s)));
        v.extend(l.island.map(|s| ("island", s)));
        v.extend(l.cells.iter().map(|(k, s)| (RIGHT[*k].0, *s)));
        v.into_iter().map(|(n, (a, b))| (n, crate::i18n::mirror::edges(rtl, a, b, left, right))).collect()
    }

    #[test]
    fn bar_parts_never_overlap() {
        // 524 DIP: the content's width in the settings window; then
        // narrower and wider ones, short and long names and clocks, both
        // reading directions.
        for width in [300.0, 360.0, 524.0, 700.0, 1000.0] {
            for (name_w, clock_w) in [(44.0, 26.0), (60.0, 44.0)] {
                for rtl in [false, true] {
                    let (left, right) = (228.0, 228.0 + width);
                    let l = layout(left, right, name_w, clock_w);
                    let s = spans(&l, left, right, name_w, clock_w, rtl);
                    for (i, (a, (al, ar))) in s.iter().enumerate() {
                        assert!(*al >= left - 0.01 && *ar <= right + 0.01 && al < ar, "{width} {rtl}: {a} {al}..{ar} outside");
                        for (b, (bl, br)) in &s[i + 1..] {
                            assert!(ar <= bl || br <= al, "{width} {name_w} {rtl}: {a} {al}..{ar} overlaps {b} {bl}..{br}");
                        }
                    }
                }
            }
        }
        // The settings window's width shows every part.
        let l = layout(228.0, 752.0, 50.0, 30.0);
        assert_eq!(l.cells.len(), RIGHT.len());
        assert!(l.menus.is_some() && l.island.is_some());
        // Narrow: the leftmost items go first, the input indicator stays.
        let l = layout(0.0, 300.0, 50.0, 30.0);
        assert!(l.cells.len() < RIGHT.len());
        if let Some((k, _)) = l.cells.first() {
            assert_eq!(*k, RIGHT.len() - 1);
        }
    }
}
