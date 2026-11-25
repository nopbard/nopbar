//! The search box at the top of the sidebar: typing lists the rows (and
//! pages) whose label, other names or description match, in place of the
//! pages, under their count; the list scrolls (wheel, arrow keys) when they
//! do not all fit. While typing, the page of the best match is shown
//! narrowed to the matching rows (model::page_filter), with "Show all" over
//! it. Picking a result opens its page whole with the row scrolled into
//! view and lit for a moment. The text field is our own: WM_CHAR for
//! typing, the IME's composition drawn in place (Korean matches while a
//! syllable is still being typed). Typing, the IME and a screen reader's
//! SetValue all end in search_changed. The page shown while typing is
//! only a preview: nothing being edited ends (an undo stays) until a
//! result is picked.

use windows::core::Result;
use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ROUNDED_RECT};
use windows::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmNotifyIME, ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow, CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT,
    COMPOSITIONFORM, CPS_CANCEL, GCS_COMPSTR, GCS_RESULTSTR, HIMC, IME_COMPOSITION_STRING, NI_COMPOSITIONSTR,
};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::model::{self, Found};
use super::rows::FLASH_TIMER;
use super::{App, Hit, win_h, SIDEBAR};
use crate::glyphs::Glyph;
use crate::i18n::t;

/// The box, in the sidebar under the app's name.
pub(super) const BOX: D2D_RECT_F = D2D_RECT_F { left: 12.0, top: 14.0, right: SIDEBAR - 12.0, bottom: 44.0 };
/// Where the pages (or the results) start.
pub(super) const LIST_TOP: f32 = 58.0;
const RESULT_H: f32 = 44.0;
/// The line with the number of results, over them.
const COUNT_H: f32 = 22.0;

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// The longest query kept (a short one is all it takes).
pub(super) const QUERY_MAX: usize = 60;

/// Pure: `text` as the box keeps it (at most QUERY_MAX characters).
pub(super) fn clip_query(text: &str) -> String {
    text.chars().take(QUERY_MAX).collect()
}

/// Pure: the page the search previews: the best match's (`best`), when it
/// is another page that exists. Tested.
pub(super) fn preview_page(current: usize, best: Option<usize>, pages: usize) -> Option<usize> {
    best.filter(|&b| b != current && b < pages)
}

/// The first result listed so that result `k` is in the `fit` shown
/// (`first`: the one listed now).
pub(super) fn keep_listed(k: usize, first: usize, fit: usize) -> usize {
    if k < first {
        k
    } else if fit > 0 && k >= first + fit {
        k + 1 - fit
    } else {
        first
    }
}

impl App {
    /// What is typed, with the syllable still being composed.
    pub(super) fn query(&self) -> String {
        format!("{}{}", self.search, self.search_comp)
    }

    /// The search is on (results listed instead of the pages).
    pub(super) fn searching(&self) -> bool {
        !self.search.is_empty() || !self.search_comp.is_empty()
    }

    pub(super) fn results(&self) -> Vec<Found> {
        model::search(&self.sections, &self.query())
    }

    /// How many results the sidebar has room for at once (the list
    /// scrolls through the rest).
    pub(super) fn results_fit() -> usize {
        ((win_h() - 12.0 - LIST_TOP - COUNT_H) / (RESULT_H + 2.0)).floor() as usize
    }

    /// The wheel at `x` (window DIPs): over the results it scrolls them.
    /// False: not over them.
    pub(super) fn results_wheel(&mut self, x: f32, delta: f32) -> bool {
        let dx = self.side_dx();
        if !self.searching() || self.welcome.is_some() || x < dx || x >= dx + SIDEBAR || self.list_open() {
            return false;
        }
        let n = self.results().len();
        let fit = Self::results_fit();
        let max = n.saturating_sub(fit);
        let step = (delta / 48.0).round() as isize;
        let first = (self.results_first as isize - step).clamp(0, max as isize) as usize;
        if first != self.results_first {
            self.results_first = first;
            self.redraw();
        }
        true
    }

