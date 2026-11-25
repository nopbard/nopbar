//! The Hot corners page's drawing of the screen, and the drop-down list of
//! Ctl::Menu rows (the corner actions are too many for a segmented choice).

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ELLIPSE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_NORMAL;
use windows_numerics::Vector2;

use super::{model, rgba, App, Hit, win_h, win_w};
use crate::config::{CornerAction, Position};
use crate::i18n::t;

/// An open list's items and padding: the menus' (popup's) padding; its
/// rows stay 32 until the settings' own batch (the menus' went to 28).
const ITEM_H: f32 = crate::theme::metrics::size::ROW_L;
const LIST_PAD: f32 = crate::popup::PAD_Y;
/// The drawing of the screen (DIPs).
const SCREEN_W: f32 = 300.0;
const SCREEN_H: f32 = 168.0;

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

/// Pure: where the color dialog starts: the accent saved as a color, else
/// the one in force (`now`, the window's "on").
fn accent_start(saved: &str, now: D2D1_COLOR_F) -> crate::theme::Rgba {
    let c = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    crate::theme::parse_color(saved.trim()).unwrap_or(crate::theme::Rgba { r: c(now.r), g: c(now.g), b: c(now.b), a: 255 })
}

/// Pure: what the dialog's answer sets: a new color's "#RRGGBB"; nothing
/// when cancelled or the same color again.
fn picked_accent(saved: &str, picked: Option<crate::theme::Rgba>) -> Option<String> {
    picked.map(crate::config::accent_hex).filter(|hex| hex != saved)
}

#[cfg(test)]
mod accent_pick_tests {
    use super::*;
    use crate::theme::Rgba;

    #[test]
    fn the_color_dialog_starts_from_the_accent_and_sets_only_a_new_color() {
        let blue = D2D1_COLOR_F { r: 0.0, g: 120.0 / 255.0, b: 212.0 / 255.0, a: 1.0 };
        // Saved as a color: from it; "system" / a name: from the one in force.
        assert_eq!(accent_start("#FFD60A", blue), Rgba { r: 0xFF, g: 0xD6, b: 0x0A, a: 255 });
        assert_eq!(accent_start("system", blue), Rgba { r: 0, g: 0x78, b: 0xD4, a: 255 });
        let yellow = Rgba { r: 0xFF, g: 0xD6, b: 0x0A, a: 255 };
        let hex = crate::config::accent_hex(yellow);
        assert_eq!(picked_accent("system", Some(yellow)), Some(hex.clone()));
        assert_eq!(picked_accent(&hex, Some(yellow)), None, "the same color: nothing to save");
        assert_eq!(picked_accent("system", None), None, "cancelled: the config stays");
    }
}

