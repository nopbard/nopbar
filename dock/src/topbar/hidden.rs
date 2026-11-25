//! Windows sent to the tray (crate::traywin: minimized and tucked) on the
//! top bar: ONE cell, next to the clock, however many there are, so it is
//! always reachable (any count; any screen width: optional cells give way
//! first, see mod.rs `overflow`; the bar at the top or the bottom). One window: its app's icon (the dock icon at the tray
//! icons' size, else a window glyph); click shows it. Several: the tray
//! glyph with their number; click (or right-click) lists them (a click on
//! one shows it), and Show all. No Close: a window in the tray is closed
//! by showing it first (crate::traywin). The tip names the window or the
//! count.
//! Bars that show the whole set (the main bar, or every bar with "Same on
//! every display") have it.
//!
//! `hidden_content` is the one function that draws the cell.

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NONE;
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};
use windows_numerics::Vector2;

use super::{with_bar_of, Cell, CompositionStretch, Style, TopBar, GLYPH_BOX, GLYPH_SIZE, TRAY_DIP};
use crate::glyphs::Glyph;

/// One-shot: the pointer rested on the cell long enough for its tip.
pub(super) const TIP_TIMER: usize = 8;
/// Space between the glyph and the count (DIP).
const COUNT_GAP: f32 = crate::theme::metrics::space::S1;
/// The count's size (the ramp's Footnote, tabular figures).
const COUNT_SIZE: f32 = crate::theme::ramp::FOOTNOTE.size;

/// Tabular figures on a whole layout (numbers keep their width); its
/// width (px).
unsafe fn tabular(gfx: &crate::gfx::Gfx, l: &IDWriteTextLayout, text: &str) -> f32 {
    if let Ok(typo) = gfx.dwrite().CreateTypography() {
        let feature = DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 };
        let range = DWRITE_TEXT_RANGE { startPosition: 0, length: text.encode_utf16().count() as u32 };
        let _ = typo.AddFontFeature(feature);
        let _ = l.SetTypography(&typo, range);
    }
    let mut m = DWRITE_TEXT_METRICS::default();
    let _ = l.GetMetrics(&mut m);
    m.widthIncludingTrailingWhitespace.ceil()
}

impl TopBar {
    /// The windows this bar shows (none on a bar without the whole set).
    pub(super) fn hidden_list(&self) -> Vec<crate::traywin::Entry> {
        if self.full() { crate::traywin::entries() } else { Vec::new() }
    }

    /// The list changed: the cell follows (made or dropped with the first
    /// or last window, redrawn otherwise).
    pub(super) fn refresh_hidden(&mut self) {
        let now = self.hidden_list();
        let same = now.len() == self.hidden.len() && now.iter().zip(&self.hidden).all(|(a, b)| a.hwnd == b.hwnd && a.title == b.title);
        if same {
            return;
        }
        let rebuild = now.is_empty() != self.hidden.is_empty();
        self.hidden = now;
        self.hidden_hover(None);
        if rebuild {
            let _ = self.build_slots();
        } else {
            self.update();
        }
    }

    /// The cell's cache key ("" = no cell).
    pub(super) fn hidden_key(&self) -> String {
        match self.hidden.as_slice() {
            [] => String::new(),
            [e] => format!("\u{0}hidden|1|{}|{}", e.hwnd, e.icon.is_some()),
            list => format!("\u{0}hidden|{}|{}", list.len(), self.scale),
        }
    }

