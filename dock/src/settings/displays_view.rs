//! The Dock page's map of the displays: each display drawn where it is
//! (its place and size scaled down), labelled with the number Windows
//! gives it (crate::displays), the main one marked, its dock's edge shown.
//! With a dock on every display a click (or the arrow keys) picks a display
//! and the rows below set that display's dock ("Dock 2"); "Same as Dock 1"
//! on, they show the main dock's settings, dimmed. "Identify" shows the
//! numbers on the screens themselves (the dock draws them; this window
//! when no dock runs). The Pinned apps page has the same choice as a row
//! of buttons.
//!
//! The map is the displays' real arrangement and is never mirrored: right
//! to left, the display on the left stays on the left (the arrow keys move
//! by place, not by reading order); the title and the button mirror.

use windows::core::Result;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ROUNDED_RECT};

use super::model;
use super::rows::{tok, Mirror};
use super::{clip_to, App, Hit};
use crate::config::{DisplayMode, Position};
use crate::i18n::{t, tf};

/// The drawing's greatest height, the space between neighbours, the
/// least room a display gets for its number.
pub(super) const MAP_H: f32 = 128.0;
const GAP: f32 = 6.0;
/// The space around the drawing and the line under it.
const FOOT_PAD: f32 = 8.0;

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// The displays (screen px) placed in `left..right` from `top`, at most
/// `max_h` tall: to scale, centered, each drawn `GAP` smaller than its
/// share so neighbours never touch. Returns the boxes (same order) and
/// the height used.
pub(super) fn map_layout(screens: &[RECT], left: f32, right: f32, top: f32, max_h: f32) -> (Vec<D2D_RECT_F>, f32) {
    if screens.is_empty() || right <= left || max_h <= 0.0 {
        return (Vec::new(), 0.0);
    }
    let x0 = screens.iter().map(|r| r.left).min().unwrap_or(0) as f32;
    let y0 = screens.iter().map(|r| r.top).min().unwrap_or(0) as f32;
    let x1 = screens.iter().map(|r| r.right).max().unwrap_or(1) as f32;
    let y1 = screens.iter().map(|r| r.bottom).max().unwrap_or(1) as f32;
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let s = ((right - left) / w).min(max_h / h);
    let used_w = w * s;
    let ox = left + ((right - left) - used_w) / 2.0;
    let boxes = screens
        .iter()
        .map(|r| {
            let l = ox + (r.left as f32 - x0) * s;
            let t = top + (r.top as f32 - y0) * s;
            let rr = ox + (r.right as f32 - x0) * s;
            let b = top + (r.bottom as f32 - y0) * s;
            // Half the gap off each side (never inside out).
            let gx = (GAP / 2.0).min((rr - l) / 4.0);
            let gy = (GAP / 2.0).min((b - t) / 4.0);
            rect(l + gx, t + gy, rr - gx, b - gy)
        })
        .collect();
    (boxes, h * s)
}