impl App {
    pub(super) unsafe fn fill_round(&self, dc: &ID2D1DeviceContext, c: D2D1_COLOR_F, r: D2D_RECT_F, rad: f32) -> Result<()> {
        let b = dc.CreateSolidColorBrush(&c, None)?;
        dc.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }, &b);
        Ok(())
    }

    /// Text cut to `r` (never drawn outside it).
    unsafe fn text_clipped(&self, dc: &ID2D1DeviceContext, s: &str, r: D2D_RECT_F, size: f32, bold: bool, c: D2D1_COLOR_F) -> Result<()> {
        if r.right - r.left <= 4.0 {
            return Ok(());
        }
        dc.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        let out = self.label(dc, s, r.left, r.top, size, bold, c);
        dc.PopAxisAlignedClip();
        out.map(|_| ())
    }

    /// A small "v" centered on (x, cy).
    pub(super) unsafe fn chevron(&self, dc: &ID2D1DeviceContext, x: f32, cy: f32, c: D2D1_COLOR_F) -> Result<()> {
        self.glyph(dc, crate::glyphs::Glyph::ChevronDown, 10.0, x, cy, c)
    }

    /// Above the corner rows (Window management, under their heading): a
    /// drawing of the screen with each corner's action, in a card of its
    /// own; returns where the card ends.
    pub(super) unsafe fn paint_corners(&self, dc: &ID2D1DeviceContext, y: f32, left: f32, right: f32) -> Result<f32> {
        let inner = right - left;
        let pad = super::rows::tok::ROW_PAD;
        let card = self.picture_card(dc, "corners.view", y, pad + 6.0 + SCREEN_H + 6.0 + pad, left, right)?;
        let y = card.top + pad;
        // The corner rows follow the switch: row `first + k` is corner k.
        let first = self.rows().iter().position(|r| r.key == "hot_corners.top_left").unwrap_or(0);

        let (sx, sy) = (left + (inner - SCREEN_W) / 2.0, y + 6.0);
        let screen = rect(sx, sy, sx + SCREEN_W, sy + SCREEN_H);
        // Bezel, then the "wallpaper": achromatic tone steps (the accent is
        // for on and picked only: design B3), corners concentric.
        let bezel = 6.0;
        self.fill_round(dc, self.ctl_fill(crate::theme::State::Rest), rect(sx - bezel, sy - bezel, screen.right + bezel, screen.bottom + bezel), self.ctl_r() + bezel)?;
        let wall = match self.hc {
            Some(h) => rgba(h.window),
            None => self.track_fill(),
        };
        self.fill_round(dc, wall, screen, self.ctl_r())?;
        dc.PushAxisAlignedClip(&screen, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        // The top bar and the dock, where they are.
        let bar_h = if self.cfg.topbar.enabled { 9.0 } else { 0.0 };
        if bar_h > 0.0 {
            self.fill_round(dc, self.wash(0.20), rect(sx, sy, screen.right, sy + bar_h), 0.0)?;
        }
        let dock = match self.cfg.dock.position {
            Position::Bottom => rect(sx + SCREEN_W / 2.0 - 56.0, screen.bottom - 17.0, sx + SCREEN_W / 2.0 + 56.0, screen.bottom - 5.0),
            Position::Left => rect(sx + 5.0, sy + SCREEN_H / 2.0 - 42.0, sx + 17.0, sy + SCREEN_H / 2.0 + 42.0),
            Position::Right => rect(screen.right - 17.0, sy + SCREEN_H / 2.0 - 42.0, screen.right - 5.0, sy + SCREEN_H / 2.0 + 42.0),
        };
        self.fill_round(dc, self.wash(0.24), dock, 4.0)?;

        // The corners, in the rows' order. The one being edited is bigger.
        let editing = |k: usize| {
            let ri = first + k;
            self.menu.map(|(r, _)| r) == Some(ri) || self.hover == Some(Hit::Menu(ri)) || (self.keyboard && self.focus == Some(Hit::Menu(ri)))
        };
        let half = SCREEN_W / 2.0 - 30.0;
        for (ri, action) in self.cfg.hot_corners.all().into_iter().enumerate() {
            let (right_side, bottom) = (ri % 2 == 1, ri >= 2);
            let point = Vector2 { X: if right_side { screen.right } else { sx }, Y: if bottom { screen.bottom } else { sy } };
            let on = action != CornerAction::None;
            let rad = if editing(ri) { 22.0 } else { 15.0 };
            // On: the ink (no hue), off: a faint tone.
            let c = if on { self.ink() } else { self.wash(0.18) };
            let b = dc.CreateSolidColorBrush(&c, None)?;
            dc.FillEllipse(&D2D1_ELLIPSE { point, radiusX: rad, radiusY: rad }, &b);
            if !on {
                continue;
            }
            let idx = CornerAction::ALL.iter().position(|a| *a == action).unwrap_or(0);
            let name = t(model::CORNER_ACTIONS[idx]);
            let tw = self.gfx.text_layout(name, super::FONT, super::rows::tok::FOOTNOTE, DWRITE_FONT_WEIGHT_NORMAL).map(|l| l.1).unwrap_or(0.0).min(half);
            let x = if right_side { screen.right - 26.0 - tw } else { sx + 26.0 };
            let ty = if bottom { screen.bottom - 26.0 } else { sy + bar_h + 8.0 };
            self.text_clipped(dc, name, rect(x, ty, x + tw + 1.0, ty + 20.0), super::rows::tok::FOOTNOTE, false, self.ink())?;
        }
        dc.PopAxisAlignedClip();
        Ok(card.bottom)
    }

    /// A drop-down list under its button `anchor` (over it when there is
    /// no room below; scrolled when it fits neither way). `first`: the
    /// first item shown; `icons`: app icons (paths) left of the names.
    /// Returns how many items fit (for keeping the highlighted one in view).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn paint_list(
        &self,
        dc: &ID2D1DeviceContext,
        hits: &mut Vec<(D2D_RECT_F, Hit)>,
        anchor: D2D_RECT_F,
        labels: &[&str],
        icons: Option<&[String]>,
        cur: Option<usize>,
        hi: usize,
        first: usize,
        item_hit: &dyn Fn(usize) -> Hit,
        min_w: f32,
        rtl: bool,
    ) -> Result<usize> {
        let n = labels.len();
        let w = (anchor.right - anchor.left).max(min_w).min(win_w() - 16.0);
        let (top, shown) = list_place(anchor, n, win_h());
        let first = first.min(n - shown);
        let h = shown as f32 * ITEM_H + LIST_PAD * 2.0;
        let x = if rtl { anchor.left } else { anchor.right - w }.clamp(8.0, win_w() - 8.0 - w);
        // Any click outside closes it.
        hits.push((rect(0.0, 0.0, win_w(), win_h()), Hit::MenuDismiss));
        let r = rect(x, top, x + w, top + h);
        // The menus' surface, opaque (the dock's menus look the same).
        let bg = match self.hc {
            Some(h) => rgba(h.window),
            None => rgba(self.look.label_colors().0),
        };
        // The list floats at elevation E2, a real blur (no outline; high
        // contrast keeps the system's edge), like every menu.
        let rad = self.menu_r();
        crate::gfx::soft_shadows(dc, r, rad, &self.look.elevation(crate::theme::Elevation::E2))?;
        self.fill_round(dc, bg, r, rad)?;
        if let Some(h) = self.hc {
            let b = dc.CreateSolidColorBrush(&rgba(h.text), None)?;
            dc.DrawRoundedRectangle(&D2D1_ROUNDED_RECT { rect: rect(r.left + 0.5, r.top + 0.5, r.right - 0.5, r.bottom - 0.5), radiusX: rad, radiusY: rad }, &b, 1.0, None);
        }
        let scrolls = shown < n;
        let bar = if scrolls { 8.0 } else { 0.0 };
        for (k, oi) in (first..first + shown).enumerate() {
            let iy = top + LIST_PAD + k as f32 * ITEM_H;
            let ir = if rtl { rect(x + LIST_PAD + bar, iy, x + w - LIST_PAD, iy + ITEM_H) } else { rect(x + LIST_PAD, iy, x + w - LIST_PAD - bar, iy + ITEM_H) };
            let hit = item_hit(oi);
            let lit = self.hover == Some(hit) || (self.keyboard && oi == hi);
            let mut fg = self.ink();
            // Concentric with the list (12 - 4 = 8), the menus' row fills.
            let item_r = crate::theme::concentric(self.menu_r(), LIST_PAD);
            if lit {
                if self.hc.is_some() {
                    self.fill_round(dc, self.accent, ir, item_r)?;
                    fg = self.on_accent();
                } else {
                    let st = if self.pressed == Some(hit) { crate::theme::State::Pressed } else { crate::theme::State::Hover };
                    self.fill_round(dc, self.row_fill(st), ir, item_r)?;
                }
            }
            // The current option: a check before it (the Mac's way).
            let on = cur == Some(oi);
            if on {
                let cx = if rtl { ir.right - 13.0 } else { ir.left + 13.0 };
                self.glyph(dc, crate::glyphs::Glyph::Check, 12.0, cx, iy + ITEM_H / 2.0, fg)?;
            }
            // The text's line centered on the item (measured, not a fixed
            // offset).
            let ty = self.text_top(iy + ITEM_H / 2.0, super::rows::tok::BODY);
            let mut tr = rect(ir.left + 26.0, ty, ir.right - 8.0, iy + ITEM_H);
            if rtl {
                tr = rect(ir.left + 8.0, ty, ir.right - 26.0, iy + ITEM_H);
            }
            if let Some(path) = icons.and_then(|i| i.get(oi)) {
                let s = 20.0;
                let ix = if rtl { tr.right - s } else { tr.left };
                self.draw_icon(dc, path, rect(ix, iy + (ITEM_H - s) / 2.0, ix + s, iy + (ITEM_H + s) / 2.0))?;
                if rtl {
                    tr.right -= s + 8.0;
                } else {
                    tr.left += s + 8.0;
                }
            }
            // The check marks the current one (never a bolder name).
            self.text_box(dc, labels[oi], tr.left, tr.top, super::rows::tok::BODY, false, tr.right - tr.left, fg, rtl)?;
            hits.push((ir, hit));
        }
        if scrolls {
            let track = h - LIST_PAD * 2.0 - 4.0;
            let th = (track * shown as f32 / n as f32).max(16.0);
            let ty = top + LIST_PAD + 2.0 + (track - th) * first as f32 / (n - shown) as f32;
            let bx = if rtl { x + 4.0 } else { x + w - 7.0 };
            self.fill_round(dc, self.faint(), rect(bx, ty, bx + 3.0, ty + th), 1.5)?;
        }
        Ok(shown)
    }

    /// First item to show so that `hi` is in view (`shown` fit, from `first`).
    pub(super) fn keep_in_view(hi: usize, first: usize, shown: usize) -> usize {
        if hi < first {
            hi
        } else if shown > 0 && hi >= first + shown {
            hi + 1 - shown
        } else {
            first
        }
    }

    /// Option `oi` of list row `ri` picked: set it (shown on the dock right
    /// away), close the list, keep the keyboard on its button.
    pub(super) fn pick_menu(&mut self, ri: usize, oi: usize) {
        self.menu = None;
        self.focus = Some(Hit::Menu(ri));
        // The accent's "Custom…": Windows' color dialog, from the color in
        // force (picked again: a new color). It runs its own message loop:
        // shown from a posted message, outside this borrow of the app, so
        // what comes meanwhile (Windows' look changing) is not dropped
        // (WM_PICK_ACCENT: accent_start, then accent_picked).
        if oi == crate::config::ACCENT_CUSTOM && self.is_accent_row(ri) {
            self.redraw();
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(self.hwnd), super::WM_PICK_ACCENT, windows::Win32::Foundation::WPARAM(ri), windows::Win32::Foundation::LPARAM(0));
            }
            return;
        }
        if let Some((opts, get, set)) = self.row_ctl(ri).and_then(|c| c.options()) {
            let n = opts.len();
            if oi < n && get(&self.cfg) != oi {
                set(&mut self.cfg, oi);
                self.changed(ri);
                return;
            }
        }
        self.redraw();
    }

    /// Accent color `oi` of row `ri` picked (Appearance's colors): the
    /// same as its list's option (pick_menu), the keyboard staying on it.
    pub(super) fn pick_swatch(&mut self, ri: usize, oi: usize) {
        self.menu = None;
        self.focus = Some(Hit::Swatch(ri, oi));
        if oi == crate::config::ACCENT_CUSTOM && self.is_accent_row(ri) {
            self.redraw();
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(self.hwnd), super::WM_PICK_ACCENT, windows::Win32::Foundation::WPARAM(ri), windows::Win32::Foundation::LPARAM(0));
            }
            return;
        }
        if let Some((opts, get, set)) = self.row_ctl(ri).and_then(|c| c.options()) {
            if oi < opts.len() && get(&self.cfg) != oi {
                set(&mut self.cfg, oi);
                self.changed(ri);
                return;
            }
        }
        self.redraw();
    }

    fn is_accent_row(&self, ri: usize) -> bool {
        self.rows().get(ri).is_some_and(|r| r.key == "theme.accent")
    }

    /// WM_PICK_ACCENT, before the dialog: where it starts (copied: the
    /// app is not borrowed while it is open).
    pub(super) fn accent_start(&self) -> crate::theme::Rgba {
        accent_start(&self.cfg.theme.accent, self.accent)
    }

    /// WM_PICK_ACCENT, after the dialog (`picked`: None, cancelled) for
    /// row `ri`: Windows' look is read again first (it may have changed
    /// while the dialog was open, high contrast among it), then a new
    /// color is set and shown.
    pub(super) fn accent_picked(&mut self, ri: usize, picked: Option<crate::theme::Rgba>) {
        let sys = crate::syslook::current();
        if sys != self.look_for.1 {
            self.refresh_look(sys);
        }
        if self.is_accent_row(ri) {
            if let Some(hex) = picked_accent(&self.cfg.theme.accent, picked) {
                self.cfg.theme.accent = hex;
                self.changed(ri);
                return;
            }
        }
        self.redraw();
    }

    /// Keys while a list is open: arrows, Page Up / Down, Home / End move
    /// (a letter: the next option starting with it), Enter / Space pick, Esc
    /// and Tab close it (Esc does not close the window then).
    pub(super) fn menu_key(&mut self, vk: u16, repeat: bool) -> bool {
        const TAB: u16 = 0x09;
        const ENTER: u16 = 0x0D;
        const ESC: u16 = 0x1B;
        const SPACE: u16 = 0x20;
        let Some((ri, hi)) = self.menu else { return false };
        let labels: Vec<&str> = match self.row_ctl(ri).and_then(|c| c.options()) {
            Some((opts, ..)) => opts.iter().map(|o| model::opt_text(o)).collect(),
            None => {
                self.menu = None;
                return false;
            }
        };
        match vk {
            ENTER | SPACE if repeat => return true,
            ENTER | SPACE => {
                self.pick_menu(ri, hi);
                return true;
            }
            ESC | TAB => {
                self.menu = None;
                self.focus = Some(Hit::Menu(ri));
            }
            _ => {
                let Some(ni) = list_move(vk, hi, &labels, self.list_shown.get()) else { return false };
                self.menu = Some((ri, ni));
                self.menu_first = Self::keep_in_view(ni, self.menu_first, self.list_shown.get());
            }
        }
        self.redraw();
        true
    }
}