    /// The box: magnifier, the text (or its placeholder), a caret while it
    /// has the keyboard, × to clear.
    pub(super) unsafe fn paint_search_box(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>) -> Result<()> {
        let rtl = crate::i18n::rtl();
        let focused = self.focus == Some(Hit::Search);
        let r = BOX;
        // A tone step, deeper while it has the keyboard (no outline; high
        // contrast outlines it in Highlight then).
        self.fill_round(dc, self.ctl_fill(if focused { crate::theme::State::Hover } else { crate::theme::State::Rest }), r, self.ctl_r())?;
        if let (true, Some(h)) = (focused, self.hc) {
            let b = dc.CreateSolidColorBrush(&super::rgba(h.highlight), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 1.0, r.top + 1.0, r.right - 1.0, r.bottom - 1.0), radiusX: self.ctl_r() - 1.0, radiusY: self.ctl_r() - 1.0 }, &b, 2.0, None);
        }
        hits.push((r, Hit::Search));
        let cy = (r.top + r.bottom) / 2.0;
        let glyph = |g: Glyph, x: f32| -> Result<()> { self.glyph(dc, g, 12.0, x, cy, self.dim()) };
        // Magnifier on the leading side, × on the trailing one.
        let (mag_x, clear_x) = if rtl { (r.right - 16.0, r.left + 16.0) } else { (r.left + 16.0, r.right - 16.0) };
        glyph(Glyph::Search, mag_x)?;
        let has = self.searching();
        if has {
            let c = rect(clear_x - 12.0, r.top + 4.0, clear_x + 12.0, r.bottom - 4.0);
            if self.hover == Some(Hit::SearchClear) {
                self.fill_round(dc, self.wash(0.1), c, self.ctl_r())?;
            }
            glyph(Glyph::Close, clear_x)?;
            hits.push((c, Hit::SearchClear));
        }
        let (tl, tr) = (r.left + 30.0, r.right - 30.0);
        let room = tr - tl;
        dc.PushAxisAlignedClip(&rect(tl, r.top, tr, r.bottom), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let caret = if !has {
            // A long translation ends in an ellipsis, not cut at the edge.
            let p = self.ellipsized(t("settings.search.placeholder"), super::rows::tok::BODY, false, room);
            let w = self.text_w(&p, super::rows::tok::BODY, false);
            self.label(dc, &p, if rtl { tr - w } else { tl }, self.text_top(cy, super::rows::tok::BODY), super::rows::tok::BODY, false, self.dim())?;
            if rtl { tr } else { tl }
        } else {
            // One run (the composition at its logical end, underlined).
            // Left to right the end stays in view as it grows; right to
            // left the text hugs the right and ends on the left.
            let all = self.query();
            let total = self.text_w(&all, super::rows::tok::BODY, false);
            let w_comp = self.text_w(&self.search_comp, super::rows::tok::BODY, false);
            let x0 = if total > room {
                if rtl { tl } else { tl - (total - room + 2.0) }
            } else if rtl {
                tr - total
            } else {
                tl
            };
            self.label(dc, &all, x0, self.text_top(cy, super::rows::tok::BODY), super::rows::tok::BODY, false, self.ink())?;
            if !self.search_comp.is_empty() {
                let u = if rtl { rect(x0, cy + 9.0, x0 + w_comp, cy + 10.0) } else { rect(x0 + total - w_comp, cy + 9.0, x0 + total, cy + 10.0) };
                self.fill_round(dc, self.ink(), u, 0.0)?;
            }
            if rtl { x0 } else { x0 + total }
        };
        if focused {
            self.fill_round(dc, self.ink(), rect(caret, cy - 8.0, caret + 1.0, cy + 8.0), 0.0)?;
        }
        self.search_caret.set(caret);
        dc.PopAxisAlignedClip();
        Ok(())
    }

    /// The results, where the pages are listed otherwise.
    pub(super) unsafe fn paint_results(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>) -> Result<()> {
        let rtl = crate::i18n::rtl();
        let found = self.results();
        let room = BOX.right - BOX.left - 8.0;
        if found.is_empty() {
            // What to do next, not only that nothing matched.
            self.para_dir(dc, t("settings.search.none"), BOX.left + 4.0, LIST_TOP + 6.0, super::rows::tok::BODY, false, room, self.dim(), rtl)?;
            return Ok(());
        }
        // How many, and which of them are listed when not all fit.
        let fit = Self::results_fit();
        let first = self.results_first.min(found.len().saturating_sub(fit));
        let count = if found.len() > fit {
            crate::i18n::tf("settings.search.count_more", &[("n", &found.len().to_string()), ("from", &(first + 1).to_string()), ("to", &(first + fit).min(found.len()).to_string())])
        } else {
            crate::i18n::tf("settings.search.count", &[("n", &found.len().to_string())])
        };
        self.text_box(dc, &count, BOX.left + 4.0, LIST_TOP + 2.0, super::rows::tok::FOOTNOTE, false, room, self.dim(), rtl)?;
        for (k, f) in found.iter().enumerate().skip(first).take(fit) {
            let y = LIST_TOP + COUNT_H + (k - first) as f32 * (RESULT_H + 2.0);
            let r = rect(BOX.left, y, BOX.right, y + RESULT_H);
            let hit = Hit::SearchResult(k);
            if self.hover == Some(hit) || (self.keyboard && self.focus == Some(hit)) {
                self.fill_round(dc, self.wash(0.06), r, self.ctl_r())?;
            }
            let Some(sec) = self.sections.get(f.section) else { continue };
            // A page of a tab with more than one: "Tab · Page".
            let alone = sec.title == sec.tab.title() || model::pages_of(&self.sections, sec.tab).len() < 2;
            let page_name = if alone { t(sec.tab.title()).to_string() } else { format!("{} \u{00B7} {}", t(sec.tab.title()), t(sec.title)) };
            let page = page_name.as_str();
            let row_text;
            let (top, sub) = match f.row.and_then(|ri| sec.rows.get(ri)) {
                Some(row) => {
                    row_text = t(row.label).to_string();
                    (row_text.as_str(), page)
                }
                None => (page, ""),
            };
            let room = r.right - r.left - 20.0;
            if sub.is_empty() {
                self.text_box(dc, top, r.left + 10.0, y + 12.0, super::rows::tok::BODY, false, room, self.ink(), rtl)?;
            } else {
                self.text_box(dc, top, r.left + 10.0, y + 5.0, super::rows::tok::BODY, false, room, self.ink(), rtl)?;
                self.text_box(dc, sub, r.left + 10.0, y + 23.0, super::rows::tok::FOOTNOTE, false, room, self.dim(), rtl)?;
            }
            hits.push((r, hit));
        }
        Ok(())
    }

    /// The search changed (typed, composed, set by a screen reader,
    /// cleared): the IME follows the caret, the list redraws from its
    /// first result, and the page of the best match is previewed narrowed
    /// to what matches (no motion: it follows the typing; nothing being
    /// edited ends, see page_change).
    pub(super) fn search_changed(&mut self) {
        self.results_first = 0;
        // The rows shown change: the widget row under the pointer may go.
        self.howto_hover = None;
        if self.searching() {
            let best = self.results().first().map(|f| f.section);
            if let Some(i) = preview_page(self.section, best, self.sections.len()) {
                // Only the view changes (see page_change).
                self.go_to_section(i, super::Nav::Preview);
                self.page_anim = None;
            }
            self.filtering = true;
            self.scroll = 0.0;
        } else {
            self.filtering = false;
        }
        self.redraw();
        self.place_ime();
    }

    /// The rows the page shown draws while the search narrows it (None:
    /// all of them).
    pub(super) fn filter_now(&self) -> Option<Vec<bool>> {
        if !self.filtering || self.welcome.is_some() || !self.searching() {
            return None;
        }
        model::page_filter(self.sections.get(self.section)?, &self.query())
    }

    /// Over a page the search narrows: what it shows, and "Show all".
    /// Returns where the page goes on.
    pub(super) unsafe fn paint_filter_note(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        let rtl = crate::i18n::rtl();
        let m = super::rows::Mirror { on: rtl, left, right };
        let label = t("settings.search.show_all");
        let bw = self.text_w(label, super::rows::tok::BODY, false) + 28.0;
        let text = crate::i18n::tf("settings.search.filtered", &[("q", self.query().trim())]);
        let room = (right - left - bw - 12.0).max(40.0);
        let th = self.para_h(&text, super::rows::tok::FOOTNOTE, room);
        let h = th.max(28.0);
        self.para_dir(dc, &text, m.span(left, room), y + (h - th) / 2.0, super::rows::tok::FOOTNOTE, false, room, self.dim(), rtl)?;
        let b = m.r(rect(right - bw, y + (h - 28.0) / 2.0, right, y + (h + 28.0) / 2.0));
        self.button(dc, b, label, self.hover == Some(Hit::FilterAll))?;
        if let Some(c) = super::clip_to(b, clip) {
            hits.push((c, Hit::FilterAll));
        }
        Ok(y + h + 14.0)
    }

    /// A typed character while the box has the keyboard. True if taken.
    pub(super) fn search_char(&mut self, ch: u16) -> bool {
        if self.focus != Some(Hit::Search) {
            return false;
        }
        match ch {
            // Backspace: the last character; Ctrl+Backspace: the last word.
            0x08 => {
                self.search.pop();
            }
            0x7F => {
                let keep = self.search.trim_end().rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
                self.search.truncate(keep);
            }
            // Other control characters (Enter, Esc, Tab) are keys.
            c if c < 0x20 => return true,
            0xD800..=0xDBFF => {
                self.surrogate = Some(ch);
                return true;
            }
            0xDC00..=0xDFFF => {
                if let Some(hi) = self.surrogate.take() {
                    if let Some(Ok(c)) = char::decode_utf16([hi, ch]).next() {
                        self.search.push(c);
                    }
                }
            }
            c => {
                if let Some(c) = char::from_u32(c as u32) {
                    // A short query is all it takes.
                    if self.search.chars().count() < QUERY_MAX {
                        self.search.push(c);
                    }
                }
            }
        }
        self.search_changed();
        true
    }

    /// The whole text set at once (a screen reader's SetValue): the same
    /// as typing it (search_changed).
    pub(super) fn set_search(&mut self, text: &str) {
        self.cancel_composition();
        self.search = clip_query(text);
        self.search_comp.clear();
        self.search_changed();
    }

    /// Clears the box (the pages come back).
    pub(super) fn clear_search(&mut self) {
        self.set_search("");
    }

    /// Keys while the box has the keyboard. None: not handled here (the
    /// character comes as WM_CHAR).
    pub(super) fn search_key(&mut self, vk: u16) -> Option<bool> {
        const ENTER: u16 = 0x0D;
        const ESC: u16 = 0x1B;
        const DOWN: u16 = 0x28;
        match vk {
            ESC if self.searching() => self.clear_search(),
            // Esc in an empty box: the keyboard leaves it (Esc again closes).
            ESC => {
                self.focus = None;
                self.redraw();
            }
            ENTER => {
                if let Some(f) = self.results().first().copied() {
                    self.jump(f);
                }
            }
            DOWN if self.searching() => {
                if !self.results().is_empty() {
                    self.focus = Some(Hit::SearchResult(0));
                    // The list may have been scrolled (wheel): back to the
                    // top, so the result focused is the one in sight.
                    self.results_first = keep_listed(0, self.results_first, Self::results_fit());
                    self.redraw();
                }
            }
            _ => return None,
        }
        Some(true)
    }

    /// Up / Down through every result (Up from the first: back to the
    /// box), Home / End to the first and last; the list scrolls to keep
    /// the focused one in sight.
    pub(super) fn result_key(&mut self, k: usize, vk: u16) -> Option<bool> {
        const HOME: u16 = 0x24;
        const END: u16 = 0x23;
        const UP: u16 = 0x26;
        const DOWN: u16 = 0x28;
        let n = self.results().len();
        let to = match vk {
            UP if k == 0 => {
                self.focus = Some(Hit::Search);
                self.redraw();
                return Some(true);
            }
            UP => k - 1,
            DOWN if k + 1 < n => k + 1,
            DOWN => return Some(true),
            HOME if n > 0 => 0,
            END if n > 0 => n - 1,
            _ => return None,
        };
        self.focus = Some(Hit::SearchResult(to));
        self.results_first = keep_listed(to, self.results_first, Self::results_fit());
        self.redraw();
        Some(true)
    }

    /// Opens a result: its page, the row scrolled into view and lit.
    pub(super) fn jump(&mut self, f: Found) {
        if f.section >= self.sections.len() {
            return;
        }
        let keyboard = self.keyboard;
        self.click(Hit::Section(f.section), 0.0);
        // A result picked: its page whole (the search stays in the box).
        self.filtering = false;
        self.flash = None;
        if let Some(ri) = f.row {
            // A row under a switch that is off is hidden: to that switch.
            let ri = match self.rows().get(ri) {
                Some(r) if ri > 0 && super::model::sub_row_off(&self.cfg, r.key) => ri - 1,
                _ => ri,
            };
            // Under "Advanced" or "More options": opened.
            self.open_fold_of(ri);
            self.flash = Some((f.section, ri, std::time::Instant::now()));
            self.reveal = Some(ri);
            unsafe {
                SetTimer(Some(self.hwnd), FLASH_TIMER, 30, None);
            }
            // From the keyboard: on to the row's control.
            if keyboard {
                self.focus = self.row_focus(ri);
            }
        }
        self.redraw();
    }

    /// The control of row `ri` as the keyboard focuses it.
    fn row_focus(&self, ri: usize) -> Option<Hit> {
        use super::model::Ctl;
        Some(match self.rows().get(ri).map(|r| &r.ctl)? {
            Ctl::Toggle { .. } => Hit::Toggle(ri),
            Ctl::Slider { .. } => Hit::Slider(ri, 0.0, 1.0),
            // Drawn as a list button when its options are too wide.
            Ctl::Choice { .. } if self.compact.borrow().contains(&ri) => Hit::Menu(ri),
            Ctl::Choice { get, .. } => Hit::Choice(ri, get(&self.cfg)),
            Ctl::Menu { .. } => Hit::Menu(ri),
            Ctl::Button { .. } => Hit::Button(ri),
            Ctl::Shortcut { .. } => Hit::Shortcut(ri),
            Ctl::Fold { .. } => Hit::Fold(ri),
            Ctl::Header => return None,
        })
    }

    /// The lit row's fade: one more frame, the timer stops when it is over.
    pub(super) fn flash_tick(&mut self) {
        if self.flash.is_none_or(|(_, _, t0)| t0.elapsed() >= super::rows::FLASH) {
            self.flash = None;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), FLASH_TIMER);
            }
        }
        self.redraw();
    }

    pub(super) fn ime_string(&self, himc: HIMC, what: IME_COMPOSITION_STRING) -> String {
        unsafe {
            let bytes = ImmGetCompositionStringW(himc, what, None, 0);
            if bytes <= 0 {
                return String::new();
            }
            let mut buf = vec![0u16; bytes as usize / 2 + 1];
            let got = ImmGetCompositionStringW(himc, what, Some(buf.as_mut_ptr() as *mut _), bytes as u32);
            if got <= 0 {
                return String::new();
            }
            buf.truncate(got as usize / 2);
            String::from_utf16_lossy(&buf)
        }
    }

    /// WM_IME_COMPOSITION: the finished text goes in, the part still being
    /// composed is shown (and searched). False: not ours (no box focus).
    pub(super) fn on_composition(&mut self, lp: LPARAM) -> bool {
        if self.focus != Some(Hit::Search) {
            return false;
        }
        let flags = lp.0 as u32;
        let himc = unsafe { ImmGetContext(self.hwnd) };
        if himc.is_invalid() {
            return false;
        }
        if flags & GCS_RESULTSTR.0 != 0 {
            let s = self.ime_string(himc, GCS_RESULTSTR);
            self.search.push_str(&s);
        }
        self.search_comp = if flags & GCS_COMPSTR.0 != 0 { self.ime_string(himc, GCS_COMPSTR) } else { String::new() };
        unsafe {
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
        self.search_changed();
        true
    }

    pub(super) fn end_composition(&mut self) {
        if !self.search_comp.is_empty() {
            self.search_comp.clear();
            self.redraw();
        }
    }

    /// A composition still open is dropped (the box lost the keyboard).
    pub(super) fn cancel_composition(&mut self) {
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if !himc.is_invalid() {
                let _ = ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                let _ = ImmReleaseContext(self.hwnd, himc);
            }
        }
        self.search_comp.clear();
    }

    /// Tells the IME where the caret is (its candidate list opens there).
    pub(super) fn place_ime(&self) {
        let s = self.scale;
        // A calendar field's caret while it has the keyboard, else the
        // search box's.
        let (x, top, bottom) = self.cal_caret().unwrap_or((self.search_caret.get() + self.side_dx(), BOX.top + 6.0, BOX.bottom - 6.0));
        let (x, top, bottom) = ((x * s) as i32, (top * s) as i32, (bottom * s) as i32);
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.is_invalid() {
                return;
            }
            let cf = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x, y: top }, rcArea: RECT::default() };
            let _ = ImmSetCompositionWindow(himc, &cf);
            let cand = CANDIDATEFORM { dwIndex: 0, dwStyle: CFS_EXCLUDE, ptCurrentPos: POINT { x, y: bottom }, rcArea: RECT { left: x, top, right: x + 1, bottom } };
            let _ = ImmSetCandidateWindow(himc, &cand);
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_best_match_on_another_page_is_previewed() {
        assert_eq!(preview_page(2, Some(5), 10), Some(5));
        assert_eq!(preview_page(5, Some(5), 10), None, "already shown");
        assert_eq!(preview_page(2, None, 10), None, "nothing found");
        assert_eq!(preview_page(2, Some(10), 10), None, "no such page");
        assert_eq!(clip_query(&"a".repeat(80)).chars().count(), QUERY_MAX);
        assert_eq!(clip_query("언어"), "언어");
    }

    /// The function's body in `src` (from `fn name(` to the next `fn `).
    fn body<'a>(src: &'a str, from: &str) -> &'a str {
        let at = src.find(from).unwrap_or_else(|| panic!("{from} not found"));
        let rest = &src[at + from.len()..];
        let end = rest.find("\n    pub(super) fn ").or_else(|| rest.find("\n    fn ")).unwrap_or(rest.len());
        &rest[..end]
    }

    #[test]
    fn typing_the_ime_and_a_screen_reader_take_one_path() {
        // The page preview and the filter happen in search_changed only;
        // every way the text changes ends there.
        let src = include_str!("search_box.rs").replace("\r\n", "\n");
        for f in ["pub(super) fn search_char(", "pub(super) fn on_composition(", "pub(super) fn set_search("] {
            assert!(body(&src, f).contains("self.search_changed();"), "{f} goes through search_changed");
        }
        assert!(body(&src, "pub(super) fn clear_search(").contains("self.set_search(\"\")"));
        // UIA ValuePattern.SetValue on the box: set_search, nothing else.
        let access = include_str!("access.rs").replace("\r\n", "\n");
        let arm = body(&access, "Act::SetValue if h == Hit::Search => {");
        let arm = &arm[..arm.find("Act::SetValue if").unwrap_or(arm.len())];
        assert!(arm.contains("self.set_search(&text);"), "{arm}");
        assert!(!arm.contains("self.search =") && !arm.contains("clear_search"), "{arm}");
        // The preview is a page shown, not a page opened (that would end
        // an undo): no click on the section.
        let changed = body(&src, "pub(super) fn search_changed(");
        assert!(changed.contains("self.go_to_section(i, super::Nav::Preview)") && !changed.contains("Hit::Section"), "{changed}");
    }

    #[test]
    fn results_scroll_to_keep_the_focus_listed() {
        assert_eq!(keep_listed(0, 0, 10), 0);
        assert_eq!(keep_listed(9, 0, 10), 0);
        assert_eq!(keep_listed(10, 0, 10), 1, "one past the end: one down");
        assert_eq!(keep_listed(25, 0, 10), 16);
        assert_eq!(keep_listed(3, 5, 10), 3, "above: it is the first");
        assert_eq!(keep_listed(0, 7, 10), 0, "into the results from the box: the list starts at the top");
        // The window lists fewer than every result a wide query finds.
        assert!(App::results_fit() >= 5 && App::results_fit() < 20);
    }

    #[test]
    fn a_wide_query_finds_more_than_fit() {
        // Every result is reachable now: a one-letter query finds more
        // than the list holds at once.
        let _l = crate::i18n::test_lock("en");
        let secs = super::super::model::sections();
        assert!(super::super::model::search(&secs, "e").len() > App::results_fit());
        for k in ["settings.search.count", "settings.search.count_more", "settings.search.clear"] {
            assert!(crate::i18n::get(k).is_some(), "{k}");
        }
    }
}