/// Where the arrow keys go on the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The display next to `from` in direction `dir` (by place): the nearest
/// whose middle is that way, straight ahead counting most.
pub(super) fn neighbor(boxes: &[D2D_RECT_F], from: usize, dir: Dir) -> Option<usize> {
    let c = |r: &D2D_RECT_F| ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
    let (fx, fy) = c(boxes.get(from)?);
    boxes
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != from)
        .filter_map(|(i, r)| {
            let (x, y) = c(r);
            let (ahead, side) = match dir {
                Dir::Left => (fx - x, (y - fy).abs()),
                Dir::Right => (x - fx, (y - fy).abs()),
                Dir::Up => (fy - y, (x - fx).abs()),
                Dir::Down => (y - fy, (x - fx).abs()),
            };
            (ahead > 0.5).then_some((i, ahead + 2.0 * side))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

impl App {
    /// The displays are read again (the window opened, or they changed);
    /// the display picked stays picked if it is still there.
    pub(super) fn refresh_screens(&mut self) {
        let picked = self.screens.get(self.display_sel).map(|d| d.key.clone());
        self.screens = crate::displays::current();
        self.display_sel = picked.and_then(|k| self.screens.iter().position(|d| d.key == k)).unwrap_or(0);
        self.sync_editing();
        // The pick may have moved (a display went, another is main now).
        self.drop_stale_confirm();
    }

    /// A dock on every display, and more than one display: each has a dock
    /// of its own to set.
    pub(super) fn per_display(&self) -> bool {
        self.cfg.dock.display_mode() == DisplayMode::Every && self.screens.len() > 1
    }

    /// Tells the rows whose dock they set (model::editing).
    pub(super) fn sync_editing(&self) {
        // (The first-run flow's rows are always the main dock's.)
        let d = self.screens.get(self.display_sel).filter(|d| self.per_display() && !d.primary && self.welcome.is_none());
        model::edit_display(d.map(|d| model::Editing { key: d.key.clone(), name: d.name.clone() }));
    }

    /// The main display's number (Dock 1 is usually the main dock).
    pub(super) fn main_number(&self) -> u32 {
        self.screens.iter().find(|d| d.primary).map(|d| d.number).unwrap_or(1)
    }

    /// "Dock 2", or "Dock 1 (main)".
    pub(super) fn dock_name(&self, i: usize) -> String {
        let Some(d) = self.screens.get(i) else { return String::new() };
        let n = d.number.to_string();
        tf(if d.primary { "settings.dock.name_main" } else { "settings.dock.name" }, &[("n", &n)])
    }

    /// Display `i` is picked (map or Pinned apps buttons). Only with a dock
    /// on every display: otherwise there is one dock, the main one.
    pub(super) fn select_display(&mut self, i: usize) {
        if i < self.screens.len() && self.per_display() {
            if i != self.display_sel {
                // A question or an undo about another dock goes.
                self.same_undo = None;
                if self.confirm == Some(super::reset::Confirm::SameAsMain) {
                    self.confirm = None;
                }
            }
            self.display_sel = i;
            self.menu = None;
        }
        self.focus = Some(Hit::Display(i));
        self.sync_editing();
        self.redraw();
    }

    /// The row's text: "Same as Dock 1 (main)" carries the main number,
    /// the picked dock's heading its name, the shared rows' heading says
    /// they are every dock's (with a dock on every display).
    pub(super) fn row_label(&self, row: &model::Row) -> String {
        match row.key {
            model::SAME_AS_MAIN => tf(row.label, &[("n", &self.main_number().to_string())]),
            model::THIS_DOCK => tf("settings.group.dock_n", &[("dock", &self.dock_name(self.display_sel))]),
            model::ALL_DOCKS if self.per_display() && self.welcome.is_none() => t("settings.group.all_docks").to_string(),
            _ => t(row.label).to_string(),
        }
    }

    /// The row's grey line ("" = none).
    /// Row `row`'s description (its "i"'s tip, a screen reader's help):
    /// its hint, then what it also does set the way it is now
    /// (App::row_aside: an explanation, never drawn under the row). The
    /// widgets card's rows say what the dock measured (settings::widgets).
    pub(super) fn row_hint(&self, row: &model::Row) -> std::borrow::Cow<'static, str> {
        if !row.hint.is_empty() {
            if let Some(h) = self.widgets_hint(row.key) {
                return h;
            }
        }
        let hint = if row.hint.is_empty() { "" } else { t(row.hint) };
        match self.row_aside(row.key) {
            Some(more) if hint.is_empty() => more.into(),
            Some(more) => format!("{hint} {more}").into(),
            None => hint.into(),
        }
    }

    /// Rows not shown now: "Same as Dock 1" for the main dock, the picked
    /// dock's heading with one dock, the theme file with none, a row under
    /// a switch that is off (model::SUB_ROWS: like macOS, it comes with
    /// its switch; its value is kept).
    pub(super) fn row_hidden(&self, key: &str) -> bool {
        if self.filter_now().is_none() && (matches!(key,"companion.enabled" | "companion.visible") || (key.starts_with("chat.") && !key.ends_with("_mcp"))) {return true;}
        if model::sub_row_off(&self.cfg, key) {
            return true;
        }
        match key {
            model::SAME_AS_MAIN => model::editing().is_none(),
            model::THIS_DOCK => !self.per_display(),
            // Theme files: only once there is one besides the built-in theme.
            "theme" => model::theme_choices() < 2,
            _ => false,
        }
    }

    /// Rows shown but not settable now: a display's dock rows while it is
    /// "Same as Dock 1" (they show the main dock's settings).
    pub(super) fn row_locked(&self, key: &str) -> bool {
        !super::agent_notifications::installed(key) || (model::DISPLAY_ROWS.contains(&key) && model::editing().is_some() && !model::editing_own(&self.cfg))
    }

    /// Where display `i`'s dock is drawn on the map: its edge, and whether
    /// it only goes there with the pointer (one dock that follows it: drawn
    /// faint on every display). None: no dock there.
    fn dock_edge(&self, i: usize) -> Option<(Position, bool)> {
        let d = self.screens.get(i)?;
        match self.cfg.dock.display_mode() {
            DisplayMode::Every => {
                let key = (!d.primary).then_some(d.key.as_str());
                Some((self.cfg.dock.for_display(key).position, false))
            }
            DisplayMode::Follow => Some((self.cfg.dock.position, true)),
            DisplayMode::Main => d.primary.then_some((self.cfg.dock.position, false)),
        }
    }

    /// Whose settings display `i`'s dock has (shown under the map and read
    /// with the display).
    fn dock_status(&self, i: usize) -> &'static str {
        match self.screens.get(i) {
            Some(d) if d.primary => t("settings.dock.map.main_dock"),
            Some(d) if self.cfg.dock.per_display.contains_key(&d.key) => t("settings.dock.map.own"),
            _ => t("settings.dock.map.same"),
        }
    }

    /// The line under the map with a dock on every display: the dock
    /// picked, its display's name and whose settings it has (None: one
    /// dock; what the map means then is in the heading's "i").
    fn map_line(&self) -> Option<String> {
        if !self.per_display() {
            return None;
        }
        let i = self.display_sel;
        let mut s = self.dock_name(i);
        if let Some(d) = self.screens.get(i).filter(|d| !d.name.is_empty()) {
            s = format!("{s} \u{00B7} {}", d.name);
        }
        Some(format!("{s} \u{00B7} {}", self.dock_status(i)))
    }

    /// The map's "i": how the dock moves, or how to give each display its
    /// own (None with a dock on every display: the map says it).
    fn map_tip(&self) -> Option<usize> {
        match self.cfg.dock.display_mode() {
            _ if self.per_display() => None,
            DisplayMode::Follow => Some(super::tooltip::card_tip("settings.dock.map.follow")),
            _ => Some(super::tooltip::card_tip("settings.dock.map.hint")),
        }
    }

    /// What a screen reader says for display `i` on the map.
    pub(super) fn display_a11y_name(&self, i: usize) -> String {
        let Some(d) = self.screens.get(i) else { return String::new() };
        let mut parts = vec![self.dock_name(i)];
        if !d.name.is_empty() {
            parts.push(d.name.clone());
        }
        parts.push(format!("{} × {}", d.rect.right - d.rect.left, d.rect.bottom - d.rect.top));
        if self.per_display() {
            parts.push(self.dock_status(i).to_string());
        }
        crate::a11y::join(&parts)
    }

    /// The map's card from `y` (none with one display): "Displays" (its
    /// "i", Identify at the row's end), then the map and the dock picked;
    /// returns where the card ends.
    pub(super) unsafe fn paint_display_map(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        if self.screens.len() < 2 {
            return Ok(y);
        }
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left, right };
        let pick = self.per_display();
        // Its first row: the name, Identify at the end.
        let head = self.card_row(dc, "dock.map", y, tok::ROW_MIN, left, right, true, false)?;
        let cy = head.top + tok::ROW_MIN / 2.0;
        let from = hits.len();
        let b = self.row_button(dc, hits, m, "dock.map", right - tok::ROW_PAD, cy, t("settings.dock.identify"), Hit::Identify, clip)?;
        self.row_text(dc, hits, "dock.map", head, cy, b.left, t("settings.dock.map"), self.map_tip(), clip)?;
        super::cards::info_first(hits, from);
        // The map, then (a dock on every display) the one picked.
        let line = self.map_line();
        let froom = right - left - tok::ROW_PAD * 2.0;
        let line_h = line.as_ref().map(|s| self.para_h(s, tok::FOOTNOTE, froom) + FOOT_PAD).unwrap_or(0.0);
        let y = head.bottom;
        let (boxes, map_h) = map_layout(&self.screens.iter().map(|d| d.rect).collect::<Vec<_>>(), left + tok::ROW_PAD, right - tok::ROW_PAD, y + FOOT_PAD + 4.0, MAP_H);
        let bottom = y + FOOT_PAD + 4.0 + map_h + FOOT_PAD + line_h + FOOT_PAD;
        let card = self.card_row(dc, "dock.map.view", y, bottom - y, left, right, false, true)?;
        let visible = |r: &D2D_RECT_F| r.bottom > clip.top && r.top < clip.bottom;

        // The displays.
        let hc = self.hc;
        for (i, (b, d)) in boxes.iter().zip(&self.screens).enumerate() {
            let selected = pick && i == self.display_sel;
            let hot = self.hover == Some(Hit::Display(i));
            let (fill, edge, ink) = match hc {
                Some(h) => {
                    let bg = if selected { super::rgba(h.highlight) } else { super::rgba(h.window) };
                    let fg = if selected { super::rgba(h.highlight_text) } else { super::rgba(h.text) };
                    (bg, super::rgba(h.text), fg)
                }
                // The dock picked: the "on" tone; the others a tone step
                // (no outlines).
                None if selected => (self.on_tone(), D2D1_COLOR_F { a: 0.0, ..self.ink() }, self.on_ink()),
                None => (self.wash(if hot { 0.16 } else { 0.10 }), D2D1_COLOR_F { a: 0.0, ..self.ink() }, self.ink()),
            };
            self.fill_round(dc, fill, *b, self.ctl_r())?;
            if hc.is_some() {
                let br = dc.CreateSolidColorBrush(&edge, None)?;
                let thick = if selected { 2.0 } else { 1.0 };
                let inset = thick / 2.0;
                dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(b.left + inset, b.top + inset, b.right - inset, b.bottom - inset), radiusX: crate::theme::concentric(self.ctl_r(), inset), radiusY: crate::theme::concentric(self.ctl_r(), inset) }, &br, thick, None);
            }
            // Its dock: a bar along the edge it sits on (faint where it
            // only goes with the pointer).
            if let Some((pos, roaming)) = self.dock_edge(i) {
                let (bw, bh) = (b.right - b.left, b.bottom - b.top);
                let bar = match pos {
                    Position::Bottom => rect(b.left + bw * 0.3, b.bottom - 6.0, b.right - bw * 0.3, b.bottom - 3.0),
                    Position::Left => rect(b.left + 3.0, b.top + bh * 0.3, b.left + 6.0, b.bottom - bh * 0.3),
                    Position::Right => rect(b.right - 6.0, b.top + bh * 0.3, b.right - 3.0, b.bottom - bh * 0.3),
                };
                let a = if roaming && hc.is_none() { 0.25 } else { 0.55 };
                self.fill_round(dc, D2D1_COLOR_F { a: ink.a * a, ..ink }, bar, 1.5)?;
            }
            // The number, and "Main" under it when there is room.
            let size = ((b.bottom - b.top) * 0.42).clamp(11.0, 30.0);
            let num = d.number.to_string();
            let nw = self.text_w(&num, size, true);
            let main_h = if d.primary && b.bottom - b.top > size + 26.0 { 16.0 } else { 0.0 };
            let ny = (b.top + b.bottom) / 2.0 - (size * 1.33 + main_h) / 2.0;
            self.label(dc, &num, (b.left + b.right - nw) / 2.0, ny, size, true, ink)?;
            if main_h > 0.0 {
                let mt = t("settings.dock.main_tag");
                let mw = self.text_w(mt, super::rows::tok::CAPTION, false).min(b.right - b.left - 8.0);
                self.text_box(dc, mt, (b.left + b.right - mw) / 2.0, ny + size * 1.33, super::rows::tok::CAPTION, false, mw + 1.0, if hc.is_some() { ink } else { self.dim() }, false)?;
            } else if d.primary {
                // No room for the word: a dot by the number.
                let b2 = dc.CreateSolidColorBrush(&ink, None)?;
                dc.FillEllipse(&windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: (b.left + b.right + nw) / 2.0 + 5.0, Y: ny + size * 0.4 }, radiusX: 2.5, radiusY: 2.5 }, &b2);
            }
            if visible(b) {
                if let Some(c) = clip_to(*b, clip) {
                    hits.push((c, Hit::Display(i)));
                }
            }
        }

        if let Some(line) = &line {
            self.para_dir(dc, line, m.span(left + tok::ROW_PAD, froom), y + FOOT_PAD + 4.0 + map_h + FOOT_PAD, tok::FOOTNOTE, false, froom, self.dim(), rtl)?;
        }
        Ok(card.bottom)
    }

    /// The Pinned apps page's choice of dock (with a dock on every
    /// display): a button per display, then a line saying whose apps the
    /// list is when that dock takes the main one's.
    pub(super) unsafe fn paint_display_picker(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        if !self.per_display() {
            return Ok(y);
        }
        let rtl = crate::i18n::rtl();
        let m = Mirror { on: rtl, left, right };
        // A row: "Dock", its displays' buttons at the end (wrapping onto
        // more lines when they do not fit).
        let order = self.picker_order();
        let widths: Vec<f32> = order.iter().map(|&i| self.text_w(&self.dock_name(i), tok::BODY, i == self.display_sel) + 28.0).collect();
        let room = right - left - tok::ROW_PAD * 2.0 - 120.0;
        let mut lines = 1usize;
        let mut run = 0.0;
        for w in &widths {
            if run > 0.0 && run + w > room {
                lines += 1;
                run = 0.0;
            }
            run += w + 6.0;
        }
        let h = (lines as f32 * (tok::CONTROL_H + 6.0) - 6.0 + tok::ROW_VPAD * 2.0).max(tok::ROW_MIN);
        let row = self.single_row(dc, "pins.dock", y, h, left, right)?;
        let first_cy = row.top + if lines == 1 { h / 2.0 } else { tok::ROW_VPAD + tok::CONTROL_H / 2.0 };
        self.row_text(dc, hits, "pins.dock", row, first_cy, right - room, t("settings.section.dock"), None, clip)?;
        // Right-aligned lines of buttons, in reading order.
        let mut line_w: Vec<f32> = vec![0.0; lines];
        {
            let (mut li, mut run) = (0usize, 0.0);
            for w in &widths {
                if run > 0.0 && run + w > room {
                    li += 1;
                    run = 0.0;
                }
                run += w + 6.0;
                line_w[li] = run - 6.0;
            }
        }
        let (mut li, mut run) = (0usize, 0.0);
        for (k, &i) in order.iter().enumerate() {
            let w = widths[k];
            if run > 0.0 && run + w > room {
                li += 1;
                run = 0.0;
            }
            let x = right - tok::ROW_PAD - line_w[li] + run;
            let cy = first_cy + li as f32 * (tok::CONTROL_H + 6.0);
            let name = self.dock_name(i);
            let r = rect(x, cy - tok::CONTROL_H / 2.0, x + w, cy + tok::CONTROL_H / 2.0);
            run += w + 6.0;
            let mr = m.r(r);
            let selected = i == self.display_sel;
            match (self.hc, selected) {
                (Some(h), true) => self.fill_round(dc, super::rgba(h.highlight), mr, self.ctl_r())?,
                (Some(h), false) => {
                    let b = dc.CreateSolidColorBrush(&super::rgba(h.text), None)?;
                    dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(mr.left + 0.5, mr.top + 0.5, mr.right - 0.5, mr.bottom - 0.5), radiusX: self.ctl_r(), radiusY: self.ctl_r() }, &b, 1.0, None);
                }
                (None, true) => self.fill_round(dc, self.on_tone(), mr, self.ctl_r())?,
                (None, false) => self.fill_round(dc, self.ctl_fill(if self.hover == Some(Hit::Display(i)) { crate::theme::State::Hover } else { crate::theme::State::Rest }), mr, self.ctl_r())?,
            }
            let fg = match self.hc {
                Some(h) if selected => super::rgba(h.highlight_text),
                None if selected => self.on_ink(),
                _ => self.ink(),
            };
            self.note(super::layout::Kind::Button, &name, mr);
            self.label(dc, &name, mr.left + 14.0, self.text_top((mr.top + mr.bottom) / 2.0, tok::BODY), tok::BODY, false, fg)?;
            if r.bottom > clip.top && r.top < clip.bottom {
                if let Some(c) = clip_to(mr, clip) {
                    hits.push((c, Hit::Display(i)));
                }
            }
        }
        Ok(row.bottom)
    }

    /// The displays by number (the Pinned apps buttons' order).
    pub(super) fn picker_order(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.screens.len()).collect();
        v.sort_by_key(|&i| self.screens[i].number);
        v
    }

    /// The arrow keys on a display (map: by place; Pinned apps buttons:
    /// by number, in reading order). Some(true) when one was picked.
    pub(super) fn display_key(&mut self, i: usize, vk: u16) -> Option<bool> {
        const HOME: u16 = 0x24;
        const END: u16 = 0x23;
        const LEFT: u16 = 0x25;
        const UP: u16 = 0x26;
        const RIGHT: u16 = 0x27;
        const DOWN: u16 = 0x28;
        let on_pins = self.sections.get(self.section).is_some_and(|s| s.kind == model::Kind::Pins);
        let next = if on_pins {
            let order = self.picker_order();
            let at = order.iter().position(|&k| k == i)?;
            let rtl = crate::i18n::rtl();
            let fwd = match vk {
                RIGHT => !rtl,
                LEFT => rtl,
                DOWN => true,
                UP => false,
                HOME => return Some(self.pick_by_key(order[0])),
                END => return Some(self.pick_by_key(order[order.len() - 1])),
                _ => return None,
            };
            let to = if fwd { (at + 1).min(order.len() - 1) } else { at.saturating_sub(1) };
            order[to]
        } else {
            let rects: Vec<RECT> = self.screens.iter().map(|d| d.rect).collect();
            let (boxes, _) = map_layout(&rects, 0.0, 1000.0, 0.0, 1000.0);
            let dir = match vk {
                LEFT => Dir::Left,
                RIGHT => Dir::Right,
                UP => Dir::Up,
                DOWN => Dir::Down,
                HOME => return Some(self.pick_by_key(self.picker_order()[0])),
                END => return Some(self.pick_by_key(*self.picker_order().last()?)),
                _ => return None,
            };
            match neighbor(&boxes, i, dir) {
                Some(n) => n,
                // Nothing that way: the key does nothing (but is taken).
                None => return Some(true),
            }
        };
        Some(self.pick_by_key(next))
    }

    fn pick_by_key(&mut self, i: usize) -> bool {
        if self.per_display() {
            self.select_display(i);
        } else {
            self.focus = Some(Hit::Display(i));
            self.redraw();
        }
        true
    }

    /// The "Same as Dock 1" switch was used. Off: the dock gets its own
    /// settings again (the ones it had, if it had some this session).
    /// On: asked first, since its own settings and apps go.
    pub(super) fn toggle_same_as_main(&mut self, ri: usize) {
        let Some(e) = model::editing() else { return };
        let Some(model::Ctl::Toggle { set, .. }) = self.rows().get(ri).map(|r| &r.ctl) else { return };
        let set = *set;
        if model::same_as_main(&self.cfg) {
            match self.own_stash.remove(&e.key) {
                Some(own) => {
                    self.cfg.dock.per_display.insert(e.key, own);
                }
                None => set(&mut self.cfg, false),
            }
            self.same_undo = None;
            self.changed(ri);
        } else {
            self.confirm = Some(super::reset::Confirm::SameAsMain);
            self.confirm_target = self.picked_target();
            // The keyboard lands on the safe answer.
            self.focus = Some(Hit::ResetNo);
            self.redraw();
        }
    }

    /// "Use main dock settings" (the question's yes): its own settings go
    /// (kept for Undo, and for the switch turned off again).
    pub(super) fn same_as_main_yes(&mut self) {
        self.confirm = None;
        self.confirm_target = None;
        let Some(e) = model::editing() else { return };
        let Some(ri) = self.rows().iter().position(|r| r.key == model::SAME_AS_MAIN) else { return };
        let Some(model::Ctl::Toggle { set, .. }) = self.rows().get(ri).map(|r| &r.ctl) else { return };
        let set = *set;
        if let Some(own) = self.cfg.dock.per_display.get(&e.key).cloned() {
            self.own_stash.insert(e.key.clone(), own.clone());
            self.same_undo = Some((e.key, own));
        }
        set(&mut self.cfg, true);
        self.focus = Some(if self.same_undo.is_some() { Hit::SameUndo } else { Hit::Toggle(ri) });
        self.changed(ri);
    }

    /// "Undo": the dock's own settings and apps as they were.
    pub(super) fn same_as_main_undo(&mut self) {
        let Some((k, own)) = self.same_undo.take() else { return };
        self.own_stash.remove(&k);
        self.cfg.dock.per_display.insert(k, own);
        self.focus = self.rows().iter().position(|r| r.key == model::SAME_AS_MAIN).map(Hit::Toggle);
        self.touch("dock.per_display");
        self.send_preview();
    }

    /// Under "Same as Dock 1": its question, or "Undo" once done (for the
    /// dock picked). Returns where the rows go on.
    pub(super) unsafe fn paint_same_card(&self, dc: &ID2D1DeviceContext, hits: &mut Vec<(D2D_RECT_F, Hit)>, y: f32, left: f32, right: f32, clip: D2D_RECT_F) -> Result<f32> {
        let Some(e) = model::editing() else { return Ok(y) };
        let dock = self.dock_name(self.display_sel);
        if self.confirm == Some(super::reset::Confirm::SameAsMain) {
            let q = tf("settings.dock.same.confirm", &[("dock", &dock)]);
            return self.confirm_card(dc, hits, y, left, right, clip, &q, t("settings.dock.same.yes"));
        }
        match &self.same_undo {
            Some((k, _)) if *k == e.key => {
                let s = tf("settings.dock.same.done", &[("dock", &dock)]);
                self.notice_card(dc, hits, y, left, right, clip, &s, t("settings.undo"), Hit::SameUndo)
            }
            _ => Ok(y),
        }
    }

    /// "Identify": each display's number on it for a moment.
    pub(super) fn identify(&mut self) {
        self.client.identify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, w: i32, h: i32) -> RECT {
        RECT { left: l, top: t, right: l + w, bottom: t + h }
    }

    fn overlap(a: &D2D_RECT_F, b: &D2D_RECT_F) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    /// Arrangements people have: side by side, a laptop under a monitor,
    /// one turned upright, a big one with a small one, three in a row,
    /// displays left of and above the main one (negative coordinates).
    fn arrangements() -> Vec<Vec<RECT>> {
        vec![
            vec![r(0, 0, 1920, 1080), r(1920, 0, 2560, 1440)],
            vec![r(0, 0, 2560, 1440), r(640, 1440, 1280, 800)],
            vec![r(0, 0, 1920, 1080), r(-1080, -400, 1080, 1920)],
            vec![r(0, 0, 3840, 2160), r(3840, 1000, 1024, 768)],
            vec![r(-1920, 0, 1920, 1080), r(0, 0, 1920, 1080), r(1920, 0, 1920, 1080)],
            vec![r(0, 0, 1920, 1080), r(0, -1440, 2560, 1440)],
            vec![r(0, 0, 7680, 2160), r(7680, 0, 800, 600)],
        ]
    }

    #[test]
    fn map_fits_and_never_overlaps() {
        for screens in arrangements() {
            for (left, right) in [(228.0, 752.0), (0.0, 300.0), (28.0, 552.0)] {
                let (boxes, h) = map_layout(&screens, left, right, 100.0, MAP_H);
                assert_eq!(boxes.len(), screens.len());
                assert!(h <= MAP_H + 0.01 && h > 0.0, "{h}");
                for (i, a) in boxes.iter().enumerate() {
                    assert!(a.left >= left - 0.01 && a.right <= right + 0.01, "{screens:?}: {a:?} outside {left}..{right}");
                    assert!(a.top >= 100.0 - 0.01 && a.bottom <= 100.0 + h + 0.01, "{a:?} outside the height");
                    assert!(a.right > a.left && a.bottom > a.top, "{a:?} inside out");
                    for b in &boxes[i + 1..] {
                        assert!(!overlap(a, b), "{screens:?}: {a:?} overlaps {b:?}");
                    }
                }
            }
        }
        assert_eq!(map_layout(&[], 0.0, 100.0, 0.0, 100.0).0.len(), 0);
    }

    #[test]
    fn map_keeps_places_and_proportions() {
        // Side by side, the right one larger: it is on the right, larger,
        // and the drawing is centered.
        let (b, _) = map_layout(&[r(0, 0, 1920, 1080), r(1920, 0, 2560, 1440)], 0.0, 500.0, 0.0, MAP_H);
        assert!(b[0].right < b[1].left);
        assert!(b[1].bottom - b[1].top > b[0].bottom - b[0].top);
        let mid = (b[0].left + b[1].right) / 2.0;
        assert!((mid - 250.0).abs() < 1.0, "centered: {mid}");
        // A display above: drawn above.
        let (b, _) = map_layout(&[r(0, 0, 1920, 1080), r(0, -1440, 2560, 1440)], 0.0, 500.0, 0.0, MAP_H);
        assert!(b[1].bottom < b[0].top);
    }

    #[test]
    fn map_is_not_mirrored_right_to_left() {
        // The arrangement is physical: the layout does not depend on the
        // reading direction (only the title and button mirror), and the
        // arrow keys move by place.
        let _l = crate::i18n::test_lock("ar");
        assert!(crate::i18n::rtl());
        let screens = [r(-1920, 0, 1920, 1080), r(0, 0, 1920, 1080)];
        let (b, _) = map_layout(&screens, 0.0, 500.0, 0.0, MAP_H);
        assert!(b[0].right < b[1].left, "the left display stays on the left");
        assert_eq!(neighbor(&b, 1, Dir::Left), Some(0));
        assert_eq!(neighbor(&b, 0, Dir::Right), Some(1));
    }

    #[test]
    fn arrows_move_by_place() {
        // Three in a row, a laptop under the middle one.
        let screens = [r(0, 0, 1920, 1080), r(-1920, 0, 1920, 1080), r(1920, 0, 1920, 1080), r(320, 1080, 1280, 800)];
        let (b, _) = map_layout(&screens, 0.0, 800.0, 0.0, 400.0);
        assert_eq!(neighbor(&b, 0, Dir::Left), Some(1));
        assert_eq!(neighbor(&b, 0, Dir::Right), Some(2));
        assert_eq!(neighbor(&b, 0, Dir::Down), Some(3));
        assert_eq!(neighbor(&b, 3, Dir::Up), Some(0), "straight up, not to the side");
        assert_eq!(neighbor(&b, 1, Dir::Left), None, "nothing further left");
        assert_eq!(neighbor(&b, 0, Dir::Up), None);
        assert_eq!(neighbor(&b, 9, Dir::Up), None);
    }
}