/// A list never opens over less than this many of its rows below its
/// button when it can show them there (it scrolls instead).
pub(super) const LIST_MIN_SHOWN: usize = 4;

/// Pure: where a dropdown list of `n` rows opens for a button `anchor` in
/// a window `win_h` tall: (its top, the rows it shows). Under the button
/// (4 below it, 8 from the window's bottom), all of it or as many rows as
/// fit there, scrolling the rest, as long as LIST_MIN_SHOWN of them fit
/// (or all, when there are fewer): what is under the button is the
/// list's own room, the rows above it stay seen (final audit 6: the list
/// opened up over the switch above). Only where that is not room enough
/// it opens above the button, the same way.
pub(super) fn list_place(anchor: D2D_RECT_F, rows: usize, win_h: f32) -> (f32, usize) {
    let n = rows.max(1);
    let below = win_h - 8.0 - (anchor.bottom + 4.0);
    let above = anchor.top - 4.0 - 8.0;
    let fits = |room: f32| (((room - LIST_PAD * 2.0) / ITEM_H).floor().max(0.0) as usize).min(n);
    let (b, a) = (fits(below), fits(above));
    let down = b >= LIST_MIN_SHOWN.min(n) || b >= a;
    let shown = if down { b } else { a }.max(1).min(rows);
    let h = shown as f32 * ITEM_H + LIST_PAD * 2.0;
    (if down { anchor.bottom + 4.0 } else { anchor.top - 4.0 - h }, shown)
}