    /// Puts the cell's look into slot `i` and returns its size (DIP): one
    /// window's app icon at the tray icons' size
    /// (else the window glyph), or the tray glyph with the number of
    /// windows in tabular figures. The one place the cell's look is set.
    pub(super) fn hidden_content(&mut self, i: usize) -> (f32, f32) {
        let v = self.slots[i].visual.clone();
        let one = match self.hidden.as_slice() {
            [] => return (0.0, 0.0),
            [e] => Some(e.icon.clone()),
            _ => None,
        };
        if let Some(icon) = one {
            if let Some(b) = icon {
                let _ = v.SetBrush(&b);
                let _ = v.SetSize(Vector2 { X: TRAY_DIP, Y: TRAY_DIP });
                return (TRAY_DIP, TRAY_DIP);
            }
            return match self.glyph_surface(Style::Glyph, Glyph::Window) {
                Some((b, w, h)) => {
                    let _ = v.SetBrush(&b);
                    let _ = v.SetSize(Vector2 { X: w, Y: h });
                    (w, h)
                }
                None => (0.0, 0.0),
            };
        }
        let n = self.hidden.len().to_string();
        let s = self.scale;
        let none = (0.0, 0.0);
        let Ok((layout, _, th)) = self.gfx.text_layout_tabular(&n, &self.theme.font.family, COUNT_SIZE * s, DWRITE_FONT_WEIGHT_NORMAL) else { return none };
        let tw = unsafe { tabular(&self.gfx, &layout, &n) };
        let side = (GLYPH_BOX * s).round().max(1.0);
        let w_px = (side + COUNT_GAP * s + tw).ceil();
        // As tall as the bar: the count stands on the bar's one baseline
        // (topbar-text review P2), the glyph centered on whole pixels.
        let h_px = self.line_px().box_h;
        let ink = self.text_color();
        let g = Glyph::Tray;
        let ty = self.on_bar_line(&layout, th);
        let gy = super::baseline::centered_px(h_px, side);
        let Ok(surface) = self.gfx.draw(w_px as u32, h_px as u32, |dc, ox, oy| unsafe {
            super::paint_glyph_cell(dc, g, ox, oy + gy, side, GLYPH_SIZE * s, ink)?;
            let b = dc.CreateSolidColorBrush(&ink, None)?;
            crate::gfx::draw_text(dc, Vector2 { X: ox + side + COUNT_GAP * s, Y: oy + ty }, &layout, &b, D2D1_DRAW_TEXT_OPTIONS_NONE);
            Ok(())
        }) else {
            return none;
        };
        let Ok(brush) = self.gfx.compositor.CreateSurfaceBrushWithSurface(&surface) else { return none };
        let _ = brush.SetStretch(CompositionStretch::Fill);
        let size = (w_px / s, h_px / s);
        let _ = v.SetBrush(&brush);
        let _ = v.SetSize(Vector2 { X: size.0, Y: size.1 });
        size
    }

    /// "Notes (hidden to the tray)", or "3 windows hidden to the tray"
    /// (screen readers, the tip).
    pub(super) fn hidden_name(&self) -> String {
        match self.hidden.as_slice() {
            [e] => crate::i18n::tf("topbar.hidden.tip", &[("title", &title_of(e))]),
            list => crate::i18n::tf("topbar.hidden.count", &[("n", &list.len().to_string())]),
        }
    }

    /// The pointer moved to bar x (None: it left or pressed): the tip
    /// waits, shows, or goes.
    pub(super) fn hidden_hover(&mut self, x: Option<f32>) {
        // No tip while a menu is up (7.4).
        let over = x.filter(|_| !crate::popup::is_tracking()).and_then(|x| self.hit(x)).is_some_and(|i| self.slots[i].cell == Cell::Hidden);
        if over == self.hidden_tip.is_some() {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIP_TIMER);
        }
        let was_up = self.hidden_tip.is_some_and(|t| t.1);
        if over {
            self.hidden_tip = Some((0, false));
            // The kit's wait; none while a tip closed a moment ago (6.3).
            match super::desktops::tip_start(true, super::desktops::tip_is_warm()) {
                super::desktops::TipStart::Now => self.hidden_tip_due(),
                _ => unsafe {
                    SetTimer(Some(self.hwnd), TIP_TIMER, super::desktops::TIP_DELAY_MS, None);
                },
            }
        } else {
            self.hidden_tip = None;
            if was_up {
                super::desktops::hide_tip();
            }
        }
    }

    pub(super) fn hidden_tip_due(&mut self) {
        if self.hidden_tip.is_none() {
            return;
        }
        let Some(i) = self.slot_of(Cell::Hidden) else { return };
        self.hidden_tip = Some((0, true));
        let r = self.slot_rect(i);
        let (at, up) = super::desktops::tip_anchor((r.left + r.right) / 2, r, (super::desktops::TIP_GAP * self.scale).round() as i32, self.cfg.at_bottom());
        super::desktops::show_tip_at(&self.gfx, &self.theme, self.scale, &self.hidden_name(), at, up, self.tip_bounds());
    }
}