/// Where a key moves the highlight in a list of `labels` (`shown` fit on
/// screen): None if the key does not move it.
pub(super) fn list_move(vk: u16, hi: usize, labels: &[&str], shown: usize) -> Option<usize> {
    const PGUP: u16 = 0x21;
    const PGDN: u16 = 0x22;
    const END: u16 = 0x23;
    const HOME: u16 = 0x24;
    const UP: u16 = 0x26;
    const DOWN: u16 = 0x28;
    let n = labels.len();
    if n == 0 {
        return None;
    }
    let page = shown.max(2) - 1;
    Some(match vk {
        UP => hi.saturating_sub(1),
        DOWN => (hi + 1).min(n - 1),
        PGUP => hi.saturating_sub(page),
        PGDN => (hi + page).min(n - 1),
        HOME => 0,
        END => n - 1,
        // A letter or digit: the next option starting with it.
        0x30..=0x39 | 0x41..=0x5A => {
            let c = (vk as u8 as char).to_ascii_lowercase();
            (1..=n).map(|k| (hi + k) % n).find(|&i| labels[i].chars().next().is_some_and(|f| f.to_lowercase().next() == Some(c)))?
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Final audit 6: '왼쪽 위' (hot corners) opened its list up over the
    /// switch above it, because all of it did not fit under it. It opens
    /// under its button and scrolls while LIST_MIN_SHOWN rows fit there;
    /// above only where under it is too tight, never past the window.
    #[test]
    fn a_list_opens_under_its_button_and_scrolls() {
        let win = 600.0;
        let at = |top: f32| rect(400.0, top, 560.0, top + 32.0);
        // Room for all: under it, all of it.
        let (top, shown) = list_place(at(100.0), 9, win);
        assert_eq!((top, shown), (136.0, 9));
        // Room for five of nine under it (all nine would fit above): still
        // under it, five shown, the rest scrolled to.
        let b = at(win - 8.0 - 4.0 - 32.0 - (5.0 * ITEM_H + LIST_PAD * 2.0));
        let (top, shown) = list_place(b, 9, win);
        assert!(top > b.bottom && shown == 5, "{top} {shown}");
        // Room for two only: above it, as many as fit there.
        let low = at(win - 8.0 - 4.0 - 32.0 - (2.0 * ITEM_H + LIST_PAD * 2.0));
        let (top, shown) = list_place(low, 9, win);
        assert!(top + shown as f32 * ITEM_H + LIST_PAD * 2.0 <= low.top - 4.0 + 1e-3 && shown == 9.min(((low.top - 12.0 - LIST_PAD * 2.0) / ITEM_H) as usize), "{top} {shown}");
        // A short list that fits under it whole stays there.
        let (top, shown) = list_place(low, 2, win);
        assert_eq!((top, shown), (low.bottom + 4.0, 2));
        // Never past the window, either way.
        for y in [0.0, 50.0, 200.0, 400.0, 520.0] {
            let (top, shown) = list_place(at(y), 12, win);
            assert!(top >= 8.0 - 1e-3 && top + shown as f32 * ITEM_H + LIST_PAD * 2.0 <= win - 8.0 + 1e-3, "{y}: {top} {shown}");
        }
        assert_eq!(list_place(at(100.0), 0, win).1, 0);
    }

    #[test]
    fn list_keys_move_the_highlight() {
        let l = ["Deutsch", "English", "Español", "Français", "한국어"];
        assert_eq!(super::list_move(0x28, 0, &l, 3), Some(1));
        assert_eq!(super::list_move(0x26, 0, &l, 3), Some(0));
        assert_eq!(super::list_move(0x22, 0, &l, 3), Some(2));
        assert_eq!(super::list_move(0x23, 1, &l, 3), Some(4));
        assert_eq!(super::list_move(0x45, 1, &l, 3), Some(2), "E: the next one with E");
        assert_eq!(super::list_move(0x45, 2, &l, 3), Some(1), "and round again");
        assert_eq!(super::list_move(0x5A, 0, &l, 3), None);
        assert_eq!(super::App::keep_in_view(7, 0, 5), 3);
        assert_eq!(super::App::keep_in_view(1, 3, 5), 1);
        assert_eq!(super::App::keep_in_view(4, 3, 5), 3);
    }
}