fn title_of(e: &crate::traywin::Entry) -> String {
    if e.title.trim().is_empty() { crate::i18n::t("dock.menu.untitled").to_string() } else { e.title.clone() }
}

/// A click on the cell: one window comes back (focused once it is seen
/// visible); with several, the list.
pub(super) fn click(bar: HWND, i: usize) {
    let Some(list) = with_bar_of(bar, |b| {
        b.hidden_hover(None);
        b.hidden.iter().map(|e| e.hwnd()).collect::<Vec<HWND>>()
    }) else {
        return;
    };
    match list.as_slice() {
        [h] => {
            crate::traywin::restore(*h);
        }
        [] => {}
        _ => menu(bar, i),
    }
}

/// Pure: what menu id `id` asks for, with `n` windows listed (Show k,
/// Show all).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pick {
    Show(usize),
    ShowAll,
}

const SHOW_ALL: u32 = 9_000;

pub(super) fn pick(id: u32, n: usize) -> Option<Pick> {
    if id == SHOW_ALL {
        return Some(Pick::ShowAll);
    }
    let k = id.checked_sub(1)? as usize;
    (k < n).then_some(Pick::Show(k))
}

/// The list (outside the bars' borrow: it pumps messages): each window (a
/// click shows it), then Show all when there are several.
pub(super) fn menu(bar: HWND, i: usize) {
    use crate::popup::Item;
    let Some((list, gfx, theme, scale, at, cell)) = with_bar_of(bar, |b| {
        b.hidden_hover(None);
        (b.hidden.clone(), b.gfx.clone(), b.theme.clone(), b.scale, b.menu_at(&b.slot_rect(i)), b.slot_rect(i))
    }) else {
        return;
    };
    if list.is_empty() {
        return;
    }
    // The design kit's list: how many are there (a quiet heading), then
    // each window with a window glyph; Show all with the restore glyph.
    let mut items: Vec<Item> = vec![Item::info(crate::i18n::tf("topbar.hidden.count", &[("n", &list.len().to_string())]))];
    items.extend(list.iter().enumerate().map(|(k, e)| Item { glyph: Some(Glyph::Window), ..Item::new(k as u32 + 1, title_of(e)).literal() }));
    if list.len() > 1 {
        items.push(Item::sep());
        items.push(Item { glyph: Some(Glyph::Restore), ..Item::new(SHOW_ALL, crate::i18n::t("topbar.hidden.show_all")) });
    }
    // The cell stays lit while its list is up (6.2 "open"); the list
    // grows from the cell (popup::open_origin).
    let out = super::while_open(bar, Cell::Hidden, || {
        crate::popup::track(crate::popup::Request {
            gfx: &gfx,
            theme: &theme,
            items,
            at: POINT { x: at.x, y: at.y },
            align: super::menu_align(),
            scale,
            titles: crate::popup::Titles { anchor: Some(cell), ..Default::default() },
            on_slide: None,
            drag_ids: 0..0,
            on_space: None,
            on_pick: None,
            hangs: true,
        })
    });
    let crate::popup::Outcome::Picked(id) = out else { return };
    match pick(id, list.len()) {
        Some(Pick::Show(k)) => {
            crate::traywin::restore(list[k].hwnd());
        }
        Some(Pick::ShowAll) => crate::traywin::restore_many(&list.iter().map(|e| e.hwnd()).collect::<Vec<_>>()),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_name_one_window_each() {
        assert_eq!(pick(1, 3), Some(Pick::Show(0)));
        assert_eq!(pick(3, 3), Some(Pick::Show(2)));
        assert_eq!(pick(4, 3), None, "past the list");
        assert_eq!(pick(0, 3), None);
        assert_eq!(pick(SHOW_ALL, 3), Some(Pick::ShowAll));
        // Every listed window is reachable, however many there are.
        let n = crate::traywin::MAX_HIDDEN;
        for k in 0..n {
            assert_eq!(pick(k as u32 + 1, n), Some(Pick::Show(k)));
        }
    }
}
